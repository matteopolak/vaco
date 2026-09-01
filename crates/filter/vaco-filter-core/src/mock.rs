//! Worked example filters, and the proof that the traits are usable.
//!
//! No real filter exists yet, so this is how the framework gets tested — and it
//! is deliberately written the way a real filter should be written, so the
//! author of the first one has something correct to copy.
//!
//! Five shapes, chosen because between them they exercise every part of the
//! contract that a 1:1 pixel filter would leave untested:
//!
//! | Filter | Shape | What it proves |
//! |---|---|---|
//! | [`Counter`] | source | on-demand production, end-of-stream timestamp |
//! | [`Invert`] | 1-in 1-out video | copy-on-write through `Arc::make_mut`, format negotiation |
//! | [`Gain`] | 1-in 1-out audio | the audio path, and a fixed input block size |
//! | [`Fps`] | 1-in N-out video | **N:M frame flow** and an output time base that differs from the input's |
//! | [`Drop`] | 1-in 0-or-1-out | a filter that consumes without producing |
//!
//! [`Fps`] is the important one. A framework tested only on 1:1 filters is a
//! framework whose N:M contract is hypothetical — and N:M is exactly what
//! `activate` exists for.

use smallvec::SmallVec;
use vaco_chlayout::ChannelLayout;
use vaco_color::ColorInfo;
use vaco_core::{Duration, MediaType, Rational, Result, Rounding, TimeBase, Timestamp};
use vaco_frame::{Frame, FrameData, FramePool};
use vaco_pixfmt::PixFmt;
use vaco_sampfmt::SampleFmt;

use crate::adapt::{Blocked, FrameFilter, FrameOut, Simple, SourceFilter, Sourced};
use crate::negotiate::{FormatSet, NodeFormats};
use crate::{FilterContext, FilterDesc, FilterFlags, Graph, LinkFormat, NodeId, Pad};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];
const AUDIO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Audio,
}];

// ------------------------------------------------------------ helpers

/// A `gray8` link format of the given geometry.
#[must_use]
pub fn gray_link(width: u32, height: u32, time_base: TimeBase) -> LinkFormat {
    LinkFormat::Video {
        format: PixFmt::Gray8,
        width,
        height,
        time_base,
        frame_rate: time_base.inverse(),
        sample_aspect_ratio: Rational::ONE,
        color: ColorInfo::default(),
    }
}

/// An `s16` stereo link format at `rate`.
#[must_use]
pub fn audio_link(rate: u32) -> LinkFormat {
    LinkFormat::Audio {
        format: SampleFmt::S16,
        sample_rate: rate,
        layout: ChannelLayout::STEREO,
        time_base: Rational::new(1, i32::try_from(rate).unwrap_or(1)),
    }
}

/// A `gray8` frame filled with `value`, timestamped `pts` in 1/25.
///
/// # Panics
///
/// Never: the pool only refuses hardware formats and over-cap allocations, and
/// a small `gray8` frame is neither. The `unwrap_or_else` exists so that a
/// caller in a doctest does not have to thread a `Result`.
#[must_use]
pub fn gray_frame(width: u32, height: u32, pts: i64, value: u8) -> Frame {
    let pool = FramePool::default();
    let mut frame = pool
        .acquire_video(PixFmt::Gray8, width, height)
        .unwrap_or_else(|_| {
            Frame::from_data(FrameData::Video {
                format: PixFmt::Gray8,
                width,
                height,
                planes: SmallVec::new(),
            })
        });
    if let Some(mut plane) = frame.plane_mut(0) {
        plane.fill(value);
    }
    frame.pts = Timestamp::new(pts);
    frame.time_base = Rational::new(1, 25);
    frame.duration = Duration(1);
    frame
}

