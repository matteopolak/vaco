//! The send/receive protocol, exercised through the mock codec.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division
)]

use std::collections::VecDeque;
use vaco_codec_core::mock::{MockCodec, MockPacket, MockProgram, MockUnit, Step};
use vaco_codec_core::{
    AsDecoder, AsEncoder, Caps, Decoder, Encoder, Machine, OnViolation, SendReceive, Stage,
    Validated, Violation,
};

use vaco_core::Error;

/// Feed every id, then drain, taking outputs whenever the codec offers them.
/// Returns (outputs seen while feeding, outputs seen while draining).
fn run<T: SendReceive<Input = MockPacket, Output = MockUnit>>(
    codec: &mut T,
    ids: &[u64],
) -> (Vec<MockUnit>, Vec<MockUnit>) {
    let mut fed = Vec::new();
    for &id in ids {
        let pkt = MockPacket::new(id);
        loop {
            match codec.send(Some(&pkt)) {
                // A corrupt input is consumed, so both cases move on.
                Ok(()) | Err(Error::InvalidData(_)) => break,
                Err(Error::OutputPending) => match codec.receive() {
                    Ok(u) => fed.push(u),
                    Err(e) => panic!("backpressure with nothing to drain: {e}"),
                },
                Err(e) => panic!("unexpected send error: {e}"),
            }
        }
        loop {
            match codec.receive() {
                Ok(u) => fed.push(u),
                Err(Error::NeedMoreInput) => break,
                Err(e) => panic!("unexpected receive error: {e}"),
            }
        }
    }
    codec.send(None).unwrap();
    let mut drained = Vec::new();
    loop {
        match codec.receive() {
            Ok(u) => drained.push(u),
            Err(Error::Eof) => break,
            Err(e) => panic!("unexpected drain error: {e}"),
        }
    }
    (fed, drained)
}

#[test]
fn one_packet_many_frames() {
    let program = MockProgram::new(vec![Step::Emit(3)]);
    assert!(program.caps().contains(Caps::SUBFRAMES));
    let mut codec = Validated::new(MockCodec::new(program.clone()));
    let ids = [10, 20];
    let (fed, drained) = run(&mut codec, &ids);
    let (want_fed, want_drained) = program.expected(&ids);
    assert_eq!(fed, want_fed);
    assert_eq!(drained, want_drained);
    assert_eq!(fed.len(), 6);
}

