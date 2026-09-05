//! One protocol, three faces — and a validator that enforces it.
//!
//! [`Decoder`], [`Encoder`] and [`BitstreamFilter`] are the same state machine
//! over three pairs of types. [`SendReceive`] is that machine stated once;
//! [`AsDecoder`], [`AsEncoder`] and [`AsBitstreamFilter`] are zero-cost
//! adapters that give it the trait face a caller expects.
//!
//! The point of the generic form is not economy of code. It is that
//! [`Validated`] — which turns a protocol violation into a loud, localised test
//! failure instead of a lost frame three crates downstream — only has to exist
//! once.

use vaco_core::{Error, Result};
use vaco_frame::Frame;
use vaco_packet::Packet;

use crate::machine::Stage;
use crate::{BitstreamFilter, Caps, Decoder, Encoder, EncoderPass, VideoParameters};

/// The send/receive protocol, independent of what is flowing through it.
///
/// Implementing this instead of [`Decoder`] directly buys the [`Validated`]
/// wrapper and the adapters below. Implementing [`Decoder`] directly is also
/// fine — [`DecoderProtocol`] adapts in the other direction.
pub trait SendReceive {
    /// Configure encoder multipass state through the protocol adapters.
    ///
    /// # Errors
    /// Multipass is unsupported unless the component implements it.
    fn set_pass(&mut self, pass: EncoderPass) -> Result<()> {
        match pass {
            EncoderPass::Single => Ok(()),
            _ => Err(Error::Unsupported(
                "this component does not support two-pass encoding",
            )),
        }
    }

    /// Return completed first-pass statistics through the protocol adapters.
    ///
    /// # Errors
    /// Propagates a component's statistics retrieval failure.
    fn pass_stats(&self) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    /// What is fed in: `Packet` for a decoder, `Frame` for an encoder.
    type Input;
    /// What comes out: `Frame` for a decoder, `Packet` for an encoder.
    type Output;

    /// What this component declares about its own buffering. The validator
    /// checks the component against exactly this.
    fn caps(&self) -> Caps;

    /// Feed one input, or `None` to begin draining.
    ///
    /// # Errors
    ///
    /// [`Error::OutputPending`] for backpressure — drain, then retry with the
    /// *same* input. [`Error::Eof`] when draining has already begun.
    fn send(&mut self, input: Option<&Self::Input>) -> Result<()>;

    /// Take the next output.
    ///
    /// # Errors
    ///
    /// [`Error::NeedMoreInput`] while feeding, [`Error::Eof`] once drained.
    fn receive(&mut self) -> Result<Self::Output>;

    /// Discard buffered state; return to [`Stage::Feeding`].
    fn flush(&mut self);

    /// Forwarded to [`Encoder::accepted_pix_fmts`] by [`AsEncoder`]; meaningless
    /// for a decoder or bitstream filter's `SendReceive`, so the empty default
    /// costs those implementors nothing.
    #[must_use]
    fn accepted_pix_fmts(&self) -> &'static [vaco_pixfmt::PixFmt] {
        &[]
    }

    /// Forwarded to [`Encoder::accepted_sample_fmts`] by [`AsEncoder`]; the
    /// audio mirror of [`accepted_pix_fmts`](Self::accepted_pix_fmts), same
    /// default, same reasoning.
    #[must_use]
    fn accepted_sample_fmts(&self) -> &'static [vaco_sampfmt::SampleFmt] {
        &[]
    }

    /// Forwarded to [`Decoder::set_extradata`] by [`AsDecoder`]; meaningless
    /// for an encoder or bitstream filter's `SendReceive`, so the empty
    /// default costs those implementors nothing.
    ///
    /// # Errors
    /// See [`Decoder::set_extradata`].
    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        let _ = extradata;
        Ok(())
    }

    /// Forwarded to [`Decoder::prime_video`] by [`AsDecoder`]; meaningless for
    /// an encoder or bitstream filter's `SendReceive`, so the empty default
    /// costs those implementors nothing.
    fn prime_video(&mut self, width: u32, height: u32) {
        let _ = (width, height);
    }

    /// Forwarded to [`Decoder::prime_video_params`].
    fn prime_video_params(&mut self, params: &VideoParameters) {
        let (width, height) = params.coded_dimensions();
        self.prime_video(width, height);
    }

    /// Forwarded to [`Decoder::prime_audio`] by [`AsDecoder`]; meaningless for
    /// an encoder or bitstream filter's `SendReceive`, so the empty default
    /// costs those implementors nothing.
    fn prime_audio(&mut self, sample_rate: u32, layout: vaco_chlayout::ChannelLayout) {
        let _ = (sample_rate, layout);
    }

    /// Forwarded to [`Encoder::set_option`] by [`AsEncoder`]; meaningless for
    /// a decoder or bitstream filter's `SendReceive`, so the default costs
    /// those implementors nothing.
    ///
    /// The default mirrors [`Encoder::set_option`]'s own default exactly —
    /// reject nothing, change nothing — rather than introducing a second
    /// "no options at all" signal that [`AsEncoder`] would have to translate
    /// back into that same contract. Before this existed, `AsEncoder<T>`
    /// could not forward `set_option` at all (there was nothing on
    /// `SendReceive` to forward *from*), which made every encoder built
    /// through it — fifteen codec crates as of this writing — unreachable
    /// from the CLI's option surface regardless of what the inner type
    /// wanted to do with an option. A codec with real options to expose
    /// overrides this the same way it would have overridden
    /// [`Encoder::set_option`] directly: handle the keys it recognises,
    /// return [`Error::Option`] for one it recognises but cannot parse, and
    /// fall through to `Ok(())` (or call this default) for anything else.
    ///
    /// # Errors
    /// See [`Encoder::set_option`].
    fn set_option(&mut self, key: &str, value: &str) -> Result<()> {
        let _ = (key, value);
        Ok(())
    }
}