/// An `s16` stereo frame of `samples` samples, timestamped `pts` in `1/rate`.
#[must_use]
pub fn audio_frame(rate: u32, samples: u32, pts: i64) -> Frame {
    let pool = FramePool::default();
    let mut frame = pool
        .acquire_audio(SampleFmt::S16, ChannelLayout::STEREO, samples, rate)
        .unwrap_or_else(|_| {
            Frame::from_data(FrameData::Audio {
                format: SampleFmt::S16,
                sample_rate: rate,
                samples,
                layout: ChannelLayout::STEREO,
                planes: SmallVec::new(),
            })
        });
    frame.pts = Timestamp::new(pts);
    frame.time_base = Rational::new(1, i32::try_from(rate).unwrap_or(1));
    frame.duration = Duration(i64::from(samples));
    frame
}

// ------------------------------------------------------------ Counter

/// A source producing `count` `gray8` frames, each filled with its own index.
///
/// Produces only when asked, which is what makes a graph with a slow sink
/// bounded rather than a memory leak.
#[derive(Debug)]
pub struct Counter {
    width: u32,
    height: u32,
    remaining: u64,
    next: i64,
}

impl Counter {
    /// The static descriptor.
    pub const DESC: FilterDesc = FilterDesc {
        name: "counter",
        description: "generate a numbered sequence of grey frames",
        inputs: &[],
        outputs: VIDEO_PAD,
        flags: FilterFlags::empty(),
    };

    /// A source of `count` frames.
    #[must_use]
    pub const fn new(width: u32, height: u32, count: u64) -> Self {
        Self {
            width,
            height,
            remaining: count,
            next: 0,
        }
    }

    /// What this filter's pads accept.
    #[must_use]
    pub fn formats(label: &str) -> NodeFormats {
        NodeFormats {
            inputs: Vec::new(),
            outputs: vec![FormatSet::video_exact(PixFmt::Gray8)],
            ties: Vec::new(),
            label: label.to_owned(),
        }
    }

    /// Add one to `graph`, wired to nothing.
    pub fn node(graph: &mut Graph, label: &str, width: u32, height: u32, count: u64) -> NodeId {
        graph.add(
            Self::DESC,
            Self::formats(label),
            Box::new(Sourced::new(Self::new(width, height, count))),
        )
    }
}

impl SourceFilter for Counter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        // A source is the only filter that has to do this: it has no input to
        // inherit geometry and timing from.
        let mut format = gray_link(self.width, self.height, Rational::new(1, 25));
        if let (Some(LinkFormat::Video { format: f, .. }), LinkFormat::Video { format: slot, .. }) =
            (ctx.output_link(0), &mut format)
        {
            *slot = *f;
        }
        ctx.set_output_link(0, format);
        Ok(())
    }

    fn produce(&mut self, _ctx: &mut FilterContext<'_>) -> Result<Option<Frame>> {
        if self.remaining == 0 {
            return Ok(None);
        }
        self.remaining -= 1;
        let pts = self.next;
        self.next = self.next.saturating_add(1);
        let value = u8::try_from(pts.rem_euclid(256)).unwrap_or(0);
        Ok(Some(gray_frame(self.width, self.height, pts, value)))
    }

    fn end_pts(&self) -> Timestamp {
        Timestamp::new(self.next)
    }
}

// ------------------------------------------------------------- Invert

/// Inverts every sample of a `gray8` frame, in place where it can.
///
/// The interesting line is `frame.plane_mut(0)`, which is `Arc::make_mut`: it
/// writes through when this filter holds the only reference and copies exactly
/// once when it does not. No flag, no `NEEDS_WRITABLE` pad bit, no way to get it
/// wrong — the reference needs all three because C cannot express ownership.
#[derive(Debug, Default)]
pub struct Invert;

impl Invert {
    /// The static descriptor.
    pub const DESC: FilterDesc = FilterDesc {
        name: "invert",
        description: "invert a greyscale frame",
        inputs: VIDEO_PAD,
        outputs: VIDEO_PAD,
        flags: FilterFlags::TIMELINE_GENERIC,
    };

