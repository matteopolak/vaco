//! The five node kinds, and the three-phase split that lets one state machine
//! be driven by one thread or by several.
//!
//! # Plan, run, commit
//!
//! Advancing a node is deliberately not one function. It is:
//!
//! 1. **plan** — decide the node is runnable and *move* its inputs out of its
//!    input wires. Needs the wires; happens on the driver's thread.
//! 2. **run** — [`Job::run`]. Owns the component and owns its inputs, touches
//!    no wire and no other node, and returns owned outputs. This is where all
//!    the time goes: decode, filter, encode, and the muxer's writes.
//! 3. **commit** — push the outputs into the output wires and apply the
//!    end-of-stream closes. Needs the wires; happens on the driver's thread.
//!
//! Phase 2 is the only expensive one and it is a pure function of owned data,
//! so the parallel driver is *the same code* with several phase-2s running at
//! once between one plan and one commit. That is what makes threads a driver
//! choice rather than a scheduler rewrite — and it is why nothing in this crate
//! needs a lock: two jobs can never reach the same wire, because neither of
//! them can reach any wire at all.
//!
//! # End of stream
//!
//! The rule is uniform and it is the thing this file exists to get right: a
//! node that has seen end of stream on its inputs **drains itself first, emits
//! its buffered tail, and only then closes its outputs.** Closing early loses
//! exactly the codec's reorder delay from the end of every output, silently,
//! which is the classic bug in this shape of code.

use vaco_codec_core::{Decoder, Encoder, Stage};
use vaco_core::{Error, Rational, Result, Timestamp};
use vaco_filter_core::{Graph, GraphStatus, LinkFormat, NodeId};
use vaco_format_core::Demuxer;
use vaco_format_core::mux::MuxWriter;
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};
use vaco_pixfmt::PixFmt;
use vaco_sampfmt::SampleFmt;
use vaco_scale::{ImageSpec, ScaleOptions, Scaler};

use crate::wire::Payload;

/// Iterations a send/receive drain may take before the component is declared
/// broken.
///
/// A codec that never reports `Eof` while draining would otherwise spin here
/// forever. Well above any real reorder delay — 16 is a large DPB, 4096 is not
/// a number a correct codec can reach — and low enough that a broken one fails
/// in milliseconds with a diagnosis instead of hanging.
const MAX_PUMP: usize = 4096;

/// What one node needs to know about its ports to decide whether it can run.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PortIn {
    /// An item is waiting on this input port.
    pub has_item: bool,
    /// The producer has closed it and it is drained.
    pub at_eof: bool,
    /// The node has already been told this port ended.
    pub eof_delivered: bool,
}

/// Everything the readiness test reads.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Ports<'a> {
    pub inputs: &'a [PortIn],
    /// Per output port: every wire fanning out of it has room.
    pub out_room: &'a [bool],
    /// The caller has asked sources to stop reading.
    pub stop_reading: bool,
}

impl Ports<'_> {
    fn all_out_room(&self) -> bool {
        self.out_room.iter().all(|r| *r)
    }

    fn any_input(&self) -> bool {
        self.inputs.iter().any(|p| p.has_item)
    }

    fn any_new_eof(&self) -> bool {
        self.inputs.iter().any(|p| p.at_eof && !p.eof_delivered)
    }

    fn all_eof(&self) -> bool {
        self.inputs.iter().all(|p| p.at_eof)
    }
}

/// One unit of work: a node's component, detached from the pipeline, plus
/// everything it is allowed to see.
///
/// `Send`, because every field is: the component traits all require it, and the
/// inputs are owned packets and frames.
#[derive(Debug)]
pub(crate) struct Job {
    pub node: usize,
    pub work: Work,
    /// `(input port, item)`, in the order the planner took them.
    pub inputs: Vec<(usize, Payload)>,
    /// Input ports that have just reached end of stream, with the timestamp
    /// each ended at, in that port's time base.
    pub ended: Vec<(usize, Timestamp)>,
    /// Every input port is finished. What tells a muxer to write its trailer.
    pub all_ended: bool,
}

/// A finished [`Job`]: the component back, plus what it produced.
#[derive(Debug)]
pub(crate) struct Done {
    pub node: usize,
    pub work: Work,
    pub out: Vec<(usize, Payload)>,
    pub close: Vec<(usize, Timestamp)>,
    pub progressed: bool,
    pub error: Option<Error>,
}