// ---------------------------------------------------------------- adapters

/// Presents a [`SendReceive`] over packets and frames as a [`Decoder`].
#[derive(Debug, Clone)]
pub struct AsDecoder<T>(pub T);

impl<T> Decoder for AsDecoder<T>
where
    T: SendReceive<Input = Packet, Output = Frame> + Send,
{
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        self.0.send(packet)
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        self.0.receive()
    }

    fn flush(&mut self) {
        self.0.flush();
    }

    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        self.0.set_extradata(extradata)
    }

    fn prime_video(&mut self, width: u32, height: u32) {
        self.0.prime_video(width, height);
    }

    fn prime_video_params(&mut self, params: &VideoParameters) {
        self.0.prime_video_params(params);
    }

    fn prime_audio(&mut self, sample_rate: u32, layout: vaco_chlayout::ChannelLayout) {
        self.0.prime_audio(sample_rate, layout);
    }
}

/// Presents a [`SendReceive`] over frames and packets as an [`Encoder`].
#[derive(Debug, Clone)]
pub struct AsEncoder<T>(pub T);

impl<T> Encoder for AsEncoder<T>
where
    T: SendReceive<Input = Frame, Output = Packet> + Send,
{
    fn set_pass(&mut self, pass: EncoderPass) -> Result<()> {
        self.0.set_pass(pass)
    }

    fn pass_stats(&self) -> Result<Option<Vec<u8>>> {
        self.0.pass_stats()
    }

    fn send_frame(&mut self, frame: Option<&Frame>) -> Result<()> {
        self.0.send(frame)
    }

    fn receive_packet(&mut self) -> Result<Packet> {
        self.0.receive()
    }

    fn flush(&mut self) {
        self.0.flush();
    }

    fn accepted_pix_fmts(&self) -> &'static [vaco_pixfmt::PixFmt] {
        self.0.accepted_pix_fmts()
    }

    fn accepted_sample_fmts(&self) -> &'static [vaco_sampfmt::SampleFmt] {
        self.0.accepted_sample_fmts()
    }

    fn set_option(&mut self, key: &str, value: &str) -> Result<()> {
        self.0.set_option(key, value)
    }
}

/// Presents a packets-to-packets [`SendReceive`] as a [`BitstreamFilter`].
#[derive(Debug, Clone)]
pub struct AsBitstreamFilter<T>(pub T);

impl<T> BitstreamFilter for AsBitstreamFilter<T>
where
    T: SendReceive<Input = Packet, Output = Packet> + Send,
{
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        self.0.send(packet)
    }

    fn receive_packet(&mut self) -> Result<Packet> {
        self.0.receive()
    }
}

/// Adapts a hand-written [`Decoder`] to [`SendReceive`], so it can be wrapped
/// in [`Validated`].
///
/// Capabilities are supplied separately because [`Decoder`] does not carry
/// them; pass the same [`Caps`] the component's [`crate::DecoderDesc`] declares.
#[derive(Debug, Clone)]
pub struct DecoderProtocol<D> {
    inner: D,
    caps: Caps,
}

impl<D> DecoderProtocol<D> {
    /// Wrap `inner`, which claims `caps`.
    pub const fn new(inner: D, caps: Caps) -> Self {
        Self { inner, caps }
    }