    /// What this filter's pads accept: `gray8` on both, tied.
    #[must_use]
    pub fn formats(label: &str) -> NodeFormats {
        NodeFormats::uniform(
            1,
            1,
            MediaType::Video,
            &FormatSet::video_exact(PixFmt::Gray8),
            label,
        )
    }

    /// Add one to `graph`.
    pub fn node(graph: &mut Graph, label: &str) -> NodeId {
        graph.add(
            Self::DESC,
            Self::formats(label),
            Box::new(Simple::new(Self)),
        )
    }
}

impl FrameFilter for Invert {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, mut input: Frame) -> Result<FrameOut> {
        if let Some(mut plane) = input.plane_mut(0) {
            for row in plane.rows_mut() {
                for byte in row.iter_mut() {
                    *byte = !*byte;
                }
            }
        }
        Ok(FrameOut::One(input))
    }
}

// --------------------------------------------------------------- Gain

/// Scales every `s16` sample by a power of two, in blocks of a fixed size.
///
/// Declares `frame_size`, so the adapter's FIFO guarantees it sees exactly that
/// many samples per call — the thing an FFT-domain filter needs and that is
/// impossible to retrofit once filters have been written assuming otherwise.
#[derive(Debug)]
pub struct Gain {
    shift: u32,
    block: u32,
}

impl Gain {
    /// The static descriptor.
    pub const DESC: FilterDesc = FilterDesc {
        name: "gain",
        description: "scale s16 samples by a power of two",
        inputs: AUDIO_PAD,
        outputs: AUDIO_PAD,
        flags: FilterFlags::empty(),
    };

    /// Halve `shift` times, in blocks of `block` samples.
    #[must_use]
    pub const fn new(shift: u32, block: u32) -> Self {
        Self { shift, block }
    }

    /// What this filter's pads accept: `s16` stereo at `rate`, tied.
    #[must_use]
    pub fn formats(label: &str, rate: u32) -> NodeFormats {
        NodeFormats::uniform(
            1,
            1,
            MediaType::Audio,
            &FormatSet::audio_exact(SampleFmt::S16, rate, ChannelLayout::STEREO),
            label,
        )
    }

    /// Add one to `graph`.
    pub fn node(graph: &mut Graph, label: &str, rate: u32, shift: u32, block: u32) -> NodeId {
        graph.add(
            Self::DESC,
            Self::formats(label, rate),
            Box::new(Simple::new(Blocked::new(Self::new(shift, block)))),
        )
    }
}

impl crate::adapt::AudioFilter for Gain {
    fn frame_size(&self) -> u32 {
        self.block
    }

    fn filter_samples(
        &mut self,
        _ctx: &mut FilterContext<'_>,
        mut input: Frame,
    ) -> Result<FrameOut> {
        let shift = self.shift.min(15);
        for mut plane in input.planes_mut() {
            for row in plane.rows_mut() {
                for pair in row.as_chunks_mut::<2>().0.iter_mut() {
                    let [lo, hi] = pair;
                    let value = i16::from_le_bytes([*lo, *hi]) >> shift;
                    let [a, b] = value.to_le_bytes();
                    *lo = a;
                    *hi = b;
                }
            }
        }
        Ok(FrameOut::One(input))
    }
}

// ---------------------------------------------------------------- Fps

/// Resamples the frame rate: N frames in, M frames out.
///
/// Duplicates when speeding up and drops when slowing down, choosing for each
/// output slot the input frame whose presentation time is nearest — the same
/// nearest-source rule the reference's `fps` uses, and the reason this is the
/// filter worth having as a worked example. It is also the only one here that
/// **changes the output link's time base**, which it does in `configure`; the
/// framework then rescales every pushed frame exactly, in `i128`, with no
/// floating point anywhere on the path.
#[derive(Debug)]
pub struct Fps {
    target: Rational,
    in_base: TimeBase,
    out_base: TimeBase,
    /// The next output frame index to emit.
    next_out: i64,
    /// The most recent input frame, held so an output slot between two inputs
    /// can be filled without waiting.
    held: Option<Frame>,
    /// The first output slot the held input does *not* cover, derived from its
    /// own duration. Without it the tail of the stream is short by however many
    /// slots the last input spanned — the classic rate-conversion bug.
    held_until: i64,
    seen_input: bool,
}