impl Job {
    /// Run the node. Never touches a wire, never blocks, never panics.
    pub(crate) fn run(self) -> Done {
        let Self {
            node,
            mut work,
            inputs,
            ended,
            all_ended,
        } = self;
        let mut out = Vec::new();
        let mut close = Vec::new();
        let mut progressed = !inputs.is_empty();
        let result = work.advance(inputs, &ended, all_ended, &mut out, &mut close);
        let error = match result {
            Ok(did) => {
                progressed |= did;
                None
            }
            Err(e) => Some(e),
        };
        progressed |= !out.is_empty() || !close.is_empty();
        Done {
            node,
            work,
            out,
            close,
            progressed,
            error,
        }
    }
}

/// The component a node owns.
#[derive(Debug)]
pub(crate) enum Work {
    Demux(DemuxWork),
    Decode(CodecWork<DecoderSide>),
    Encode(CodecWork<EncoderSide>),
    Convert(Box<CodecWork<ConverterSide>>),
    ConvertAudio(Box<CodecWork<AudioConverterSide>>),
    Filter(Box<FilterWork>),
    Mux(Box<MuxWork>),
}

impl Work {
    /// Whether this node has finished for good.
    ///
    /// For a muxer that means the trailer is written, which is the only
    /// definition of "done" that distinguishes a finished output from a
    /// truncated one.
    pub(crate) fn is_done(&self) -> bool {
        match self {
            Self::Demux(d) => d.finished,
            Self::Decode(c) => c.stage == Stage::Drained,
            Self::Encode(c) => c.stage == Stage::Drained,
            Self::Convert(c) => c.stage == Stage::Drained,
            Self::ConvertAudio(c) => c.stage == Stage::Drained,
            Self::Filter(f) => f.sink_closed.iter().all(|c| *c),
            Self::Mux(m) => m.writer.is_none(),
        }
    }

    /// Whether this node can make progress right now.
    ///
    /// This is the whole of the backpressure policy: a node with nowhere to put
    /// its output is not runnable, so a producer that outruns its consumer
    /// simply stops being scheduled. Nothing blocks and nothing buffers.
    pub(crate) fn ready(&self, ports: Ports<'_>) -> bool {
        match self {
            Self::Demux(d) => !ports.stop_reading && !d.finished && ports.all_out_room(),
            Self::Decode(c) => c.ready(ports),
            Self::Encode(c) => c.ready(ports),
            Self::Convert(c) => c.ready(ports),
            Self::ConvertAudio(c) => c.ready(ports),
            Self::Filter(f) => f.ready(ports),
            Self::Mux(m) => m.ready(ports),
        }
    }

    /// Whether the node can accept another item at all this step.
    ///
    /// A filter holding a frame the graph refused takes nothing new, so the
    /// number of frames in limbo between a wire and a buffer source can never
    /// exceed one per input port.
    pub(crate) fn accepts_input(&self) -> bool {
        match self {
            Self::Filter(f) => f.stashed.is_empty(),
            _ => true,
        }
    }

    /// How many items this node should be given this step.
    ///
    /// One, for everything that expands its input: a decoder handed a batch
    /// could emit a batch's worth of reorder delay at once, and the wire's
    /// bound would mean nothing. A muxer may take a batch because it only ever
    /// consumes.
    pub(crate) const fn batch(&self) -> usize {
        match self {
            Self::Demux(_) => 0,
            Self::Decode(_) | Self::Encode(_) | Self::Convert(_) | Self::ConvertAudio(_) | Self::Filter(_) => 1,
            Self::Mux(_) => 16,
        }
    }

    fn advance(
        &mut self,
        inputs: Vec<(usize, Payload)>,
        ended: &[(usize, Timestamp)],
        all_ended: bool,
        out: &mut Vec<(usize, Payload)>,
        close: &mut Vec<(usize, Timestamp)>,
    ) -> Result<bool> {
        match self {
            Self::Demux(d) => d.advance(out, close),
            Self::Decode(c) => c.advance(inputs, ended, out, close),
            Self::Encode(c) => c.advance(inputs, ended, out, close),
            Self::Convert(c) => c.advance(inputs, ended, out, close),
            Self::ConvertAudio(c) => c.advance(inputs, ended, out, close),
            Self::Filter(f) => f.advance(inputs, ended, out, close),
            Self::Mux(m) => m.advance(inputs, ended, all_ended),
        }
    }
}

// ------------------------------------------------------------------- demux

