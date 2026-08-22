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
use vaco_codec_core::{Caps, Machine, OnViolation, SendReceive, Stage, Validated, Violation};

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
