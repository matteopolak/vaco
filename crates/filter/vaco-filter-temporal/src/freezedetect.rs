//! `freezedetect` — a passthrough analysis filter that flags runs of frames
//! whose luma barely changes for at least `duration` seconds.
//!
//! `ffmpeg -h filter=freezedetect`: `n`/`noise` (`0..=1`, default `0.001`,
//! a fraction-of-full-scale mean-absolute-frame-difference tolerance) and
//! `d`/`duration` (a time spec, default `2` seconds).
//!
//! # Metadata export
//!
//! The reference reports freeze events as frame-attached dictionary entries —
//! `lavfi.freezedetect.freeze_start`, `.freeze_duration`, `.freeze_end` — and
//! log lines. Now that `vaco_frame::Frame` carries a metadata dictionary
//! (interface gap 11, closed), this filter writes the same three keys onto
//! the frame that carries the corresponding event, in the same order the
//! reference does (`freeze_start` alone on the confirming frame;
//! `freeze_duration` then `freeze_end`, together, on the frame that breaks
//! the run). [`Filter::events`] still exists as a plain accessor for tests —
//! it predates the metadata export and stayed because comparing a `Vec` is
//! easier in a unit test than parsing tags back out of a `Frame`.
//!
//! Value formatting is measured against `ffmpeg 8.1`, not guessed: each value
//! is seconds since stream start, printed with [`format_lavfi_time`] — six
//! decimal digits, then trailing zeros trimmed, then a bare trailing `.`
//! trimmed too, so an exact whole number prints as `0` rather than
//! `0.000000` while `1.001001` prints in full. The end/duration values use
//! the timestamp of the frame that *breaks* the freeze (the first frame that
//! differs again), not the last frozen frame — checked against the reference
//! at multiple frame rates including one (`24000/1001`) chosen specifically
//! to distinguish the two, since they agree whenever the frame spacing
//! divides evenly into the confirmation point.
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