/// Reads one packet per step and routes it to the output port its stream is
/// mapped to. Packets belonging to unmapped streams are dropped here, which is
/// what makes `-map` a selection and not a filter further downstream.
pub(crate) struct DemuxWork {
    pub demuxer: Box<dyn Demuxer>,
    /// Stream index to output port. `None` means the stream is not mapped
    /// anywhere and its packets are discarded.
    pub port_for_stream: Vec<Option<usize>>,
    /// The last timestamp seen on each output port, so end of stream carries a
    /// position rather than `NONE`.
    pub last_pts: Vec<Timestamp>,
    pub finished: bool,
    /// Recoverable `InvalidData` errors tolerated before the input is declared
    /// unusable. The reference's `-max_error_rate` in its simplest form.
    pub max_errors: u32,
    pub errors: u32,
}

impl std::fmt::Debug for DemuxWork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DemuxWork")
            .field("finished", &self.finished)
            .field("errors", &self.errors)
            .finish_non_exhaustive()
    }
}

impl DemuxWork {
    fn end(&mut self, close: &mut Vec<(usize, Timestamp)>) {
        self.finished = true;
        for (port, pts) in self.last_pts.iter().enumerate() {
            close.push((port, *pts));
        }
    }

    fn advance(
        &mut self,
        out: &mut Vec<(usize, Payload)>,
        close: &mut Vec<(usize, Timestamp)>,
    ) -> Result<bool> {
        if self.finished {
            return Ok(false);
        }
        match self.demuxer.read_packet() {
            Ok(packet) => {
                let idx = packet.stream_index as usize;
                let Some(Some(port)) = self.port_for_stream.get(idx).copied() else {
                    // Not mapped: dropped on the floor, but the read itself is
                    // progress, so the guard does not see a stall.
                    return Ok(true);
                };
                if let Some(slot) = self.last_pts.get_mut(port) {
                    let ts = if packet.pts.is_some() {
                        packet.pts
                    } else {
                        packet.dts
                    };
                    if ts.is_some() {
                        *slot = ts;
                    }
                }
                out.push((port, Payload::Packet(packet)));
                Ok(true)
            }
            Err(Error::Eof) => {
                self.end(close);
                Ok(true)
            }
            Err(e) if e.is_recoverable() => {
                self.errors = self.errors.saturating_add(1);
                if self.errors > self.max_errors {
                    Err(e)
                } else {
                    Ok(true)
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Stop reading but end cleanly: what `-t`, `-frames` and a graceful
    /// cancellation do. The rest of the pipeline drains and the trailer is
    /// still written, so the output is a valid file.
    pub(crate) fn stop(&mut self, close: &mut Vec<(usize, Timestamp)>) {
        if !self.finished {
            self.end(close);
        }
    }
}

// ------------------------------------------------------- decode and encode

/// A decoder and an encoder are the same state machine over swapped types, so
/// the drain logic — the part that is easy to get wrong — is written once.
pub(crate) trait Side: Send + std::fmt::Debug {
    fn send(&mut self, item: Option<&Payload>) -> Result<()>;
    fn recv(&mut self) -> Result<Payload>;
}

/// Packets in, frames out.
pub(crate) struct DecoderSide(pub Box<dyn Decoder>);

impl std::fmt::Debug for DecoderSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DecoderSide")
    }
}

impl Side for DecoderSide {
    fn send(&mut self, item: Option<&Payload>) -> Result<()> {
        match item {
            Some(Payload::Packet(p)) => self.0.send_packet(Some(p)),
            Some(Payload::Frame(_)) => Err(Error::InvalidData("a frame reached a decoder")),
            None => self.0.send_packet(None),
        }
    }

    fn recv(&mut self) -> Result<Payload> {
        self.0.receive_frame().map(Payload::Frame)
    }
}

/// Frames in, packets out.
pub(crate) struct EncoderSide(pub Box<dyn Encoder>);

impl std::fmt::Debug for EncoderSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EncoderSide")
    }
}

impl Side for EncoderSide {
    fn send(&mut self, item: Option<&Payload>) -> Result<()> {
        match item {
            Some(Payload::Frame(fr)) => self.0.send_frame(Some(fr)),
            Some(Payload::Packet(_)) => Err(Error::InvalidData("a packet reached an encoder")),
            None => self.0.send_frame(None),
        }
    }

    fn recv(&mut self) -> Result<Payload> {
        self.0.receive_packet().map(Payload::Packet)
    }
}

