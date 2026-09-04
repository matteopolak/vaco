//! The filter framework: pads, links, format negotiation and the scheduler.
//!
//! Filters run under a cooperative `activate` model rather than async. Plan 16
//! §1 argues the choice: an async generator's state is opaque, which makes a
//! stalled graph undebuggable, and executor scheduling order would vary run to
//! run — unacceptable when D6 requires byte-identical output.
//!
//! # What is in here
//!
//! | Module | Contents |
//! |---|---|
//! | [`link`] | [`link::Link`], the per-link frame queue, and the end-of-stream convention |
//! | [`context`] | what a filter can see during one step, and the nine frame-flow rules |
//! | [`negotiate`] | [`Constraint`], [`FormatSet`], the union-find solver and the diagnostic renderer |
//! | [`negotiate::loss`] | what a conversion costs, measured against the reference |
//! | [`sched`] | [`Graph`], readiness, quiescence diagnosis, buffer sources and sinks |
//! | [`adapt`] | the adapters almost every filter is written against |
//! | [`mock`] | worked example filters, and the proof that the traits are usable |
//!
//! # The one idea worth reading first
//!
//! Filter-to-filter frame flow is **N:M**, exactly as packet-to-frame is in
//! `vaco-codec-core`. One input can produce several outputs (`fps` upsampling),
//! several inputs can produce none (`fps` downsampling, `tmix` filling its
//! window), and end of stream can produce many (anything with a buffer). That is
//! why [`Filter::activate`] is a bounded step reporting an [`Activity`] rather
//! than `filter(frame) -> Frame`, and it is the same reason the codec layer is
//! send/receive rather than `decode(packet) -> Frame`.
//!
//! The two contracts to get right before writing a filter are the **frame-flow
//! rules** in [`context`] — F2, that end of stream is sticky and ordered behind
//! the queue, above all — and the **negotiation model** in [`negotiate`].
//!
//! # A complete graph
//!
//! ```
//! use vaco_core::{MediaType, Rational};
//! use vaco_filter_core::mock::{self, Invert};
//! use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
//! use vaco_filter_core::{Graph, GraphStatus, LinkFormat};
//! use vaco_pixfmt::PixFmt;
//!
//! let mut graph = Graph::new();
//! let src = graph.add_source(
//!     "in",
//!     MediaType::Video,
//!     NodeFormats {
//!         outputs: vec![FormatSet::video_exact(PixFmt::Gray8)],
//!         label: "in".into(),
//!         ..NodeFormats::default()
//!     },
//! );
//! let inv = Invert::node(&mut graph, "invert");
//! let sink = graph.add_sink(
//!     "out",
//!     MediaType::Video,
//!     NodeFormats {
//!         inputs: vec![FormatSet::default()],
//!         label: "out".into(),
//!         ..NodeFormats::default()
//!     },
//! );
//! graph.connect(src, 0, inv, 0)?;
//! graph.connect(inv, 0, sink, 0)?;
//! graph.set_source_format(src, mock::gray_link(16, 16, Rational::new(1, 25)))?;
//! graph.configure()?;
//!
//! graph.send(src, mock::gray_frame(16, 16, 0, 0x20))?;
//! graph.close_source(src, vaco_core::Timestamp::new(1))?;
//! graph.run()?;
//!
//! let out = graph.recv(sink)?;
//! assert_eq!(out.plane(0).and_then(|p| p.row(0)).and_then(|r| r.first()), Some(&0xdf));
//! assert!(graph.violations().is_empty());
//! assert_eq!(graph.run()?, GraphStatus::Eof);
//! # Ok::<(), vaco_core::Error>(())
//! ```

#![forbid(unsafe_code)]

use vaco_chlayout::ChannelLayout;
use vaco_color::{ColorInfo, ColorRange};
use vaco_core::{MediaType, Rational, Result};
use vaco_frame::{Frame, FramePool};
use vaco_pixfmt::PixFmt;
use vaco_sampfmt::SampleFmt;

pub mod adapt;
pub mod context;
pub mod link;
pub mod mock;
pub mod negotiate;
pub mod sched;
#[cfg(test)]
mod test_support;
pub mod timeline;

pub use adapt::{
    AudioFilter, Blocked, Dual, DualFilter, Fanout, FanoutFilter, FrameFilter, FrameOut, Paired,
    PairedFilter, Simple, SourceFilter, Sourced,
};
pub use context::{LinkView, NodeLinks, NodeView};
pub use link::{Direction, Link, LinkArena, LinkId, LinkStats, NodeId, PadRef, Rejected, Status};
pub use negotiate::{
    Assignment, AutoConvert, Conflict, ConflictSide, Constraint, ConverterFactory, ConverterSpec,
    FormatSet, Insertion, LinkEnds, NegotiationPlan, NoConversion, NodeFormats, Property, Tie,
    negotiate,
};
pub use sched::{Graph, GraphStatus, Priority, Progress, Stall, Violation};
pub use timeline::{Timeline, TimelineSupport};