    /// Recover the wrapped decoder.
    #[must_use]
    pub fn into_inner(self) -> D {
        self.inner
    }
}

impl<D: Decoder> SendReceive for DecoderProtocol<D> {
    type Input = Packet;
    type Output = Frame;

    fn caps(&self) -> Caps {
        self.caps
    }

    fn send(&mut self, input: Option<&Packet>) -> Result<()> {
        self.inner.send_packet(input)
    }

    fn receive(&mut self) -> Result<Frame> {
        self.inner.receive_frame()
    }

    fn flush(&mut self) {
        self.inner.flush();
    }

    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        self.inner.set_extradata(extradata)
    }

    fn prime_video(&mut self, width: u32, height: u32) {
        self.inner.prime_video(width, height);
    }

    fn prime_video_params(&mut self, params: &VideoParameters) {
        self.inner.prime_video_params(params);
    }

    fn prime_audio(&mut self, sample_rate: u32, layout: vaco_chlayout::ChannelLayout) {
        self.inner.prime_audio(sample_rate, layout);
    }
}

// --------------------------------------------------------------- validation

/// A way in which a component broke the send/receive contract.
///
/// Each variant names a rule from `docs/signal/vaco-codec-core.md`. None of
/// them is reachable by a caller doing something wrong — [`Validated`] filters
/// illegal *calls* out before they reach the component — so every one of these
/// is a bug in the component itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Violation {
    /// Input was accepted after end of stream was signalled. Rule S3.
    SendAfterEof,
    /// `send` reported `NeedMoreInput`, which is a receive-side answer. Rule S1.
    NeedMoreInputFromSend,
    /// `receive` reported `OutputPending`, which is a send-side answer. Rule R1.
    OutputPendingFromReceive,
    /// `send` claimed backpressure but the following `receive` had nothing to
    /// hand over. Backpressure that is not backed by output is a livelock.
    /// Rule S2.
    BackpressureWithoutOutput,
    /// `receive` asked for more input after end of stream. Rule R3.
    NeedMoreInputWhileDraining,
    /// `receive` reported end of stream before it was signalled. Rule R4.
    EofBeforeDrain,
    /// Output appeared after `receive` had already reported end of stream.
    /// Rule R5.
    OutputAfterEof,
    /// Output appeared before any input was accepted. Rule R6.
    OutputWithoutInput,
    /// Output appeared during a drain, from a component that does not declare
    /// [`Caps::DELAY`] and that had already reported having nothing left.
    /// Rule C1.
    DelayedOutputWithoutCap,
    /// More outputs than inputs, without [`Caps::SUBFRAMES`]. Rule C2.
    SubframesWithoutCap,
    /// After `flush`, the component still had output queued or still reported
    /// end of stream. Rule F1.
    FlushDidNotReset,
}

impl Violation {
    /// A one-line explanation, for the panic message and for test output.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::SendAfterEof => "input was accepted after send(None); it must return Err(Eof)",
            Self::NeedMoreInputFromSend => "send returned NeedMoreInput; only receive may",
            Self::OutputPendingFromReceive => "receive returned OutputPending; only send may",
            Self::BackpressureWithoutOutput => {
                "send returned OutputPending but no output was pending"
            }
            Self::NeedMoreInputWhileDraining => {
                "receive returned NeedMoreInput while draining; it must return output or Eof"
            }
            Self::EofBeforeDrain => "receive returned Eof before send(None) was called",
            Self::OutputAfterEof => "output was produced after receive returned Eof",
            Self::OutputWithoutInput => "output was produced before any input was accepted",
            Self::DelayedOutputWithoutCap => {
                "output was produced while draining without declaring Caps::DELAY"
            }
            Self::SubframesWithoutCap => {
                "more outputs than inputs without declaring Caps::SUBFRAMES"
            }
            Self::FlushDidNotReset => "flush() did not return the component to the feeding state",
        }
    }
}

/// What [`Validated`] does when it catches a [`Violation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OnViolation {
    /// Panic immediately, naming the rule. The default, because a codec bug
    /// that fails a test loudly costs an hour and one that corrupts output
    /// silently costs a week.
    #[default]
    Panic,
    /// Record it in [`Validated::violations`] and carry on. For a fuzz target,
    /// which wants to collect findings rather than abort on the first.
    Record,
}

