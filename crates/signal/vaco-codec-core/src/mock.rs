//! A reference codec that exercises every corner of the send/receive protocol.
//!
//! No real codec exists yet, so this is how the protocol gets tested — and it
//! is deliberately written the way a real codec should be written, so that the
//! first author of a real one has something correct to copy.
//!
//! Three behaviours matter and all three are here:
//!
//! * **several outputs from one input** (`Step::Emit(n)`) — the `SUBFRAMES`
//!   case, which a naive `decode(packet) -> Frame` API cannot express;
//! * **a reorder delay** (`Step::Reorder`) — inputs that produce nothing for a
//!   while as a buffer fills, the `DELAY` case;
//! * **a drain at end of stream** — everything the reorder buffer still holds
//!   comes out after the `None` send.
//!
//! [`MockProgram::expected`] is an independent reference model of the same
//! behaviour, written without reference to [`Machine`]. Property tests drive
//! the codec through arbitrary legal call sequences and compare against it, so
//! "never loses or duplicates an output" is checked rather than asserted.

use std::collections::VecDeque;

use smallvec::SmallVec;
use vaco_core::{Duration, Error, Rational, Result, Timestamp};
use vaco_frame::{Frame, FrameData, FrameFlags};
use vaco_packet::Packet;
use vaco_pixfmt::PixFmt;

use crate::machine::{Accept, Machine};
use crate::{Caps, CodecParameters, Parser, SendReceive};

/// What the mock does with one input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Produce `n` outputs immediately. `n > 1` requires [`Caps::SUBFRAMES`].
    Emit(u32),
    /// Push this input into the reorder buffer, releasing the oldest once the
    /// buffer is over its delay. Requires [`Caps::DELAY`].
    Reorder,
    /// Produce nothing at all: a header-only packet.
    Skip,
    /// Fail with a recoverable error, having consumed the input.
    Corrupt,
}

/// One output of the mock codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MockUnit {
    /// The identifier of the input this came from.
    pub source: u64,
    /// Which of that input's outputs this is.
    pub sub: u32,
}

/// One input of the mock codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MockPacket {
    /// An identifier that flows through to every output derived from it.
    pub id: u64,
}

impl MockPacket {
    /// A packet with the given identifier.
    #[must_use]
    pub const fn new(id: u64) -> Self {
        Self { id }
    }
}

/// The behaviour a [`MockCodec`] follows, applied to inputs in order and
/// repeating once exhausted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockProgram {
    steps: Vec<Step>,
    reorder_delay: usize,
}

impl Default for MockProgram {
    /// One output per input, no delay: the simplest legal codec.
    fn default() -> Self {
        Self {
            steps: vec![Step::Emit(1)],
            reorder_delay: 0,
        }
    }
}

impl MockProgram {
    /// A program cycling through `steps`. Empty means [`Step::Emit(1)`].
    #[must_use]
    pub fn new(steps: Vec<Step>) -> Self {
        Self {
            steps: if steps.is_empty() {
                vec![Step::Emit(1)]
            } else {
                steps
            },
            reorder_delay: 0,
        }
    }

    /// How many inputs the reorder buffer holds back.
    #[must_use]
    pub const fn with_reorder_delay(mut self, delay: usize) -> Self {
        self.reorder_delay = delay;
        self
    }

    /// The reorder delay.
    #[must_use]
    pub const fn reorder_delay(&self) -> usize {
        self.reorder_delay
    }

    /// The step for the `n`th input since the last flush.
    #[must_use]
    pub fn step(&self, n: usize) -> Step {
        let len = self.steps.len().max(1);
        self.steps.get(n % len).copied().unwrap_or(Step::Emit(1))
    }

    /// The capabilities this program genuinely needs.
    ///
    /// Deriving them rather than letting the caller state them is what makes
    /// the mock a *correct* example: a codec must declare exactly what it does.
    #[must_use]
    pub fn caps(&self) -> Caps {
        let mut caps = Caps::empty();
        if self
            .steps
            .iter()
            .any(|s| matches!(s, Step::Emit(n) if *n > 1))
        {
            caps |= Caps::SUBFRAMES;
        }
        if self.steps.contains(&Step::Reorder) {
            caps |= Caps::DELAY;
        }
        caps
    }