impl Fps {
    /// The static descriptor.
    pub const DESC: FilterDesc = FilterDesc {
        name: "fps",
        description: "convert the frame rate by duplicating or dropping",
        inputs: VIDEO_PAD,
        outputs: VIDEO_PAD,
        flags: FilterFlags::empty(),
    };

    /// Convert to `target` frames per second.
    #[must_use]
    pub const fn new(target: Rational) -> Self {
        Self {
            target,
            in_base: Rational::UNDEFINED,
            out_base: Rational::UNDEFINED,
            next_out: 0,
            held: None,
            held_until: 0,
            seen_input: false,
        }
    }

    /// What this filter's pads accept: any pixel format, both pads agreeing.
    #[must_use]
    pub fn formats(label: &str) -> NodeFormats {
        NodeFormats::passthrough(1, 1, MediaType::Video, label)
    }

    /// Add one to `graph`.
    pub fn node(graph: &mut Graph, label: &str, target: Rational) -> NodeId {
        graph.add(
            Self::DESC,
            Self::formats(label),
            Box::new(Simple::new(Self::new(target))),
        )
    }

    /// Emit every output slot up to, but not including, `limit`.
    fn emit_until(&mut self, limit: i64) -> SmallVec<[Frame; 4]> {
        let mut out = SmallVec::new();
        let Some(source) = self.held.as_ref() else {
            return out;
        };
        while self.next_out < limit {
            let mut frame = source.clone();
            frame.time_base = self.out_base;
            frame.pts = Timestamp::new(self.next_out);
            frame.duration = Duration(1);
            out.push(frame);
            self.next_out = self.next_out.saturating_add(1);
        }
        out
    }

    /// The output slot an input at `pts` falls in.
    ///
    /// Floor, not nearest: output slot `k` covers input times
    /// `[k·out_base, (k+1)·out_base)`, so an input belongs to the slot its own
    /// time falls inside. Rounding to nearest would put an input at 0.96·slot
    /// into the *next* slot, and a one-second 25 fps stream would then yield
    /// eleven frames at 10 fps instead of ten — which is what it did before this
    /// comment existed.
    fn slot(&self, pts: Timestamp) -> Option<i64> {
        pts.rescale(self.in_base, self.out_base, Rounding::Down)
            .ticks()
    }
}

impl FrameFilter for Fps {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let Some(slot) = self.slot(input.pts) else {
            // No timestamp: pass through at the next slot rather than guessing.
            self.held_until = self.next_out.saturating_add(1);
            self.held = Some(input);
            let out = self.emit_until(self.next_out.saturating_add(1));
            return Ok(FrameOut::from_iter(out));
        };
        // Everything before this input's slot is served by the previous frame.
        let out = if self.seen_input {
            self.emit_until(slot)
        } else {
            self.next_out = slot;
            SmallVec::new()
        };
        self.seen_input = true;
        // How far this input reaches: its own end time, in output slots. A frame
        // with no duration covers exactly one slot.
        let end = if input.duration.0 > 0 {
            input
                .pts
                .offset(input.duration.0)
                .rescale(self.in_base, self.out_base, Rounding::Down)
                .ticks()
                .unwrap_or(slot.saturating_add(1))
        } else {
            slot.saturating_add(1)
        };
        self.held_until = end.max(slot.saturating_add(1));
        self.held = Some(input);
        Ok(FrameOut::from_iter(out))
    }

    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        self.in_base = ctx
            .input_link(0)
            .map_or(Rational::UNDEFINED, LinkFormat::time_base);
        self.out_base = self.target.inverse();
        if let Some(mut format) = ctx.output_link(0).cloned() {
            format.set_time_base(self.out_base);
            if let LinkFormat::Video { frame_rate, .. } = &mut format {
                *frame_rate = self.target;
            }
            ctx.set_output_link(0, format);
        }
        Ok(())
    }

    fn flush_state(&mut self) {
        // `Fps` holds the previous input so an output slot between two inputs
        // can be filled. Across a seek that frame is from the wrong place in the
        // stream, and keeping it would splice it onto the new position.
        self.held = None;
        self.held_until = 0;
        self.next_out = 0;
        self.seen_input = false;
    }

    fn flush(&mut self, _ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        // The last input covers exactly one further slot: without this the final
        // frame of every stream is silently dropped, which is the classic
        // rate-conversion bug.
        if self.held.is_none() {
            return Ok(FrameOut::None);
        }
        let out = self.emit_until(self.held_until.max(self.next_out.saturating_add(1)));
        self.held = None;
        Ok(FrameOut::from_iter(out))
    }
}