#[test]
fn reorder_delay_holds_output_back_then_drains_it() {
    let program = MockProgram::new(vec![Step::Reorder]).with_reorder_delay(2);
    assert!(program.caps().contains(Caps::DELAY));
    let mut codec = Validated::new(MockCodec::new(program.clone()));
    let ids = [1, 2, 3, 4, 5];
    let (fed, drained) = run(&mut codec, &ids);
    // Two inputs are held back at all times, so the last two only appear at EOF.
    assert_eq!(
        fed.iter().map(|u| u.source).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(
        drained.iter().map(|u| u.source).collect::<Vec<_>>(),
        vec![4, 5]
    );
    let (want_fed, want_drained) = program.expected(&ids);
    assert_eq!(fed, want_fed);
    assert_eq!(drained, want_drained);
}

#[test]
fn header_only_packets_produce_nothing() {
    let program = MockProgram::new(vec![Step::Skip, Step::Emit(1)]);
    let mut codec = Validated::new(MockCodec::new(program.clone()));
    let ids = [7, 8, 9, 10];
    let (fed, drained) = run(&mut codec, &ids);
    assert_eq!(
        fed.iter().map(|u| u.source).collect::<Vec<_>>(),
        vec![8, 10]
    );
    assert!(drained.is_empty());
    assert_eq!(program.expected(&ids).0, fed);
}

#[test]
fn corrupt_input_is_recoverable() {
    let program = MockProgram::new(vec![Step::Corrupt, Step::Emit(1)]);
    let mut codec = Validated::new(MockCodec::new(program));
    let bad = MockPacket::new(1);
    assert!(matches!(codec.send(Some(&bad)), Err(Error::InvalidData(_))));
    let good = MockPacket::new(2);
    codec.send(Some(&good)).unwrap();
    assert_eq!(codec.receive().unwrap(), MockUnit { source: 2, sub: 0 });
}

#[test]
fn send_after_eof_is_eof() {
    let mut codec = Validated::new(MockCodec::new(MockProgram::default()));
    let pkt = MockPacket::new(1);
    codec.send(Some(&pkt)).unwrap();
    assert!(codec.receive().is_ok());
    codec.send(None).unwrap();
    assert!(matches!(codec.send(Some(&pkt)), Err(Error::Eof)));
    assert!(matches!(codec.send(None), Err(Error::Eof)));
    assert!(matches!(codec.receive(), Err(Error::Eof)));
    // Eof is stable, not a one-shot.
    assert!(matches!(codec.receive(), Err(Error::Eof)));
}

#[test]
fn receive_before_any_input_asks_for_input() {
    let mut codec = Validated::new(MockCodec::new(MockProgram::default()));
    assert!(matches!(codec.receive(), Err(Error::NeedMoreInput)));
}

#[test]
fn flush_returns_to_feeding_from_every_stage() {
    let program = MockProgram::new(vec![Step::Reorder]).with_reorder_delay(4);
    let mut codec = Validated::new(MockCodec::new(program));

    // ... from Feeding, with output buffered.
    for id in 0..3 {
        codec.send(Some(&MockPacket::new(id))).unwrap();
    }
    codec.flush();
    assert_eq!(codec.stage(), Stage::Feeding);
    assert!(matches!(codec.receive(), Err(Error::NeedMoreInput)));

    // ... from Draining.
    codec.send(Some(&MockPacket::new(9))).unwrap();
    codec.send(None).unwrap();
    codec.flush();
    assert_eq!(codec.stage(), Stage::Feeding);
    codec.send(Some(&MockPacket::new(11))).unwrap();

    // ... from Drained.
    codec.send(None).unwrap();
    while codec.receive().is_ok() {}
    codec.flush();
    assert_eq!(codec.stage(), Stage::Feeding);
    codec.send(Some(&MockPacket::new(12))).unwrap();
}

#[test]
fn flush_discards_buffered_output() {
    let program = MockProgram::new(vec![Step::Reorder]).with_reorder_delay(8);
    let mut codec = Validated::new(MockCodec::new(program));
    for id in 0..5 {
        codec.send(Some(&MockPacket::new(id))).unwrap();
    }
    codec.flush();
    codec.send(None).unwrap();
    assert!(matches!(codec.receive(), Err(Error::Eof)));
}

#[test]
fn backpressure_is_always_backed_by_output() {
    let program = MockProgram::new(vec![Step::Emit(1)]);
    let mut codec = Validated::new(MockCodec::with_capacity(program, 2));
    let mut sent = 0u64;
    let mut hit_backpressure = false;
    for id in 0..8 {
        match codec.send(Some(&MockPacket::new(id))) {
            Ok(()) => sent += 1,
            Err(Error::OutputPending) => {
                hit_backpressure = true;
                // The contract: there is something to take, and the *same*
                // input is still ours to retry.
                assert!(codec.receive().is_ok());
                codec.send(Some(&MockPacket::new(id))).unwrap();
                sent += 1;
            }
            Err(e) => panic!("unexpected: {e}"),
        }
    }
    assert!(hit_backpressure);
    assert_eq!(sent, 8);
}

// -------------------------------------------------------- violation detection

/// A codec that buffers but does not declare `Caps::DELAY`. It keeps its own
/// queue rather than a `Machine`, because a `Machine` would have caught this in
/// debug builds — the validator has to catch it at the trait boundary too.
#[derive(Debug, Default)]
struct SecretlyDelayed {
    held: Option<MockUnit>,
    out: VecDeque<MockUnit>,
    eof: bool,
    done: bool,
}

impl SendReceive for SecretlyDelayed {
    type Input = MockPacket;
    type Output = MockUnit;

    fn caps(&self) -> Caps {
        Caps::empty()
    }

    fn send(&mut self, input: Option<&MockPacket>) -> Result<(), Error> {
        if self.eof {
            return Err(Error::Eof);
        }
        if let Some(p) = input {
            if let Some(prev) = self.held.replace(MockUnit {
                source: p.id,
                sub: 0,
            }) {
                self.out.push_back(prev);
            }
        } else {
            self.eof = true;
            if let Some(prev) = self.held.take() {
                self.out.push_back(prev);
            }
        }
        Ok(())
    }

    fn receive(&mut self) -> Result<MockUnit, Error> {
        if let Some(u) = self.out.pop_front() {
            return Ok(u);
        }
        if self.eof {
            self.done = true;
            Err(Error::Eof)
        } else {
            Err(Error::NeedMoreInput)
        }
    }

    fn flush(&mut self) {
        self.held = None;
        self.out.clear();
        self.eof = false;
        self.done = false;
    }
}

#[test]
fn undeclared_buffering_is_caught() {
    let mut codec = Validated::with_mode(SecretlyDelayed::default(), OnViolation::Record);
    codec.send(Some(&MockPacket::new(1))).unwrap();
    assert!(matches!(codec.receive(), Err(Error::NeedMoreInput)));
    codec.send(None).unwrap();
    assert!(codec.receive().is_ok());
    assert!(matches!(codec.receive(), Err(Error::Eof)));
    assert!(
        codec
            .violations()
            .contains(&Violation::DelayedOutputWithoutCap),
        "expected a DELAY violation, got {:?}",
        codec.violations()
    );
}

/// A codec that emits two outputs for one input without declaring `SUBFRAMES`.
#[derive(Debug, Default)]
struct SecretlyExpanding {
    out: VecDeque<MockUnit>,
    eof: bool,
}

impl SendReceive for SecretlyExpanding {
    type Input = MockPacket;
    type Output = MockUnit;

    fn caps(&self) -> Caps {
        Caps::empty()
    }

    fn send(&mut self, input: Option<&MockPacket>) -> Result<(), Error> {
        if self.eof {
            return Err(Error::Eof);
        }
        if let Some(p) = input {
            self.out.push_back(MockUnit {
                source: p.id,
                sub: 0,
            });
            self.out.push_back(MockUnit {
                source: p.id,
                sub: 1,
            });
        } else {
            self.eof = true;
        }
        Ok(())
    }

    fn receive(&mut self) -> Result<MockUnit, Error> {
        if let Some(u) = self.out.pop_front() {
            return Ok(u);
        }
        if self.eof {
            Err(Error::Eof)
        } else {
            Err(Error::NeedMoreInput)
        }
    }

    fn flush(&mut self) {
        self.out.clear();
        self.eof = false;
    }
}

#[test]
fn undeclared_subframes_is_caught() {
    let mut codec = Validated::with_mode(SecretlyExpanding::default(), OnViolation::Record);
    codec.send(Some(&MockPacket::new(1))).unwrap();
    assert!(codec.receive().is_ok());
    assert!(codec.receive().is_ok());
    assert!(
        codec.violations().contains(&Violation::SubframesWithoutCap),
        "expected a SUBFRAMES violation, got {:?}",
        codec.violations()
    );
}

#[test]
fn eof_before_drain_is_caught() {
    #[derive(Debug, Default)]
    struct EagerEof;
    impl SendReceive for EagerEof {
        type Input = MockPacket;
        type Output = MockUnit;
        fn caps(&self) -> Caps {
            Caps::empty()
        }
        fn send(&mut self, _input: Option<&MockPacket>) -> Result<(), Error> {
            Ok(())
        }
        fn receive(&mut self) -> Result<MockUnit, Error> {
            Err(Error::Eof)
        }
        fn flush(&mut self) {}
    }
    let mut codec = Validated::with_mode(EagerEof, OnViolation::Record).with_flush_probe(false);
    assert!(matches!(codec.receive(), Err(Error::Eof)));
    assert!(codec.violations().contains(&Violation::EofBeforeDrain));
}

#[test]
#[should_panic(expected = "codec protocol violation")]
fn panic_mode_fails_the_test_loudly() {
    #[derive(Debug, Default)]
    struct EagerEof;
    impl SendReceive for EagerEof {
        type Input = MockPacket;
        type Output = MockUnit;
        fn caps(&self) -> Caps {
            Caps::empty()
        }
        fn send(&mut self, _input: Option<&MockPacket>) -> Result<(), Error> {
            Ok(())
        }
        fn receive(&mut self) -> Result<MockUnit, Error> {
            Err(Error::Eof)
        }
        fn flush(&mut self) {}
    }
    let mut codec = Validated::new(EagerEof);
    let _ = codec.receive();
}

#[test]
fn machine_states_are_what_they_claim() {
    let mut m: Machine<u32> = Machine::with_capacity(Caps::DELAY, 2);
    assert_eq!(m.stage(), Stage::Feeding);
    assert!(matches!(m.receive(), Err(Error::NeedMoreInput)));
    m.accept(false).unwrap();
    m.emit(1);
    m.accept(false).unwrap();
    m.emit(2);
    assert!(m.is_full());
    assert!(matches!(m.accept(false), Err(Error::OutputPending)));
    assert_eq!(m.receive().unwrap(), 1);
    m.accept(false).unwrap();
    m.emit(3);
    m.accept(true).unwrap();
    assert_eq!(m.stage(), Stage::Draining);
    // Not finished yet: claiming Eof here would drop queued output.
    m.finish();
    assert_eq!(m.receive().unwrap(), 2);
    assert_eq!(m.receive().unwrap(), 3);
    assert!(matches!(m.receive(), Err(Error::Eof)));
    assert_eq!(m.stage(), Stage::Drained);
    assert!(matches!(m.accept(false), Err(Error::Eof)));
    m.flush();
    assert_eq!(m.stage(), Stage::Feeding);
}

#[test]
fn draining_without_finish_does_not_claim_eof() {
    let mut m: Machine<u32> = Machine::new(Caps::DELAY);
    m.accept(true).unwrap();
    // The component has not said it is done, so Eof would be a lie.
    assert!(matches!(m.receive(), Err(Error::NeedMoreInput)));
    m.finish();
    assert!(matches!(m.receive(), Err(Error::Eof)));
}

/// A `SendReceive` whose `set_extradata` is observable only by what it
/// returns, since nothing outside this file can downcast a `Box<dyn Decoder>`
/// back to its concrete type.
#[derive(Debug, Default)]
struct ExtradataProbe;

impl SendReceive for ExtradataProbe {
    type Input = vaco_packet::Packet;
    type Output = vaco_frame::Frame;

    fn caps(&self) -> Caps {
        Caps::empty()
    }

    fn send(&mut self, _input: Option<&vaco_packet::Packet>) -> Result<(), Error> {
        Err(Error::Eof)
    }

    fn receive(&mut self) -> Result<vaco_frame::Frame, Error> {
        Err(Error::Eof)
    }

    fn flush(&mut self) {}

    fn set_extradata(&mut self, extradata: &[u8]) -> Result<(), Error> {
        Err(Error::Option {
            name: "extradata-probe".to_owned(),
            detail: format!(
                "reached the inner SendReceive with {} bytes",
                extradata.len()
            ),
        })
    }
}

/// `set_extradata` has to survive every layer a registered decoder is built
/// through — `vaco-codec-subtitle-bitmap`'s three decoders are exactly
/// `Box::new(AsDecoder(Validated::new(inner)))`, which is what
/// `DecoderDesc::make` hands back as a `Box<dyn Decoder>`. `AsDecoder` and
/// `Validated` each carry their own explicit `impl Decoder`/`impl
/// SendReceive`, so a new trait method with a default body is silently
/// swallowed by any one of them that forgets to forward it instead of
/// reaching `ExtradataProbe` — the same shape gap 9 found one layer down in
/// `Box<dyn Muxer>`. `ExtradataProbe::set_extradata` always errs with a
/// distinctive message, so the only way this test can pass is if that exact
/// error surfaces through `AsDecoder`, `Validated` and the `Box<dyn Decoder>`
/// all three.
#[test]
fn set_extradata_forwards_through_as_decoder_validated_and_the_box() {
    let mut boxed: Box<dyn Decoder> = Box::new(AsDecoder(Validated::new(ExtradataProbe)));
    let err = boxed
        .set_extradata(&[1, 2, 3])
        .expect_err("must reach ExtradataProbe::set_extradata, not the trait default");
    match err {
        Error::Option { name, detail } => {
            assert_eq!(name, "extradata-probe");
            assert!(detail.contains("3 bytes"), "unexpected detail: {detail}");
        }
        other => panic!("expected Error::Option from ExtradataProbe, got {other:?}"),
    }
}

/// The default body alone must be harmless — empty, non-empty, and called
/// twice — for every codec whose container carries no configuration record,
/// mirroring `the_default_set_extradata_is_harmless` in `tests/parser.rs`.
#[test]
fn the_default_decoder_set_extradata_is_harmless() {
    #[derive(Debug, Default)]
    struct NoOpDecoder;
    impl Decoder for NoOpDecoder {
        fn send_packet(&mut self, _packet: Option<&vaco_packet::Packet>) -> Result<(), Error> {
            Err(Error::Eof)
        }
        fn receive_frame(&mut self) -> Result<vaco_frame::Frame, Error> {
            Err(Error::Eof)
        }
        fn flush(&mut self) {}
    }
    let mut d = NoOpDecoder;
    d.set_extradata(&[]).expect("empty is harmless");
    d.set_extradata(&[1, 2, 3]).expect("ignored is harmless");
    d.set_extradata(&[1, 2, 3]).expect("twice is harmless");
    let mut boxed: Box<dyn Decoder> = Box::new(NoOpDecoder);
    boxed.set_extradata(&[9]).expect("forwards through the box");
}

/// A call through a *generic* `D: Decoder` bound, instantiated with
/// `Box<dyn Decoder>` — the case the blanket `impl<D: Decoder + ?Sized>
/// Decoder for Box<D>` exists for.
///
/// This is deliberately a different failure mode from the two tests above.
/// Calling `.set_extradata()` directly on a `Box<dyn Decoder>` variable
/// dispatches through the trait object's own vtable and would still work
/// even with no `impl Decoder for Box<D>` at all — that path is already
/// covered above and does not exercise the blanket impl. Generic code
/// resolves the method through whatever `impl Decoder for Box<D>` the
/// compiler can find instead, so a blanket impl that inherited the trait's
/// default `set_extradata` (rather than forwarding it) would make exactly
/// this function silently return `Ok(())` without reaching `d`.
fn set_extradata_through_generic_decoder<D: Decoder>(
    d: &mut D,
    extradata: &[u8],
) -> Result<(), Error> {
    d.set_extradata(extradata)
}

#[test]
fn the_box_blanket_impl_forwards_through_a_generic_decoder_bound() {
    let mut boxed: Box<dyn Decoder> = Box::new(AsDecoder(Validated::new(ExtradataProbe)));
    let err = set_extradata_through_generic_decoder(&mut boxed, &[7, 7])
        .expect_err("must reach ExtradataProbe::set_extradata through the generic bound");
    match err {
        Error::Option { name, .. } => assert_eq!(name, "extradata-probe"),
        other => panic!("expected Error::Option from ExtradataProbe, got {other:?}"),
    }
}

/// A `SendReceive` whose `prime_video` is observable through a shared handle
/// rather than by inspecting the probe itself afterwards — once wrapped in
/// `AsDecoder(Validated(_))` and boxed as `Box<dyn Decoder>`, the concrete
/// type is erased and cannot be recovered without `Any`, so the probe hands
/// out an `Arc<Mutex<_>>` clone before it is moved into the wrapper stack.
/// `vaco-codec-ffv1`'s `Ffv1Decoder` is wired through exactly this
/// `AsDecoder(Validated(inner))` shape, and `Decoder::prime_video` is a
/// defaulted method just like `set_extradata`: a wrapper that inherits the
/// trait default instead of forwarding silently discards the container's
/// reported frame size, the same gap this crate's docs record for
/// `set_extradata`/`Box<dyn Muxer>`.
#[derive(Debug, Default)]
struct PrimeVideoProbe {
    seen: std::sync::Arc<std::sync::Mutex<Option<(u32, u32)>>>,
}

impl SendReceive for PrimeVideoProbe {
    type Input = vaco_packet::Packet;
    type Output = vaco_frame::Frame;

    fn caps(&self) -> Caps {
        Caps::empty()
    }

    fn send(&mut self, _input: Option<&vaco_packet::Packet>) -> Result<(), Error> {
        Err(Error::Eof)
    }

    fn receive(&mut self) -> Result<vaco_frame::Frame, Error> {
        Err(Error::Eof)
    }

    fn flush(&mut self) {}

    fn prime_video(&mut self, width: u32, height: u32) {
        *self.seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((width, height));
    }
}

/// `prime_video` has to survive the same `AsDecoder(Validated(inner))) ->
/// Box<dyn Decoder>` shape
/// [`set_extradata_forwards_through_as_decoder_validated_and_the_box`] checks
/// for `set_extradata` — see [`PrimeVideoProbe`]'s docs for why.
#[test]
fn prime_video_forwards_through_as_decoder_validated_and_the_box() {
    let probe = PrimeVideoProbe::default();
    let seen = probe.seen.clone();
    let mut boxed: Box<dyn Decoder> = Box::new(AsDecoder(Validated::new(probe)));
    boxed.prime_video(160, 120);
    assert_eq!(
        *seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
        Some((160, 120))
    );
}

/// The `Box<dyn Decoder>` blanket impl's own forward, mirroring
/// [`the_box_blanket_impl_forwards_through_a_generic_decoder_bound`] — a
/// generic `D: Decoder` bound instantiated with `Box<dyn Decoder>`, which
/// resolves through `impl<D: Decoder + ?Sized> Decoder for Box<D>` rather
/// than a direct vtable call on `&mut dyn Decoder`.
fn prime_video_through_generic_decoder<D: Decoder>(d: &mut D, width: u32, height: u32) {
    d.prime_video(width, height);
}

#[test]
fn the_box_blanket_impl_forwards_prime_video_through_a_generic_decoder_bound() {
    let probe = PrimeVideoProbe::default();
    let seen = probe.seen.clone();
    let mut boxed: Box<dyn Decoder> = Box::new(AsDecoder(Validated::new(probe)));
    prime_video_through_generic_decoder(&mut boxed, 176, 144);
    assert_eq!(
        *seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
        Some((176, 144))
    );
}

/// Mirrors [`ExtradataProbe`] on the encode side: an encoder-shaped
/// `SendReceive` that overrides `set_option` so a test can tell whether a
/// call reached it or was swallowed by a wrapper's inherited default.
struct OptionProbe;

impl SendReceive for OptionProbe {
    type Input = vaco_frame::Frame;
    type Output = vaco_packet::Packet;

    fn caps(&self) -> Caps {
        Caps::empty()
    }

    fn send(&mut self, _input: Option<&vaco_frame::Frame>) -> Result<(), Error> {
        Err(Error::Eof)
    }

    fn receive(&mut self) -> Result<vaco_packet::Packet, Error> {
        Err(Error::Eof)
    }

    fn flush(&mut self) {}

    fn set_option(&mut self, key: &str, value: &str) -> Result<(), Error> {
        Err(Error::Option {
            name: "option-probe".to_owned(),
            detail: format!("reached the inner SendReceive with {key}={value}"),
        })
    }

    fn accepted_sample_fmts(&self) -> &'static [vaco_sampfmt::SampleFmt] {
        &[vaco_sampfmt::SampleFmt::S16, vaco_sampfmt::SampleFmt::S32]
    }
}

