//! `vfrdet` — a passthrough analysis filter that detects a variable frame
//! rate by comparing consecutive presentation-timestamp deltas.
//!
//! `ffmpeg -h filter=vfrdet`: no options.
//!
//! Unlike [`crate::idet`], the reference's `vfrdet` publishes **no**
//! per-frame metadata at all — measured directly (`ffprobe -show_frames -f
//! lavfi -i testsrc2,vfrdet`, ffmpeg 8.1, 2026-08-23; the `tags` object is
//! empty on every frame). Its only output is a final summary log line
//! (`VFR:... (%u/%u)`) written at filter destruction, leaving no equivalent
//! frame-attached channel for this result.
//! [`Filter::stats`] exposes the same running counts as a plain accessor,
//! the way `vaco-filter-temporal::freezedetect` does for the same reason.
//!
//! # Algorithm
//!
//! Track the delta between each frame's `pts` and the previous one, in the
//! input's own time base. A delta that differs from the immediately
//! preceding delta is one "variable" event; the constant-delta case (every
//! consecutive pair agrees) is what the reference calls constant frame
//! rate.
//!
//! # Independent oracle
//!
//! A synthetic stream with `pts = 0, 1, 2, 3, ...` (constant delta) must
//! report zero variable events; a stream with `pts = 0, 1, 3, 4, 6` (deltas
//! `1, 2, 1, 2`) must report a variable event at every step after the
//! first, since no two consecutive deltas agree — both checked directly
//! against [`Filter::stats`].

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::VIDEO_PAD;

pub const DESC: FilterDesc = FilterDesc {
    name: "vfrdet",
    description: "Variable frame rate detect filter.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

/// Running counts. `pub(crate)`, same reasoning as `idet::Tally`.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Stats {
    pub(crate) constant: u64,
    pub(crate) variable: u64,
}

#[derive(Debug, Default)]
pub(crate) struct Filter {
    prev_pts: Option<i64>,
    prev_delta: Option<i64>,
    stats: Stats,
}

impl Filter {
    #[allow(
        dead_code,
        reason = "exercised by this module's tests; see the module doc"
    )]
    pub(crate) const fn stats(&self) -> Stats {
        self.stats
    }

    fn step(&mut self, pts: Option<i64>) {
        if let (Some(prev), Some(now)) = (self.prev_pts, pts) {
            let delta = now.saturating_sub(prev);
            if let Some(prev_delta) = self.prev_delta {
                if prev_delta == delta {
                    self.stats.constant = self.stats.constant.saturating_add(1);
                } else {
                    self.stats.variable = self.stats.variable.saturating_add(1);
                }
            }
            self.prev_delta = Some(delta);
        }
        self.prev_pts = pts;
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        let _ = ctx;
        self.step(frame.pts.ticks());
        Ok(FrameOut::One(frame))
    }

    fn flush_state(&mut self) {
        self.prev_pts = None;
        self.prev_delta = None;
        self.stats = Stats::default();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let _ = req;
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::default())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_deltas_report_no_variable_events() {
        let mut filt = Filter::default();
        for pts in 0..6i64 {
            filt.step(Some(pts));
        }
        assert_eq!(filt.stats().variable, 0);
        assert!(filt.stats().constant > 0);
    }

    #[test]
    fn alternating_deltas_report_every_step_as_variable() {
        let mut filt = Filter::default();
        for pts in [0i64, 1, 3, 4, 6, 7] {
            filt.step(Some(pts));
        }
        let s = filt.stats();
        assert_eq!(s.constant, 0, "no two consecutive deltas ever agree");
        assert!(s.variable > 0);
    }
}