/// Frames in, frames out: a pixel-format bridge between a decoder and an
/// encoder that does not accept what the decoder produces.
///
/// One converter per instance covers one destination format; `vaco-scale`'s
/// own `Scaler` reconfigures itself when a frame's dimensions change, so this
/// only has to notice a *format* mismatch and rebuild the plan the first time
/// it sees one, or whenever the source geometry changes. A frame already in
/// `dst_format` is cloned through untouched — cheap, since a `Frame`'s planes
/// are reference-counted — rather than run through the scaler as a same-format
/// no-op.
pub(crate) struct ConverterSide {
    dst_format: PixFmt,
    limits: Limits,
    scaler: Option<Scaler>,
    budget: Budget,
    pending: Option<Frame>,
    eof: bool,
}

impl std::fmt::Debug for ConverterSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConverterSide")
            .field("dst_format", &self.dst_format)
            .finish_non_exhaustive()
    }
}

impl ConverterSide {
    pub(crate) fn new(dst_format: PixFmt, limits: Limits) -> Self {
        Self {
            dst_format,
            budget: Budget::new(limits.clone()),
            limits,
            scaler: None,
            pending: None,
            eof: false,
        }
    }

    fn convert(&mut self, src: &Frame) -> Result<Frame> {
        let FrameData::Video {
            format,
            width,
            height,
            ..
        } = src.data
        else {
            return Err(Error::InvalidData(
                "a format converter received a non-video frame",
            ));
        };
        if format == self.dst_format {
            return Ok(src.clone());
        }
        let src_spec = ImageSpec {
            format,
            width,
            height,
            color: src.color,
        };
        let dst_spec = ImageSpec {
            format: self.dst_format,
            width,
            height,
            color: src.color,
        };
        let scaler = if let Some(s) = &mut self.scaler {
            s
        } else {
            let s = Scaler::with_limits(
                &src_spec,
                &dst_spec,
                &ScaleOptions::default(),
                self.limits.clone(),
            )?;
            self.scaler.insert(s)
        };
        let mut dst = Frame::alloc_video(&mut self.budget, self.dst_format, width, height)?;
        scaler.scale_frame(src, &mut dst)?;
        dst.pts = src.pts;
        dst.duration = src.duration;
        dst.time_base = src.time_base;
        dst.color = src.color;
        dst.sample_aspect_ratio = src.sample_aspect_ratio;
        dst.flags = src.flags;
        Ok(dst)
    }
}

impl Side for ConverterSide {
    fn send(&mut self, item: Option<&Payload>) -> Result<()> {
        match item {
            Some(Payload::Frame(f)) => {
                self.pending = Some(self.convert(f)?);
                Ok(())
            }
            Some(Payload::Packet(_)) => Err(Error::InvalidData(
                "a packet reached a format converter",
            )),
            None => {
                self.eof = true;
                Ok(())
            }
        }
    }

    fn recv(&mut self) -> Result<Payload> {
        if let Some(f) = self.pending.take() {
            return Ok(Payload::Frame(f));
        }
        if self.eof {
            return Err(Error::Eof);
        }
        Err(Error::NeedMoreInput)
    }
}

/// Frames in, frames out: a sample-format bridge between a decoder and an
/// encoder that does not accept what the decoder produces.
///
/// The audio twin of [`ConverterSide`], for exactly the gap that one's doc
/// names for video: `vaco-codec-dsp-fmtconvert`/`vaco-resample` existed and
/// were tested, but nothing sat between a decoder's real output format and an
/// encoder that only accepts one specific format — so decoding AAC's planar
/// float into `pcm_s16le` (packed s16) either got refused by the encoder
/// (`"encoder input sample format does not match this codec"`) or, worse, was
/// accepted and mislabeled, and a downstream muxer refused the *container*
/// property instead (`"wav: planar sample formats are not supported"`) for a
/// stream whose bytes were never actually planar to begin with (E2E-GAPS 3).
///
/// Channel layout and sample rate are passed through unchanged — this only
/// ever changes *format* (packed vs. planar, and element width), the same way
/// [`ConverterSide`] only ever changes pixel format and never resolution.
/// Resampling or remixing is `vaco-resample::Resampler`'s job, reached
/// through `-ar`/`-ac`/a real filtergraph, not this ad-hoc node.
pub(crate) struct AudioConverterSide {
    dst_format: SampleFmt,
    budget: Budget,
    pending: Option<Frame>,
    eof: bool,
}