/// The audio mirror of `set_option_forwards_through_as_encoder_and_validated`
/// (E2E-GAPS 3): `accepted_sample_fmts` is new on `SendReceive`, so it needs
/// the same check `accepted_pix_fmts` would have needed the day it was
/// added -- a wrapper that inherited the trait default instead of forwarding
/// would silently tell every caller "accepts anything", which is exactly the
/// state that let a mismatched sample format reach an encoder and a muxer
/// undetected before this gap was closed.
#[test]
fn accepted_sample_fmts_forwards_through_as_encoder_and_validated() {
    let enc = AsEncoder(Validated::new(OptionProbe));
    assert_eq!(
        enc.accepted_sample_fmts(),
        &[vaco_sampfmt::SampleFmt::S16, vaco_sampfmt::SampleFmt::S32]
    );
}

/// `set_option` has to survive both layers a registered encoder is commonly
/// built through: `Validated`, then `AsEncoder`. Before `SendReceive` grew
/// this method, `AsEncoder<T>` had nothing to forward it *from* at all, so
/// every encoder built this way -- fifteen codec crates as of this writing --
/// was unreachable from the CLI's option surface regardless of what the
/// wrapped codec actually did with it.
#[test]
fn set_option_forwards_through_as_encoder_and_validated() {
    let mut enc = AsEncoder(Validated::new(OptionProbe));
    let err = enc
        .set_option("b", "1000000")
        .expect_err("must reach OptionProbe::set_option, not the trait default");
    match err {
        Error::Option { name, detail } => {
            assert_eq!(name, "option-probe");
            assert!(detail.contains("b=1000000"), "unexpected detail: {detail}");
        }
        other => panic!("expected Error::Option from OptionProbe, got {other:?}"),
    }
}