/// The reference's `lavfi.freezedetect.*` value formatting: six decimal
/// digits, then trailing zeros trimmed, then a bare trailing `.` trimmed.
///
/// Measured against `ffmpeg 8.1`, at frame rates chosen so the value lands on
/// a whole number (`0` → `"0"`, not `"0.000000"`), on a value with trailing
/// zeros short of six digits (`1.001000` → `"1.001"`), and on a value using
/// the full six digits (`1.000001` stays `1.000001`).
fn format_lavfi_time(value: f64) -> String {
    let mut s = format!("{value:.6}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    s
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
        reason = "exercised by this module's tests only; production consumers read the \
                  same information from the frame's `lavfi.freezedetect.*` metadata \
                  instead, set alongside this in `step`"
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

    fn step(&mut self, mut frame: Frame, tb: Rational) -> FrameOut {
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
                    // Tagged on the *confirming* frame — the one being
                    // processed right now, not the one the freeze started
                    // on. Measured: the reference's `freeze_start` tag lands
                    // on `now`'s frame with `start`'s value.
                    frame.set_metadata("lavfi.freezedetect.freeze_start", format_lavfi_time(start));
                }
            } else {
                if self.confirmed
                    && let Some(start) = self.run_start_secs
                {
                    // `end` is `now` — the first frame that differs again —
                    // not the last frozen frame. The two agree whenever the
                    // frame spacing divides the confirmation point evenly,
                    // which is why a naive check against round-number frame
                    // rates alone would not have caught using the wrong one.
                    self.events.push(FreezeEvent { start, end: Some(now) });
                    frame.set_metadata("lavfi.freezedetect.freeze_duration", format_lavfi_time(now - start));
                    frame.set_metadata("lavfi.freezedetect.freeze_end", format_lavfi_time(now));
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

    fn out_frame(out: FrameOut) -> Frame {
        match out {
            FrameOut::One(f) => f,
            _ => panic!("freezedetect is 1:1, expected exactly one frame out"),
        }
    }

    /// `format_lavfi_time` against the reference's measured behaviour:
    /// `ffprobe -show_frames` on a `freezedetect` graph at 10 fps, at
    /// `29.97` (a value that keeps its full six digits), and at
    /// `24000/1001` (a value with trailing zeros to trim).
    #[test]
    fn value_formatting_matches_the_reference() {
        assert_eq!(format_lavfi_time(0.0), "0");
        assert_eq!(format_lavfi_time(1.0), "1");
        assert_eq!(format_lavfi_time(1.000_001), "1.000001");
        assert_eq!(format_lavfi_time(1.001), "1.001");
        assert_eq!(format_lavfi_time(0.333_333), "0.333333");
    }

    /// The confirming frame carries `freeze_start` alone; no other frame in
    /// the run does, matching `ffprobe -show_frames`'s tag placement (each
    /// tag on exactly the one frame the reference attaches it to, no
    /// re-emission on every subsequent frame of the same run).
    #[test]
    fn freeze_start_lands_on_the_confirming_frame_only() {
        let tb = Rational::new(1, 10); // 10 fps
        let opts = Options {
            noise: 0.001,
            duration_secs: 0.5,
        };
        let mut f = Filter::new(opts);
        let mut tagged_frames = Vec::new();
        for n in 0..10i64 {
            let out = out_frame(f.step(frame_at(128, n, tb), tb));
            if !out.metadata().is_empty() {
                tagged_frames.push((n, out.metadata().to_vec()));
            }
        }
        // Reference, measured: exactly frame index 5 (t=0.5s) carries the
        // tag, with the run's start time (t=0), not its own timestamp.
        assert_eq!(tagged_frames.len(), 1);
        assert_eq!(tagged_frames[0].0, 5);
        assert_eq!(
            tagged_frames[0].1,
            &[("lavfi.freezedetect.freeze_start".to_string(), "0".to_string())]
        );
    }

    /// `freeze_duration`/`freeze_end` use the timestamp of the frame that
    /// *breaks* the freeze, not the last frozen frame — the two are
    /// indistinguishable at a uniform frame rate, so this test uses an
    /// irregular one on purpose. A run of four frames at pts 0,1,2,3 (last
    /// similar frame at pts 3) is broken by a frame at pts 10: the reference
    /// (measured on `ffmpeg 8.1`, `planning/AGENT-CONSTRAINTS.md`'s
    /// `tblend`/256-vs-255 caution taken to heart) reports `end`/`duration`
    /// from pts 10, not pts 3.
    #[test]
    fn freeze_end_uses_the_breaking_frame_not_the_last_frozen_one() {
        let tb = Rational::new(1, 1);
        let opts = Options {
            noise: 0.001,
            duration_secs: 2.5,
        };
        let mut f = Filter::new(opts);
        let _ = f.step(frame_at(1, 0, tb), tb);
        let _ = f.step(frame_at(1, 1, tb), tb);
        let _ = f.step(frame_at(1, 2, tb), tb);
        let confirming = out_frame(f.step(frame_at(1, 3, tb), tb));
        assert_eq!(
            confirming.metadata(),
            &[("lavfi.freezedetect.freeze_start".to_string(), "0".to_string())]
        );
        let breaking = out_frame(f.step(frame_at(2, 10, tb), tb));
        // The wrong-neighbour hypothesis (end = last similar frame's pts = 3)
        // would print "3"/"3" here; the reference prints "10"/"10".
        assert_eq!(
            breaking.metadata(),
            &[
                ("lavfi.freezedetect.freeze_duration".to_string(), "10".to_string()),
                ("lavfi.freezedetect.freeze_end".to_string(), "10".to_string()),
            ]
        );
    }

    /// A frame with nothing to report carries no metadata entry at all — not
    /// an empty one (`AGENT-CONSTRAINTS.md`'s "empty collection at
    /// construction" trap).
    #[test]
    fn frames_outside_an_event_carry_no_metadata() {
        let tb = Rational::new(1, 10);
        let opts = Options {
            noise: 0.001,
            duration_secs: 5.0, // never confirms within this short stream
        };
        let mut f = Filter::new(opts);
        for n in 0..5i64 {
            let out = out_frame(f.step(frame_at(128, n, tb), tb));
            assert!(out.metadata().is_empty());
        }
    }

    /// A freeze still ongoing when the stream ends never gets an `end`/
    /// `duration` tag — matching the reference, which has no later frame to
    /// attach one to either.
    #[test]
    fn a_freeze_still_open_at_end_of_stream_never_gets_an_end_tag() {
        let tb = Rational::new(1, 10);
        let opts = Options {
            noise: 0.001,
            duration_secs: 0.5,
        };
        let mut f = Filter::new(opts);
        for n in 0..10i64 {
            let out = out_frame(f.step(frame_at(128, n, tb), tb));
            assert!(out.metadata_get("lavfi.freezedetect.freeze_duration").is_none());
            assert!(out.metadata_get("lavfi.freezedetect.freeze_end").is_none());
        }
        // Confirmed and still open per `events()`'s existing accessor.
        assert_eq!(f.events().last().map(|e| e.end), Some(None));
    }
}