impl std::fmt::Debug for AudioConverterSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioConverterSide")
            .field("dst_format", &self.dst_format)
            .finish_non_exhaustive()
    }
}

impl AudioConverterSide {
    pub(crate) fn new(dst_format: SampleFmt, limits: Limits) -> Self {
        Self {
            dst_format,
            budget: Budget::new(limits),
            pending: None,
            eof: false,
        }
    }

    fn convert(&mut self, src: &Frame) -> Result<Frame> {
        let FrameData::Audio {
            format,
            sample_rate,
            samples,
            layout,
            planes: src_planes,
        } = &src.data
        else {
            return Err(Error::InvalidData(
                "a sample-format converter received a non-audio frame",
            ));
        };
        let (format, sample_rate, samples, layout) =
            (*format, *sample_rate, *samples, layout.clone());
        if format == self.dst_format {
            return Ok(src.clone());
        }
        let src_slices: Vec<&[u8]> = src_planes.iter().map(|p| p.data.as_slice()).collect();
        let src_ref = if format.is_planar() {
            vaco_resample::AudioRef::planar(format, &src_slices)?
        } else {
            let data = src_slices
                .first()
                .copied()
                .ok_or(Error::InvalidData("audio frame has no plane 0"))?;
            vaco_resample::AudioRef::packed(format, layout.channels, data)?
        };

        let mut dst = Frame::alloc_audio(
            &mut self.budget,
            self.dst_format,
            layout.clone(),
            samples,
            sample_rate,
        )?;
        let FrameData::Audio {
            planes: dst_planes, ..
        } = &mut dst.data
        else {
            return Err(Error::InvalidData(
                "a freshly allocated audio frame is not audio",
            ));
        };
        if self.dst_format.is_planar() {
            let mut dst_slices: Vec<&mut [u8]> =
                dst_planes.iter_mut().map(|p| p.data.make_mut()).collect();
            let mut dst_mut = vaco_resample::AudioMut::planar(self.dst_format, &mut dst_slices)?;
            vaco_resample::convert::convert(src_ref, &mut dst_mut)?;
        } else {
            let plane = dst_planes
                .first_mut()
                .ok_or(Error::InvalidData("allocated audio frame has no plane 0"))?;
            let mut dst_mut = vaco_resample::AudioMut::packed(
                self.dst_format,
                layout.channels,
                plane.data.make_mut(),
            )?;
            vaco_resample::convert::convert(src_ref, &mut dst_mut)?;
        }
        dst.pts = src.pts;
        dst.duration = src.duration;
        dst.time_base = src.time_base;
        dst.flags = src.flags;
        Ok(dst)
    }
}

impl Side for AudioConverterSide {
    fn send(&mut self, item: Option<&Payload>) -> Result<()> {
        match item {
            Some(Payload::Frame(f)) => {
                self.pending = Some(self.convert(f)?);
                Ok(())
            }
            Some(Payload::Packet(_)) => Err(Error::InvalidData(
                "a packet reached a sample-format converter",
            )),
            None => {
                self.eof = true;
                Ok(())
            }
        }
    }

    fn recv(&mut self) -> Result<Payload> {
        if let Some(f) = self.pending.take() {
            return Ok(Payload::Frame(f));
        }
        if self.eof {
            return Err(Error::Eof);
        }
        Err(Error::NeedMoreInput)
    }
}

/// One send/receive component, with the protocol's three stages made explicit.
#[derive(Debug)]
pub(crate) struct CodecWork<S: Side> {
    pub side: S,
    pub stage: Stage,
    /// The last output timestamp, so the close carries a position.
    pub last_pts: Timestamp,
    /// An input the component refused because it wanted to be drained first.
    /// Held here rather than pushed back onto a wire, so the wire stays a
    /// strict SPSC queue and the retry uses the *same* item, as the protocol
    /// requires.
    pub stashed: Option<Payload>,
    /// End of stream has been announced but not yet acted on, because there is
    /// still a stashed input in front of it.
    ///
    /// The planner delivers each port's end exactly once, so a node that
    /// returns early without acting on it must *remember* it. Forgetting here
    /// is how a pipeline hangs one packet short of the end.
    pub pending_eof: Option<Timestamp>,
    /// Where the input stream ended, used only when the component itself never
    /// produced a timestamp.
    pub end_pts: Timestamp,
}

