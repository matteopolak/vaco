//! `freezedetect` — a passthrough analysis filter that flags runs of frames
//! whose luma barely changes for at least `duration` seconds.
//!
//! `ffmpeg -h filter=freezedetect`: `n`/`noise` (`0..=1`, default `0.001`,
//! a fraction-of-full-scale mean-absolute-frame-difference tolerance) and
//! `d`/`duration` (a time spec, default `2` seconds).
//!
//! # What this crate cannot reproduce: metadata export
//!
//! The reference reports freeze events as frame-attached dictionary entries
//! (`lavfi.freezedetect.freeze_start`/`freeze_duration`/`freeze_end`) and log
//! lines. `vaco_frame::Frame` has no open-ended per-frame metadata
//! dictionary — [`vaco_frame::FrameSideData`] is a closed, `#[non_exhaustive]`
//! enum generated from a fixed side-data table this crate does not own, so
//! there is nowhere to attach an arbitrary key/value pair without editing
//! `vaco-frame`, which is out of this brief's scope. [`Filter::events`]
//! exposes the same information (start/end timestamps, in seconds) as a
//! plain accessor on the concrete filter type instead — real detection
//! logic, just not the reference's export mechanism. Documented here and in
//! `docs/filter/vaco-filter-temporal.md` rather than silently dropped.
//!
//! # Algorithm
//!
//! [`vaco_filter_vdsp::normalised_sad`] on the luma plane, between each
//! frame and its predecessor, is this filter's mean-absolute-frame-
//! difference. A run of consecutive frames all scoring at or below `noise`
//! is a candidate freeze, starting at the *first* frame of the run (the one
//! before the first near-zero diff — a freeze is "the picture stopped
//! changing here", not "these two frames matched"). Once a candidate run's
//! span reaches `duration` seconds it becomes a confirmed freeze event; a
//! later frame that breaks the run closes it.
//!
//! # Independent oracle
//!
//! A synthetic stream of `N` byte-identical frames (every `normalised_sad`
//! along it is exactly `0.0`, at or below any non-negative `noise`) spanning
//! more than `duration` seconds at the stream's frame rate must produce
//! exactly one freeze event; a synthetic stream where every frame differs by
//! more than `noise` (a moving ramp) must produce none — both checked
//! against [`Filter::events`] directly, not against this filter's own
//! per-frame decision re-examined a second way.

use vaco_core::{MediaType, Rational, Result, Timestamp};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{VIDEO_PAD, str_opt};

