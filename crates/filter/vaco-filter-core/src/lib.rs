//! The filter framework: pads, links, format negotiation and the scheduler.
//!
//! Filters run under a cooperative `activate` model rather than async. Plan 16
//! §1 argues the choice: an async generator's state is opaque, which makes a
//! stalled graph undebuggable, and executor scheduling order would vary run to
//! run — unacceptable when D6 requires byte-identical output.

use vaco_chlayout::ChannelLayout;
use vaco_color::{ColorInfo, ColorRange};
use vaco_core::{MediaType, Rational, Result};
use vaco_frame::Frame;
use vaco_pixfmt::PixFmt;
use vaco_sampfmt::SampleFmt;

pub mod negotiate;
pub use negotiate::{Constraint, FormatSet};

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
#[derive(Debug)]
pub struct FilterContext<'a> {
    marker: std::marker::PhantomData<&'a ()>,
}

impl FilterContext<'_> {
    /// Take a frame from an input pad, if one is queued.
    pub fn take_input(&mut self, pad: usize) -> Option<Frame> {
        let _ = pad;
        todo!("P0-03 freeze: pop from the link's frame queue")
    }

    /// Push a frame to an output pad.
    ///
    /// # Errors
    /// Propagates downstream failure.
    pub fn push_output(&mut self, pad: usize, frame: Frame) -> Result<()> {
        let _ = (pad, frame);
        todo!("P0-03 freeze: enqueue and mark the consumer ready")
    }

    /// Signal that an output pad will produce nothing further.
    pub fn close_output(&mut self, pad: usize) {
        let _ = pad;
        todo!("P0-03 freeze: propagate EOF downstream")
    }

    /// Whether an input pad has reached EOF and drained.
    #[must_use]
    pub fn input_at_eof(&self, pad: usize) -> bool {
        let _ = pad;
        todo!("P0-03 freeze")
    }

    /// The agreed configuration of a link, valid after `configure`.
    #[must_use]
    pub fn link(&self, pad: usize) -> &LinkFormat {
        let _ = pad;
        todo!("P0-03 freeze")
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

/// Static description of a filter.
#[derive(Debug, Clone, Copy)]
pub struct FilterDesc {
    pub name: &'static str,
    pub description: &'static str,
    pub inputs: &'static [Pad],
    pub outputs: &'static [Pad],
    pub flags: FilterFlags,
}

const _: () = {
    let _ = ColorRange::Full;
};