impl<S: Side> CodecWork<S> {
    fn ready(&self, ports: Ports<'_>) -> bool {
        if !ports.all_out_room() {
            return false;
        }
        if self.stashed.is_some() {
            return true;
        }
        if self.pending_eof.is_some() {
            return true;
        }
        match self.stage {
            Stage::Feeding => ports.any_input() || ports.all_eof(),
            Stage::Draining => true,
            Stage::Drained => false,
        }
    }

    /// Take everything the component will hand over right now.
    fn drain(&mut self, out: &mut Vec<(usize, Payload)>) -> Result<bool> {
        for _ in 0..MAX_PUMP {
            match self.side.recv() {
                Ok(item) => {
                    if item.pts().is_some() {
                        self.last_pts = item.pts();
                    }
                    out.push((0, item));
                }
                Err(Error::NeedMoreInput) => return Ok(false),
                Err(Error::Eof) => return Ok(true),
                Err(e) => return Err(e),
            }
        }
        Err(Error::InvalidData(
            "a codec produced output without end, which is a protocol violation",
        ))
    }

    fn advance(
        &mut self,
        inputs: Vec<(usize, Payload)>,
        ended: &[(usize, Timestamp)],
        out: &mut Vec<(usize, Payload)>,
        close: &mut Vec<(usize, Timestamp)>,
    ) -> Result<bool> {
        let mut progressed = false;
        for (_, ts) in ended {
            let slot = self.pending_eof.get_or_insert(*ts);
            if slot.is_none() {
                *slot = *ts;
            }
        }
        if self.stage == Stage::Feeding {
            let mut queued: Vec<Payload> = Vec::new();
            if let Some(held) = self.stashed.take() {
                queued.push(held);
            }
            queued.extend(inputs.into_iter().map(|(_, item)| item));
            for item in queued {
                let mut accepted = false;
                for _ in 0..MAX_PUMP {
                    match self.side.send(Some(&item)) {
                        Ok(()) => {
                            accepted = true;
                            break;
                        }
                        // Backpressure inside the component: drain, then retry
                        // with the *same* item. Never drop it, never substitute
                        // the next one.
                        Err(Error::OutputPending) => {
                            self.drain(out)?;
                            progressed = true;
                        }
                        Err(e) => return Err(e),
                    }
                }
                if !accepted {
                    self.stashed = Some(item);
                    return Ok(progressed);
                }
                progressed = true;
                self.drain(out)?;
            }
            // Only once the component has taken everything we hold may we tell
            // it the stream is over. Draining first is what keeps its tail.
            if let Some(end) = self.pending_eof.take() {
                if self.end_pts.is_none() {
                    self.end_pts = end;
                }
                self.side.send(None)?;
                self.stage = Stage::Draining;
                progressed = true;
            }
        }
        if self.stage == Stage::Draining {
            let before = out.len();
            let finished = self.drain(out)?;
            if finished {
                self.stage = Stage::Drained;
                let end = if self.last_pts.is_some() {
                    self.last_pts
                } else {
                    self.end_pts
                };
                close.push((0, end));
            }
            // Real progress here is "produced at least one item" or "reached
            // Drained" — not merely "we asked and were told NeedMoreInput
            // again". `drain` returning `Ok(false)` with nothing pushed to
            // `out` means the component is still draining with nothing ready
            // *yet*, which is legitimate for one call (a codec's own reorder
            // delay can need several steps to empty) but is indistinguishable
            // from a broken component that announced draining and then never
            // produces `Eof` at all — `vaco-codec-alac`'s and
            // `vaco-codec-vorbis`'s decoders both do exactly this today,
            // `send_packet(None)` a no-op that never moves them out of
            // "always `NeedMoreInput`".
            //
            // Before this fix, `progressed` was unconditionally `true` here,
            // which fed a false "yes, something happened" into
            // `Pipeline::end_step`'s `ProgressGuard` on *every* step for as
            // long as the stuck node was the only thing left runnable — the
            // guard exists precisely to convert a livelock into
            // `LimitError::NoProgress` after enough consecutive do-nothing
            // steps, and a decoder that never drains defeated it by lying
            // about whether the step actually did anything, hanging the
            // whole CLI instead of failing with a diagnosis. Measured against
            // a real `ffmpeg`-encoded ALAC file decoded end to end: before
            // this change, `vaco -i in.m4a -c:a pcm_s16le -f null -` hangs
            // indefinitely; after it, the pipeline reports `NoProgress` and
            // the run ends with a diagnosis instead of a wedged process.
            progressed = out.len() > before || finished;
        }
        Ok(progressed)
    }
}