/// What one `activate` call accomplished.
///
/// Returned rather than inferred so the scheduler can distinguish "made progress,
/// call me again" from "genuinely blocked" without guessing — which is what lets
/// it *diagnose* a stalled graph instead of hanging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    /// Work was done. Schedule again.
    Progressed,
    /// Blocked on input that has not arrived.
    NeedInput,
    /// Blocked on a downstream consumer that has not drained.
    Blocked,
    /// This filter has emitted everything it ever will.
    Eof,
}

bitflags::bitflags! {
    /// Controls how a graph-level runtime command is delivered.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct CommandFlags: u8 {
        /// Stop after the first matching filter instance.
        const ONE  = 1 << 0;
        /// Refuse a command the filter has not declared safe for a fast path.
        const FAST = 1 << 1;
    }
}

/// One runtime command after graph-level target resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command<'a> {
    /// The command or runtime-option name.
    pub name: &'a str,
    /// The argument passed to the command.
    pub arg: &'a str,
    /// Delivery constraints supplied by the caller.
    pub flags: CommandFlags,
}

/// A runtime command's response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandReply {
    /// The command completed without a response body.
    Ok,
    /// Text returned by a query-like command.
    Text(String),
}

#[derive(Debug)]
pub(crate) struct CommandRequest {
    pub target: String,
    pub name: String,
    pub arg: String,
    pub flags: CommandFlags,
}

/// A filter instance.
///
/// Most filters never implement this directly — the adapters in this crate
/// (`Simple`, `SliceFilter`, `AudioFilter`, `Synced`) cover the common shapes,
/// so a filter author writes only the per-frame work. That matters because there
/// are ~560 filters and an awkward API would be paid for 560 times.
pub trait Filter: Send {
    /// Do one bounded unit of work.
    ///
    /// Must not loop until blocked: the scheduler needs to interleave filters
    /// fairly, and a filter that drains its entire input starves its siblings.
    ///
    /// # Errors
    /// Propagates any failure from the underlying operation.
    fn activate(&mut self, ctx: &mut FilterContext<'_>) -> Result<Activity>;

    /// Called once when link formats have been agreed.
    ///
    /// # Errors
    /// [`vaco_core::Error::Unsupported`] if the negotiated formats are unusable.
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let _ = ctx;
        Ok(())
    }

    /// Discard buffered state after a seek. Does not change configuration.
    ///
    /// **Added after the freeze, with the orchestrator's approval**, on the same
    /// basis as `Muxer::stream_time_base` in `vaco-format-core`: a defaulted
    /// method breaks no implementation, and without it the interface cannot
    /// express something it has to.
    ///
    /// [`Graph::flush`] clears every link's queue and its sticky end of stream,
    /// which is what a seek does to the *framework*. It could not reach the
    /// *filter*, so a delay line, a reorder buffer or an FFT window survived the
    /// seek and produced wrong output afterwards — and an adapter that had
    /// already reported [`Activity::Eof`] could be left holding an output pad the
    /// flush had re-opened, which a fuzz target found and downstream would have
    /// waited on forever.
    ///
    /// Mirrors `Decoder::flush` in `vaco-codec-core`: infallible and total, with
    /// a post-state indistinguishable from a freshly configured filter.
    fn flush(&mut self) {}

    /// Classify a runtime command before it is dispatched.
    ///
    /// Existing commands are short in-process mutations, so the compatible
    /// default is [`CommandFlags::FAST`]. A filter that performs blocking I/O
    /// or expensive rebuilding overrides this and returns an empty set for
    /// that command.
    fn command_flags(&self, name: &str) -> CommandFlags {
        let _ = name;
        CommandFlags::FAST
    }