    /// The reference model: what feeding `ids` and then draining must produce.
    ///
    /// Returns the outputs produced while feeding and the outputs produced by
    /// the drain, separately, because the protocol distinguishes them.
    #[must_use]
    pub fn expected(&self, ids: &[u64]) -> (Vec<MockUnit>, Vec<MockUnit>) {
        let mut fed = Vec::new();
        let mut queue: VecDeque<MockUnit> = VecDeque::new();
        for (n, &id) in ids.iter().enumerate() {
            match self.step(n) {
                Step::Emit(count) => {
                    for sub in 0..count {
                        fed.push(MockUnit { source: id, sub });
                    }
                }
                Step::Reorder => {
                    queue.push_back(MockUnit { source: id, sub: 0 });
                    if queue.len() > self.reorder_delay
                        && let Some(u) = queue.pop_front()
                    {
                        fed.push(u);
                    }
                }
                Step::Skip | Step::Corrupt => {}
            }
        }
        (fed, queue.into_iter().collect())
    }
}

/// A codec built the way a real one should be: a [`Machine`] for the protocol,
/// and its own state for the actual work.
#[derive(Debug)]
pub struct MockCodec {
    program: MockProgram,
    machine: Machine<MockUnit>,
    reorder: VecDeque<MockUnit>,
    inputs: usize,
}

impl MockCodec {
    /// A codec following `program`.
    #[must_use]
    pub fn new(program: MockProgram) -> Self {
        let caps = program.caps();
        Self {
            program,
            machine: Machine::new(caps),
            reorder: VecDeque::new(),
            inputs: 0,
        }
    }

    /// A codec with an explicit output queue depth, for exercising
    /// backpressure.
    #[must_use]
    pub fn with_capacity(program: MockProgram, capacity: usize) -> Self {
        let caps = program.caps();
        Self {
            program,
            machine: Machine::with_capacity(caps, capacity),
            reorder: VecDeque::new(),
            inputs: 0,
        }
    }

    /// The program being followed.
    #[must_use]
    pub const fn program(&self) -> &MockProgram {
        &self.program
    }

    /// The protocol state.
    #[must_use]
    pub const fn stage(&self) -> crate::Stage {
        self.machine.stage()
    }
}

impl SendReceive for MockCodec {
    type Input = MockPacket;
    type Output = MockUnit;

    fn caps(&self) -> Caps {
        self.machine.caps()
    }

    fn send(&mut self, input: Option<&MockPacket>) -> Result<()> {
        // Step one, always: let the machine validate the transition and apply
        // backpressure. Nothing below this line runs if it refuses.
        match self.machine.accept(input.is_none())? {
            Accept::Drain => {
                // Everything the reorder buffer still holds comes out now.
                while let Some(unit) = self.reorder.pop_front() {
                    self.machine.emit(unit);
                }
                self.machine.finish();
                Ok(())
            }
            Accept::Input => {
                let Some(pkt) = input else {
                    return Ok(());
                };
                let step = self.program.step(self.inputs);
                self.inputs += 1;
                match step {
                    Step::Emit(count) => {
                        for sub in 0..count {
                            self.machine.emit(MockUnit {
                                source: pkt.id,
                                sub,
                            });
                        }
                    }
                    Step::Reorder => {
                        self.reorder.push_back(MockUnit {
                            source: pkt.id,
                            sub: 0,
                        });
                        if self.reorder.len() > self.program.reorder_delay
                            && let Some(unit) = self.reorder.pop_front()
                        {
                            self.machine.emit(unit);
                        }
                    }
                    Step::Skip => {}
                    Step::Corrupt => {
                        return Err(Error::InvalidData("mock codec: corrupt input"));
                    }
                }
                Ok(())
            }
        }
    }

    fn receive(&mut self) -> Result<MockUnit> {
        self.machine.receive()
    }

    fn flush(&mut self) {
        self.machine.flush();
        self.reorder.clear();
        self.inputs = 0;
    }
}

/// The mock as a real [`Decoder`](crate::Decoder), over [`Packet`] and
/// [`Frame`].
///
/// Wrap it: `AsDecoder(MockDecoder::new(program))`, or
/// `AsDecoder(Validated::new(MockDecoder::new(program)))` to have protocol
/// violations fail the test.
///
/// A packet's identity is taken from its presentation timestamp and travels to
/// every frame derived from it, which is what makes "no output was lost or
/// duplicated" checkable end to end.
#[derive(Debug)]
pub struct MockDecoder {
    inner: MockCodec,
}

impl MockDecoder {
    /// A decoder following `program`.
    #[must_use]
    pub fn new(program: MockProgram) -> Self {
        Self {
            inner: MockCodec::new(program),
        }
    }