// ------------------------------------------------------------------ filter

/// A `vaco-filter-core` graph, wrapped so its buffer sources and sinks become
/// pipeline ports.
///
/// The graph has its own bounded links and its own cooperative step function,
/// so this node does not re-implement scheduling: it feeds what the graph asks
/// for, runs the graph to quiescence, and takes what the sinks hold.
#[derive(Debug)]
pub(crate) struct FilterWork {
    pub graph: Graph,
    /// Buffer source node per input port.
    pub sources: Vec<NodeId>,
    /// Buffer sink node per output port.
    pub sinks: Vec<NodeId>,
    pub sink_closed: Vec<bool>,
    pub last_pts: Vec<Timestamp>,
    /// Frames the graph would not take. Same reason as `CodecWork::stashed`.
    pub stashed: Vec<(usize, Frame)>,
    /// Ends announced but not yet applied, for the same reason.
    pub pending_eof: Vec<(usize, Timestamp)>,
    /// What the graph said last time, so readiness costs nothing.
    pub pending_output: bool,
}

impl FilterWork {
    fn ready(&self, ports: Ports<'_>) -> bool {
        if !ports.all_out_room() {
            return false;
        }
        self.pending_output
            || !self.stashed.is_empty()
            || !self.pending_eof.is_empty()
            || ports.any_new_eof()
            || ports.any_input()
    }