    /// Process a graph-level runtime command and optionally return text.
    ///
    /// The default preserves existing [`Filter::command`] implementations and
    /// enforces [`CommandFlags::FAST`] through [`Filter::command_flags`]. A
    /// query-like filter overrides this method to return
    /// [`CommandReply::Text`].
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::Unsupported`] when `FAST` was requested for a slow
    /// command, or anything returned by [`Filter::command`].
    fn process_command(&mut self, command: &Command<'_>) -> Result<CommandReply> {
        if command.flags.contains(CommandFlags::FAST)
            && !self
                .command_flags(command.name)
                .contains(CommandFlags::FAST)
        {
            return Err(vaco_core::Error::Unsupported("filter command is not fast"));
        }
        self.command(command.name, command.arg)?;
        Ok(CommandReply::Ok)
    }

    /// Handle a runtime command (`sendcmd`, `zmq`, or the timeline).
    ///
    /// # Errors
    /// [`vaco_core::Error::Option`] for an unknown command or bad value.
    fn command(&mut self, name: &str, value: &str) -> Result<()> {
        let _ = (name, value);
        Err(vaco_core::Error::Unsupported(
            "filter accepts no runtime commands",
        ))
    }
}

/// The scheduler's handle to one filter's links.
///
/// See the [`context`] module for the frame-flow contract these methods
/// implement, and for the additional accessors — `peek_input`,
/// `take_input_status`, `input_link`, `output_link`, `set_output_link`,
/// `close_output_at`, `pool`, `send_command` — that a filter beyond the
/// simplest shape needs.
#[derive(Debug)]
pub struct FilterContext<'a> {
    links: &'a mut LinkArena,
    node: &'a NodeLinks,
    pool: &'a FramePool,
    /// Every node's public identity, for [`FilterContext::graph_nodes`] and
    /// resolving [`context::LinkView`]'s `PadRef`s from
    /// [`FilterContext::graph_links`].
    graph_nodes: &'a [context::NodeView],
    /// Requests emitted by an in-graph command source during `activate`.
    commands: Option<&'a mut Vec<CommandRequest>>,
    /// Set when a pushed frame did not match its link's negotiated format.
    format_mismatch: bool,
    /// Set when a push landed on an already-closed pad.
    push_after_close: bool,
    /// Set when a push was refused for backpressure, which loses the frame.
    /// See [`Violation::FrameDroppedByBackpressure`].
    dropped_by_backpressure: bool,
}

impl FilterContext<'_> {
    /// Take a frame from an input pad, if one is queued.
    ///
    /// Frames come out in order and end of stream never jumps the queue
    /// (rule F1). `None` means "nothing queued right now", which is *not* the
    /// same as end of stream — ask [`FilterContext::input_at_eof`] for that.
    pub fn take_input(&mut self, pad: usize) -> Option<Frame> {
        let id = u32::try_from(pad).ok().and_then(|p| self.node.input(p))?;
        self.links.get_mut(id).and_then(link::Link::pop)
    }

    /// Push a frame to an output pad.
    ///
    /// The frame's timestamps are rescaled into the output link's time base
    /// exactly, with `Rounding::NearInf` (rule F9). Its format must match what
    /// negotiation agreed; a mismatch is recorded as
    /// [`Violation::FrameFormatMismatch`] rather than being allowed to surface
    /// at the sink.
    ///
    /// # Errors
    /// [`vaco_core::Error::OutputPending`] when the link is full — backpressure,
    /// not a failure. [`vaco_core::Error::Eof`] when the pad has been closed.
    /// [`vaco_core::Error::InvalidData`] for a pad this filter does not have.
    pub fn push_output(&mut self, pad: usize, frame: Frame) -> Result<()> {
        self.push_checked(pad, frame)
    }

    /// Signal that an output pad will produce nothing further.
    ///
    /// Idempotent (rule F4). Use [`FilterContext::close_output_at`] when the
    /// timestamp the stream ended at matters, which it does for `tpad`, `xfade`
    /// and `concat`.
    pub fn close_output(&mut self, pad: usize) {
        self.close_output_at(pad, vaco_core::Timestamp::NONE);
    }

    /// Whether an input pad has reached EOF and drained.
    ///
    /// **Sticky**: once true it stays true (rule F2). A filter may ask as often
    /// as it likes and will not get two different answers.
    #[must_use]
    pub fn input_at_eof(&self, pad: usize) -> bool {
        u32::try_from(pad)
            .ok()
            .and_then(|p| self.node.input(p))
            .and_then(|id| self.links.get(id))
            .is_some_and(link::Link::at_eof)
    }

    /// The agreed configuration of a link, valid after `configure`.
    ///
    /// `pad` indexes a **concatenated** space: input pads first, then output
    /// pads. So a 1-in-1-out filter reads its input at `0` and its output at
    /// `1`, a source reads its output at `0`, and a sink reads its input at `0`.
    /// That is the only total reading of a single index over two pad lists, and
    /// it is easy to get wrong — prefer [`FilterContext::input_link`] and
    /// [`FilterContext::output_link`], which say which they mean. See the
    /// signature gaps section of `docs/filter/vaco-filter-core.md`.
    ///
    /// An out-of-range index yields an unconfigured placeholder, recognisable by
    /// its zero dimensions, rather than panicking.
    #[must_use]
    pub fn link(&self, pad: usize) -> &LinkFormat {
        let inputs = self.node.inputs().len();
        let id = if pad < inputs {
            u32::try_from(pad).ok().and_then(|p| self.node.input(p))
        } else {
            u32::try_from(pad - inputs)
                .ok()
                .and_then(|p| self.node.output(p))
        };
        self.links.format(id)
    }
}

/// The negotiated format of one link.
#[derive(Debug, Clone)]
pub enum LinkFormat {
    Video {
        format: PixFmt,
        width: u32,
        height: u32,
        time_base: Rational,
        frame_rate: Rational,
        sample_aspect_ratio: Rational,
        color: ColorInfo,
    },
    Audio {
        format: SampleFmt,
        sample_rate: u32,
        layout: ChannelLayout,
        time_base: Rational,
    },
}

impl LinkFormat {
    /// Overwrite the format fields this set resolved, leaving geometry and
    /// timing alone.
    ///
    /// Negotiation decides *what* flows; everything else on the link is
    /// inherited from upstream or set by the filter in `configure`.
    pub fn apply(&mut self, set: &FormatSet) {
        match self {
            Self::Video { format, .. } => {
                if let Some(f) = set.pixel_formats.as_ref().and_then(Constraint::resolved) {
                    *format = *f;
                }
            }
            Self::Audio {
                format,
                sample_rate,
                layout,
                ..
            } => {
                if let Some(f) = set.sample_formats.as_ref().and_then(Constraint::resolved) {
                    *format = *f;
                }
                if let Some(r) = set.sample_rates.as_ref().and_then(Constraint::resolved) {
                    *sample_rate = *r;
                }
                if let Some(l) = set.channel_layouts.as_ref().and_then(Constraint::resolved) {
                    *layout = l.clone();
                }
            }
        }
    }

    /// The constraint set this format satisfies, as a source pad would declare
    /// it. Every property is [`Constraint::Exact`].
    #[must_use]
    pub fn to_format_set(&self) -> FormatSet {
        match self {
            Self::Video { format, .. } => FormatSet::video_exact(*format),
            Self::Audio {
                format,
                sample_rate,
                layout,
                ..
            } => FormatSet::audio_exact(*format, *sample_rate, layout.clone()),
        }
    }
}

/// A filter's input or output pad.
#[derive(Debug, Clone, Copy)]
pub struct Pad {
    pub name: &'static str,
    pub media_type: MediaType,
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct FilterFlags: u16 {
        /// Input pad count is determined by options, not fixed.
        const DYNAMIC_INPUTS  = 1 << 0;
        const DYNAMIC_OUTPUTS = 1 << 1;
        /// Can process independent slices of a frame concurrently.
        const SLICE_THREADS   = 1 << 2;
        /// Touches only metadata; the framework may skip it for hardware frames.
        const METADATA_ONLY   = 1 << 3;
        /// Supports `enable=`, evaluated by the framework.
        const TIMELINE_GENERIC = 1 << 4;
        /// Supports `enable=`, evaluated by the filter itself.
        const TIMELINE_INTERNAL = 1 << 5;
    }
}

impl FilterFlags {
    /// Which of the three timeline modes this flag set names.
    ///
    /// Both timeline bits set is meaningless — a filter either evaluates
    /// `enable=` itself or lets the framework do it. `Internal` wins, because a
    /// filter that says it wants to see the flag itself has a reason.
    #[must_use]
    pub const fn timeline(self) -> TimelineSupport {
        if self.contains(Self::TIMELINE_INTERNAL) {
            TimelineSupport::Internal
        } else if self.contains(Self::TIMELINE_GENERIC) {
            TimelineSupport::Generic
        } else {
            TimelineSupport::None
        }
    }
}

/// Static description of a filter.
#[derive(Debug, Clone, Copy)]
pub struct FilterDesc {
    pub name: &'static str,
    pub description: &'static str,
    pub inputs: &'static [Pad],
    pub outputs: &'static [Pad],
    pub flags: FilterFlags,
}

impl FilterDesc {
    /// Whether the descriptor is self-consistent.
    ///
    /// `TIMELINE_GENERIC` means "the framework forwards the input frame
    /// untouched when `enable` is false", which is only well defined for one
    /// input and one output of the same media type. The reference leaves that as
    /// a convention; checking it here turns a class of silent misbehaviour into
    /// a registration-time error.
    #[must_use]
    pub fn is_consistent(&self) -> bool {
        if self.flags.timeline() == TimelineSupport::Generic {
            let one_each = self.inputs.len() == 1 && self.outputs.len() == 1;
            let same = match (self.inputs.first(), self.outputs.first()) {
                (Some(i), Some(o)) => i.media_type == o.media_type,
                _ => false,
            };
            if !(one_each && same) {
                return false;
            }
        }
        true
    }
}

const _: () = {
    let _ = ColorRange::Full;
};