    /// The frame a [`MockUnit`] stands for: 16×16, no planes allocated, with
    /// the source identity in the timestamp.
    #[must_use]
    pub fn frame_for(unit: MockUnit) -> Frame {
        Frame {
            data: FrameData::Video {
                format: PixFmt::Yuv420p,
                width: 16,
                height: 16,
                planes: SmallVec::default(),
            },
            pts: Timestamp::new(i64::try_from(unit.source).unwrap_or(i64::MAX)),
            duration: Duration::ZERO,
            time_base: Rational::ONE,
            color: vaco_color::ColorInfo::default(),
            sample_aspect_ratio: Rational::ONE,
            flags: if unit.sub == 0 {
                FrameFlags::KEY
            } else {
                FrameFlags::empty()
            },
            side_data: SmallVec::default(),
        }
    }
}

impl SendReceive for MockDecoder {
    type Input = Packet;
    type Output = Frame;

    fn caps(&self) -> Caps {
        self.inner.caps()
    }

    fn send(&mut self, input: Option<&Packet>) -> Result<()> {
        let mapped = input.map(|p| MockPacket::new(p.pts.ticks().unwrap_or(0) as u64));
        self.inner.send(mapped.as_ref())
    }

    fn receive(&mut self) -> Result<Frame> {
        self.inner.receive().map(Self::frame_for)
    }

    fn flush(&mut self) {
        self.inner.flush();
    }
}

/// A parser that consumes fixed-size units, for exercising
/// [`ParserDriver`](crate::ParserDriver).
///
/// It reports units by byte count only — it cannot build a [`Packet`], because
/// `vaco-pool::Buffer` has no public constructor yet — so it exists to test the
/// harness's byte accounting, reassembly, stall detection and end-of-stream
/// handling rather than the packets themselves.
#[derive(Debug)]
pub struct MockParser {
    unit_len: usize,
    /// When set, the parser lies about how much it consumed. The harness must
    /// catch it.
    over_consume: bool,
    /// When set, the parser never consumes anything. The harness must not hang.
    stall: bool,
    units: u64,
    params: Option<CodecParameters>,
    declares_whole_sample_only: bool,
}

impl MockParser {
    /// A parser that consumes `unit_len` bytes at a time.
    #[must_use]
    pub const fn new(unit_len: usize) -> Self {
        Self {
            unit_len: if unit_len == 0 { 1 } else { unit_len },
            over_consume: false,
            stall: false,
            units: 0,
            params: None,
            declares_whole_sample_only: false,
        }
    }

    /// Make the parser claim to consume more than it was given.
    #[must_use]
    pub const fn over_consuming(mut self) -> Self {
        self.over_consume = true;
        self
    }

    /// Make the parser never consume anything.
    #[must_use]
    pub const fn stalling(mut self) -> Self {
        self.stall = true;
        self
    }

    /// Make [`Parser::whole_sample_only`] answer `true` — for exercising
    /// [`ParserDriver::push`](crate::ParserDriver::push)'s bypass path
    /// without a real `vaco-parse-ffv1`/`vaco-parse-vpx`/`vaco-parse-prores`
    /// sample on hand.
    #[must_use]
    pub const fn whole_sample_only(mut self) -> Self {
        self.declares_whole_sample_only = true;
        self
    }

    /// Units it has seen go past.
    #[must_use]
    pub const fn units(&self) -> u64 {
        self.units
    }

    /// Publish stream parameters, as a real parser does once it has seen a
    /// header.
    #[must_use]
    pub fn with_parameters(mut self, params: CodecParameters) -> Self {
        self.params = Some(params);
        self
    }
}

impl Parser for MockParser {
    fn parse(&mut self, input: &[u8]) -> Result<(Option<Packet>, usize)> {
        if self.over_consume {
            return Ok((None, input.len() + 1));
        }
        if self.stall || input.len() < self.unit_len {
            return Ok((None, 0));
        }
        self.units = self.units.saturating_add(1);
        // A `whole_sample_only` mock consumes the *whole* input regardless of
        // `unit_len` — the same "one call, one already-framed sample" shape
        // `Ffv1Parser`/`Vp8Parser`/`Vp9Parser`/`ProresParser` all report,
        // which is what `ParserDriver::push`'s bypass path is for.
        let used = if self.declares_whole_sample_only {
            input.len()
        } else {
            self.unit_len
        };
        Ok((None, used))
    }

    fn parameters(&self) -> Option<&CodecParameters> {
        self.params.as_ref()
    }

    fn whole_sample_only(&self) -> bool {
        self.declares_whole_sample_only
    }
}