/// The default body alone must be harmless for a codec with no options at
/// all -- an encoder that never overrides `SendReceive::set_option` (the
/// common case among the `AsEncoder`-based codecs) must still accept every
/// key silently, matching `Encoder::set_option`'s own documented default and
/// the reference's behaviour for an `AVOption` a codec ignores.
#[test]
fn the_default_as_encoder_set_option_is_harmless() {
    struct NoOptions;
    impl SendReceive for NoOptions {
        type Input = vaco_frame::Frame;
        type Output = vaco_packet::Packet;

        fn caps(&self) -> Caps {
            Caps::empty()
        }

        fn send(&mut self, _input: Option<&vaco_frame::Frame>) -> Result<(), Error> {
            Err(Error::Eof)
        }

        fn receive(&mut self) -> Result<vaco_packet::Packet, Error> {
            Err(Error::Eof)
        }

        fn flush(&mut self) {}
    }
    let mut enc = AsEncoder(NoOptions);
    enc.set_option("b", "1M").expect("ignored is harmless");
    enc.set_option("b", "1M").expect("twice is harmless");
    let mut boxed: Box<dyn Encoder> = Box::new(AsEncoder(NoOptions));
    boxed
        .set_option("qscale", "5")
        .expect("forwards through the box, still harmless");
}

/// A call through a *generic* `E: Encoder` bound, instantiated with
/// `Box<dyn Encoder>` -- mirrors
/// `the_box_blanket_impl_forwards_through_a_generic_decoder_bound` on the
/// decode side. `Box<dyn Encoder>` needs no blanket impl of its own (unlike
/// `Box<dyn Decoder>`, nothing in this crate wraps a boxed encoder
/// generically), but the call must still resolve to `AsEncoder`'s override
/// and not any inherited default.
fn set_option_through_generic_encoder<E: Encoder + ?Sized>(
    e: &mut E,
    key: &str,
    value: &str,
) -> Result<(), Error> {
    e.set_option(key, value)
}

#[test]
fn set_option_reaches_the_inner_codec_through_a_generic_encoder_bound() {
    let mut boxed: Box<dyn Encoder> = Box::new(AsEncoder(Validated::new(OptionProbe)));
    let err = set_option_through_generic_encoder(&mut *boxed, "b", "42")
        .expect_err("must reach OptionProbe::set_option through the generic bound");
    match err {
        Error::Option { name, .. } => assert_eq!(name, "option-probe"),
        other => panic!("expected Error::Option from OptionProbe, got {other:?}"),
    }
}