    fn advance(
        &mut self,
        inputs: Vec<(usize, Payload)>,
        ended: &[(usize, Timestamp)],
        out: &mut Vec<(usize, Payload)>,
        close: &mut Vec<(usize, Timestamp)>,
    ) -> Result<bool> {
        let mut progressed = false;
        self.pending_eof.extend_from_slice(ended);
        let mut queued: Vec<(usize, Frame)> = std::mem::take(&mut self.stashed);
        for (port, item) in inputs {
            let Some(frame) = item.into_frame() else {
                return Err(Error::InvalidData("a packet reached a filter graph"));
            };
            queued.push((port, frame));
        }
        let mut held = Vec::new();
        for (port, frame) in queued {
            let Some(node) = self.sources.get(port).copied() else {
                return Err(Error::InvalidData(
                    "a filter input port has no buffer source",
                ));
            };
            if !held.is_empty() {
                // Preserve per-port order: once one frame is held back, every
                // later frame must queue behind it.
                held.push((port, frame));
                continue;
            }
            // `Graph::send` hands a refused frame back in `Rejected::frame`,
            // so backpressure needs no defensive copy. This used to clone
            // every frame whose send *might* be refused, because `send`
            // consumed the frame and dropped it while its documentation
            // claimed the opposite; the clone was the workaround, not the
            // design. Now the frame simply comes back and goes on the held
            // queue.
            match self.graph.send(node, frame) {
                Ok(()) => progressed = true,
                // The buffer-source link is full. Keep the frame — and carry
                // on to run the graph anyway, because draining its sinks is
                // exactly what makes room for the frame we are holding.
                // Returning here instead is a livelock: the node would be
                // rescheduled forever with nothing able to change.
                Err(r) if matches!(r.error, Error::OutputPending) => {
                    held.push((port, r.frame));
                }
                Err(r) => return Err(r.error),
            }
        }
        self.stashed = held;
        // A source is closed only when nothing of its is still held back, so
        // the graph never sees end of stream ahead of a frame that precedes it.
        if self.stashed.is_empty() {
            for (port, pts) in std::mem::take(&mut self.pending_eof) {
                if let Some(node) = self.sources.get(port).copied() {
                    self.graph.close_source(node, pts)?;
                    progressed = true;
                }
            }
        }
        let status = self.graph.run()?;
        for port in 0..self.sinks.len() {
            let Some(&sink) = self.sinks.get(port) else {
                continue;
            };
            if self.sink_closed.get(port).copied().unwrap_or(true) {
                continue;
            }
            for _ in 0..MAX_PUMP {
                match self.graph.recv(sink) {
                    Ok(frame) => {
                        if frame.pts.is_some()
                            && let Some(slot) = self.last_pts.get_mut(port)
                        {
                            *slot = frame.pts;
                        }
                        out.push((port, Payload::Frame(frame)));
                        progressed = true;
                    }
                    Err(Error::NeedMoreInput) => break,
                    Err(Error::Eof) => {
                        if let Some(slot) = self.sink_closed.get_mut(port) {
                            *slot = true;
                        }
                        close.push((
                            port,
                            self.last_pts.get(port).copied().unwrap_or(Timestamp::NONE),
                        ));
                        progressed = true;
                        break;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        self.pending_output =
            matches!(status, GraphStatus::HasOutput(_)) || !self.stashed.is_empty();
        if let GraphStatus::Deadlock(_) = &status {
            return Err(Error::InvalidData(
                "a filter graph stopped with work outstanding; see Graph::classify",
            ));
        }
        Ok(progressed)
    }
}

/// The time base of a negotiated link, whichever media it carries.
///
/// A filter graph's links carry their own time base, and the pipeline has to
/// rescale into it on the way in and out of it. `LinkFormat` is an enum over
/// two shapes that both have one, so reading it is a match that every caller
/// would otherwise write.
#[must_use]
pub(crate) fn link_time_base(format: &LinkFormat) -> Rational {
    match format {
        LinkFormat::Video { time_base, .. } | LinkFormat::Audio { time_base, .. } => *time_base,
    }
}

// --------------------------------------------------------------------- mux

/// A muxer already past its header, mid-file.
///
/// The ordering rules — M1 to M11, the interleave queue, the bitstream-filter
/// stage — are `vaco-format-core`'s, not this crate's:
/// [`vaco_format_core::mux::MuxWriter`] already implements and tests all of
/// them, and a second implementation of packet ordering was exactly the kind
/// of duplication D19 exists to prevent. This struct used to be that second
/// implementation — driving a raw `dyn Muxer` through hand-rolled `init`,
/// header, interleave-queue and trailer bookkeeping — which is gap 8 in
/// `planning/INTERFACE-GAPS.md`: it is also why `set_metadata` had to be
/// called before any stream existed, and why the bitstream-filter stage
/// (M6) and the codec-compatibility check (M15) were never reached at all.
/// [`crate::spec::PipelineSpec::build`] now calls
/// [`vaco_format_core::mux::MuxBuilder::open`] instead, so by the time a
/// `MuxWork` exists the header is already written and every stream already
/// passed `query_codec`; this struct's only job is feeding packets to the
/// `MuxWriter` that came back and calling `finish` once every input is done.
pub(crate) struct MuxWork {
    /// `None` once [`MuxWriter::finish`] has run — there is no second
    /// trailer, so the value that could write one is gone rather than
    /// tracked by a separate flag.
    pub writer: Option<MuxWriter>,
    /// Input port to muxer stream index.
    pub stream_index: Vec<u32>,
}

impl std::fmt::Debug for MuxWork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MuxWork")
            .field("finished", &self.writer.is_none())
            .field("report", &self.writer.as_ref().map(MuxWriter::report))
            .finish_non_exhaustive()
    }
}

impl MuxWork {
    fn ready(&self, ports: Ports<'_>) -> bool {
        if self.writer.is_none() {
            return false;
        }
        ports.any_input() || ports.any_new_eof() || ports.all_eof()
    }

    fn advance(
        &mut self,
        inputs: Vec<(usize, Payload)>,
        ended: &[(usize, Timestamp)],
        all_ended: bool,
    ) -> Result<bool> {
        let Some(writer) = self.writer.as_mut() else {
            return Ok(false);
        };
        let mut progressed = false;
        for (port, item) in inputs {
            let Some(mut packet) = item.into_packet() else {
                return Err(Error::InvalidData("a frame reached a muxer"));
            };
            let Some(index) = self.stream_index.get(port).copied() else {
                return Err(Error::InvalidData(
                    "a muxer input port has no output stream",
                ));
            };
            packet.stream_index = index;
            // M1-M4 (rescale), M5 (interleave), M6 (bitstream filters) and M7
            // (the write itself) all happen inside this one call now — the
            // input/output time bases `MuxWriter` rescales between are the
            // ones `MuxBuilder::add_stream`/`open` already settled.
            writer.write_packet(packet)?;
            progressed = true;
        }
        for (port, _) in ended {
            if let Some(index) = self.stream_index.get(*port).copied() {
                writer.end_stream(index)?;
                progressed = true;
            }
        }
        if all_ended && let Some(w) = self.writer.take() {
            w.finish()?;
            progressed = true;
        }
        Ok(progressed)
    }
}