pub const DESC: FilterDesc = FilterDesc {
    name: "freezedetect",
    description: "Detects frozen video input.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

/// One detected freeze span, in seconds from stream start (the input's own
/// time base). `end` is `None` while the freeze is still ongoing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct FreezeEvent {
    pub(crate) start: f64,
    pub(crate) end: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Options {
    noise: f64,
    duration_secs: f64,
}

#[derive(Debug)]
pub(crate) struct Filter {
    opts: Options,
    prev: Option<Frame>,
    run_start_secs: Option<f64>,
    confirmed: bool,
    events: Vec<FreezeEvent>,
}

fn seconds(pts: Timestamp, tb: Rational) -> f64 {
    let Some(ticks) = pts.ticks() else { return 0.0 };
    #[allow(clippy::cast_precision_loss, reason = "display-scale timestamp conversion")]
    {
        ticks as f64 * f64::from(tb.num) / f64::from(tb.den.max(1))
    }
}

impl Filter {
    pub(crate) fn new(opts: Options) -> Self {
        Self {
            opts,
            prev: None,
            run_start_secs: None,
            confirmed: false,
            events: Vec::new(),
        }
    }

    /// Every freeze event observed so far, confirmed or (if still ongoing)
    /// with `end: None`. `pub(crate)` rather than `pub`: [`Filter`] itself is
    /// crate-internal (reached only through the boxed `dyn Filter` the
    /// registry hands back), so a wider visibility here would be
    /// unreachable dead API, not a real public surface.
    #[must_use]
    #[allow(
        dead_code,
        reason = "exercised by this module's tests; not yet wired to a production \
                  metadata-export sink (see the module doc's metadata-export note)"
    )]
    pub(crate) fn events(&self) -> Vec<FreezeEvent> {
        let mut all = self.events.clone();
        // `self.events` only ever holds *closed* runs (pushed when a run
        // ends, see `step`), so the still-open run this represents — if
        // any — can never already be in it: no de-duplication needed, and
        // none is attempted (which would otherwise mean comparing `f64`
        // timestamps for exact equality).
        if self.confirmed
            && let Some(start) = self.run_start_secs
        {
            all.push(FreezeEvent { start, end: None });
        }
        all
    }

    fn step(&mut self, frame: Frame, tb: Rational) -> FrameOut {
        let now = seconds(frame.pts, tb);
        if let Some(prev) = &self.prev {
            let similar = match (frame.plane(0), prev.plane(0)) {
                (Some(a), Some(b)) => vaco_filter_vdsp::normalised_sad(a, b) <= self.opts.noise,
                _ => false,
            };
            if similar {
                if self.run_start_secs.is_none() {
                    // The freeze began at the *previous* frame's timestamp.
                    self.run_start_secs = Some(seconds(prev.pts, tb));
                }
                if let Some(start) = self.run_start_secs
                    && !self.confirmed
                    && now - start >= self.opts.duration_secs
                {
                    self.confirmed = true;
                }
            } else {
                if self.confirmed
                    && let Some(start) = self.run_start_secs
                {
                    self.events.push(FreezeEvent {
                        start,
                        end: Some(seconds(prev.pts, tb)),
                    });
                }
                self.run_start_secs = None;
                self.confirmed = false;
            }
        }
        self.prev = Some(frame.clone());
        FrameOut::One(frame)
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        let tb = match ctx.input_link(0) {
            Some(vaco_filter_core::LinkFormat::Video { time_base, .. }) => *time_base,
            _ => frame.time_base,
        };
        Ok(self.step(frame, tb))
    }

    fn flush_state(&mut self) {
        self.prev = None;
        self.run_start_secs = None;
        self.confirmed = false;
        self.events.clear();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let noise = str_opt(req, "noise")
        .or_else(|| str_opt(req, "n"))
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.001)
        .clamp(0.0, 1.0);
    let duration_secs = str_opt(req, "duration")
        .or_else(|| str_opt(req, "d"))
        .and_then(|v| v.trim_end_matches('s').parse::<f64>().ok())
        .unwrap_or(2.0);
    let opts = Options { noise, duration_secs };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(opts))),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_pixfmt::PixFmt;

    fn frame_at(value: u8, pts: i64, tb: Rational) -> Frame {
        let pool = vaco_frame::FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 4, 4).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            p.fill(value);
        }
        f.pts = Timestamp::new(pts);
        f.time_base = tb;
        f
    }

    #[test]
    fn a_genuinely_frozen_stream_fires() {
        let tb = Rational::new(1, 10); // 10 fps, 0.1s per frame
        let opts = Options {
            noise: 0.001,
            duration_secs: 0.5,
        };
        let mut f = Filter::new(opts);
        // 10 identical frames spans 0.9s of "no change" (last pts 9 * 0.1s),
        // comfortably over the 0.5s duration threshold.
        for n in 0..10i64 {
            let _ = f.step(frame_at(128, n, tb), tb);
        }
        let events = f.events();
        assert!(!events.is_empty(), "a static stream must report a freeze");
        assert!(events[0].start < 0.5);
    }

    #[test]
    fn a_moving_stream_never_fires() {
        let tb = Rational::new(1, 10);
        let opts = Options {
            noise: 0.001,
            duration_secs: 0.5,
        };
        let mut f = Filter::new(opts);
        for n in 0..10i64 {
            #[allow(clippy::cast_possible_truncation, reason = "n is 0..10, well within u8")]
            let v = (n * 20) as u8;
            let _ = f.step(frame_at(v, n, tb), tb);
        }
        assert!(f.events().is_empty(), "a changing stream must not report a freeze");
    }

    #[test]
    fn a_freeze_shorter_than_duration_never_confirms() {
        let tb = Rational::new(1, 10);
        let opts = Options {
            noise: 0.001,
            duration_secs: 5.0, // much longer than the whole test stream
        };
        let mut f = Filter::new(opts);
        for n in 0..10i64 {
            let _ = f.step(frame_at(128, n, tb), tb);
        }
        assert!(f.events().is_empty());
    }
}