// --------------------------------------------------------------- Drop

/// Drops every `n`th frame. The "consumes without producing" shape.
#[derive(Debug)]
pub struct Drop {
    every: u64,
    seen: u64,
}

impl Drop {
    /// The static descriptor.
    pub const DESC: FilterDesc = FilterDesc {
        name: "dropevery",
        description: "drop every nth frame",
        inputs: VIDEO_PAD,
        outputs: VIDEO_PAD,
        flags: FilterFlags::empty(),
    };

    /// Drop one frame in `every`.
    #[must_use]
    pub const fn new(every: u64) -> Self {
        Self { every, seen: 0 }
    }

    /// What this filter's pads accept: anything, both pads agreeing.
    #[must_use]
    pub fn formats(label: &str) -> NodeFormats {
        NodeFormats::passthrough(1, 1, MediaType::Video, label)
    }

    /// Add one to `graph`.
    pub fn node(graph: &mut Graph, label: &str, every: u64) -> NodeId {
        graph.add(
            Self::DESC,
            Self::formats(label),
            Box::new(Simple::new(Self::new(every))),
        )
    }
}

impl FrameFilter for Drop {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        self.seen = self.seen.saturating_add(1);
        if self.every != 0 && self.seen.is_multiple_of(self.every) {
            return Ok(FrameOut::None);
        }
        Ok(FrameOut::One(input))
    }
}

// --------------------------------------------------------------- Sink

/// Convenience: a video sink accepting anything.
#[must_use]
pub fn any_video_sink(label: &str) -> NodeFormats {
    NodeFormats {
        inputs: vec![FormatSet::default()],
        outputs: Vec::new(),
        ties: Vec::new(),
        label: label.to_owned(),
    }
}

/// Convenience: an audio sink accepting anything.
#[must_use]
pub fn any_audio_sink(label: &str) -> NodeFormats {
    NodeFormats {
        inputs: vec![FormatSet::default()],
        outputs: Vec::new(),
        ties: Vec::new(),
        label: label.to_owned(),
    }
}

/// Convenience: a video source declaring exactly `format`.
#[must_use]
pub fn video_source_formats(label: &str, format: PixFmt) -> NodeFormats {
    NodeFormats {
        inputs: Vec::new(),
        outputs: vec![FormatSet::video_exact(format)],
        ties: Vec::new(),
        label: label.to_owned(),
    }
}

/// Convenience: an audio source declaring exactly this configuration.
#[must_use]
pub fn audio_source_formats(label: &str, rate: u32) -> NodeFormats {
    NodeFormats {
        inputs: Vec::new(),
        outputs: vec![FormatSet::audio_exact(
            SampleFmt::S16,
            rate,
            ChannelLayout::STEREO,
        )],
        ties: Vec::new(),
        label: label.to_owned(),
    }
}