/// Wraps a [`SendReceive`] and checks every call against the protocol.
///
/// It is a `SendReceive` itself, so it composes:
/// `AsDecoder(Validated::new(MyCodec::new()))` is a `Decoder` that fails a test
/// the moment `MyCodec` misbehaves.
///
/// The validator is deliberately *not* a debug-only construct. Wrapping in
/// tests, fuzz targets and conformance runs is the point; production builds
/// simply do not wrap.
#[derive(Debug)]
pub struct Validated<T: SendReceive> {
    inner: T,
    mode: OnViolation,
    flush_probe: bool,
    violations: Vec<Violation>,
    stage: Stage,
    /// Inputs accepted since the last flush, and outputs handed over. Without
    /// [`Caps::SUBFRAMES`] the second may never exceed the first — which is the
    /// same statement as "at most one output per input", but phrased in what a
    /// caller can actually see, so a queue that filled up under backpressure
    /// does not read as a violation.
    inputs: u64,
    outputs: u64,
    /// The component said it had nothing left, and no input has been accepted
    /// since. Output appearing after that, during a drain, is buffering — which
    /// is what [`Caps::DELAY`] declares.
    starved: bool,
    expect_output: bool,
}

impl<T: SendReceive> Validated<T> {
    /// Wrap `inner`, panicking on the first violation.
    #[must_use]
    pub fn new(inner: T) -> Self {
        Self::with_mode(inner, OnViolation::Panic)
    }

    /// Wrap `inner`, recording violations instead of panicking.
    #[must_use]
    pub fn recording(inner: T) -> Self {
        Self::with_mode(inner, OnViolation::Record)
    }

    /// Wrap `inner` with an explicit policy.
    #[must_use]
    pub fn with_mode(inner: T, mode: OnViolation) -> Self {
        Self {
            inner,
            mode,
            flush_probe: true,
            violations: Vec::new(),
            stage: Stage::Feeding,
            inputs: 0,
            outputs: 0,
            starved: false,
            expect_output: false,
        }
    }

    /// Whether `flush` should probe the component afterwards to confirm it
    /// reset (rule F1). On by default; turn it off for a component whose
    /// `receive` is expensive even when it has nothing to say.
    #[must_use]
    pub const fn with_flush_probe(mut self, probe: bool) -> Self {
        self.flush_probe = probe;
        self
    }

    /// Violations seen so far. Always empty in [`OnViolation::Panic`] mode,
    /// which never gets that far.
    #[must_use]
    pub fn violations(&self) -> &[Violation] {
        &self.violations
    }

    /// The protocol state the validator believes the component is in.
    #[must_use]
    pub const fn stage(&self) -> Stage {
        self.stage
    }

    /// Borrow the wrapped component.
    pub const fn inner(&self) -> &T {
        &self.inner
    }

    /// Recover the wrapped component.
    #[must_use]
    pub fn into_inner(self) -> T {
        self.inner
    }

    #[expect(
        clippy::panic,
        reason = "the point of the validator: a codec bug must fail a test loudly, \
                  and it is only ever wrapped around a component under test"
    )]
    fn report(&mut self, v: Violation) {
        match self.mode {
            OnViolation::Panic => panic!("codec protocol violation: {}", v.describe()),
            OnViolation::Record => self.violations.push(v),
        }
    }
}

impl<T: SendReceive> SendReceive for Validated<T> {
    fn set_pass(&mut self, pass: EncoderPass) -> Result<()> {
        self.inner.set_pass(pass)
    }

    fn pass_stats(&self) -> Result<Option<Vec<u8>>> {
        self.inner.pass_stats()
    }

    type Input = T::Input;
    type Output = T::Output;

    fn caps(&self) -> Caps {
        self.inner.caps()
    }

    fn accepted_pix_fmts(&self) -> &'static [vaco_pixfmt::PixFmt] {
        self.inner.accepted_pix_fmts()
    }

    fn accepted_sample_fmts(&self) -> &'static [vaco_sampfmt::SampleFmt] {
        self.inner.accepted_sample_fmts()
    }

    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        self.inner.set_extradata(extradata)
    }

    fn prime_video(&mut self, width: u32, height: u32) {
        self.inner.prime_video(width, height);
    }

    fn prime_video_params(&mut self, params: &VideoParameters) {
        self.inner.prime_video_params(params);
    }

    fn prime_audio(&mut self, sample_rate: u32, layout: vaco_chlayout::ChannelLayout) {
        self.inner.prime_audio(sample_rate, layout);
    }

    fn set_option(&mut self, key: &str, value: &str) -> Result<()> {
        self.inner.set_option(key, value)
    }

    fn send(&mut self, input: Option<&Self::Input>) -> Result<()> {
        let eof = input.is_none();
        let result = self.inner.send(input);
        if !self.stage.accepts_input() {
            // Illegal call by the caller, not by the component: the only legal
            // answer is Eof, so anything else is the component's fault.
            if !matches!(result, Err(Error::Eof)) {
                self.report(Violation::SendAfterEof);
            }
            return result;
        }
        match &result {
            Ok(()) => {
                if eof {
                    self.stage = Stage::Draining;
                } else {
                    self.inputs = self.inputs.saturating_add(1);
                    self.starved = false;
                }
                self.expect_output = false;
            }
            Err(Error::OutputPending) => self.expect_output = true,
            Err(Error::NeedMoreInput) => self.report(Violation::NeedMoreInputFromSend),
            Err(_) => {
                // A component that reports a decode error has still consumed
                // the input, so it counts towards the output budget.
                self.inputs = self.inputs.saturating_add(1);
                self.starved = false;
                self.expect_output = false;
            }
        }
        result
    }

    fn receive(&mut self) -> Result<Self::Output> {
        let result = self.inner.receive();
        match &result {
            Ok(_) => {
                if self.stage == Stage::Drained {
                    self.report(Violation::OutputAfterEof);
                }
                if self.inputs == 0 {
                    self.report(Violation::OutputWithoutInput);
                }
                if self.stage == Stage::Draining
                    && self.starved
                    && !self.caps().contains(Caps::DELAY)
                {
                    self.report(Violation::DelayedOutputWithoutCap);
                }
                self.outputs = self.outputs.saturating_add(1);
                if self.outputs > self.inputs && !self.caps().contains(Caps::SUBFRAMES) {
                    self.report(Violation::SubframesWithoutCap);
                }
                self.starved = false;
                self.expect_output = false;
            }
            Err(Error::NeedMoreInput) => {
                if self.expect_output {
                    self.report(Violation::BackpressureWithoutOutput);
                    self.expect_output = false;
                }
                if self.stage == Stage::Feeding {
                    // The component has nothing left for the inputs it has
                    // been given. Anything that appears after end of stream is
                    // therefore buffered, not merely queued.
                    self.starved = true;
                } else {
                    self.report(Violation::NeedMoreInputWhileDraining);
                }
            }
            Err(Error::Eof) => {
                if self.stage == Stage::Feeding {
                    self.report(Violation::EofBeforeDrain);
                } else {
                    self.stage = Stage::Drained;
                }
            }
            Err(Error::OutputPending) => self.report(Violation::OutputPendingFromReceive),
            Err(_) => {}
        }
        result
    }

    fn flush(&mut self) {
        self.inner.flush();
        self.stage = Stage::Feeding;
        self.inputs = 0;
        self.outputs = 0;
        self.starved = false;
        self.expect_output = false;
        if self.flush_probe && !matches!(self.inner.receive(), Err(Error::NeedMoreInput)) {
            self.report(Violation::FlushDidNotReset);
        }
    }
}

/// Wrap any [`Decoder`] so that a protocol violation fails loudly.
///
/// `caps` must be what the component's descriptor declares.
pub fn validate_decoder<D: Decoder>(
    decoder: D,
    caps: Caps,
) -> AsDecoder<Validated<DecoderProtocol<D>>> {
    AsDecoder(Validated::new(DecoderProtocol::new(decoder, caps)))
}

#[cfg(test)]
mod pass_tests {
    use super::*;

    #[derive(Default)]
    struct Statistics(Vec<u8>);
    impl SendReceive for Statistics {
        type Input = Frame;
        type Output = Packet;
        fn caps(&self) -> Caps {
            Caps::empty()
        }
        fn send(&mut self, _: Option<&Frame>) -> Result<()> {
            Ok(())
        }
        fn receive(&mut self) -> Result<Packet> {
            Err(Error::NeedMoreInput)
        }
        fn flush(&mut self) {}
        fn set_pass(&mut self, pass: EncoderPass) -> Result<()> {
            if let EncoderPass::Second(bytes) = pass {
                self.0 = bytes;
            }
            Ok(())
        }
        fn pass_stats(&self) -> Result<Option<Vec<u8>>> {
            Ok(Some(self.0.clone()))
        }
    }

    #[test]
    fn encoder_adapter_and_validator_preserve_opaque_statistics() -> Result<()> {
        let mut encoder = AsEncoder(Validated::new(Statistics::default()));
        encoder.set_pass(EncoderPass::Second(vec![0, 255, 13, 10]))?;
        assert_eq!(encoder.pass_stats()?, Some(vec![0, 255, 13, 10]));
        Ok(())
    }
}
