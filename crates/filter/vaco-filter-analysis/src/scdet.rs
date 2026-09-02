//! `scdet` — detect scene changes and export a per-frame change score.
//!
//! `ffmpeg -h filter=scdet`: one video pad in, one out. `threshold`/`t`
//! (`0..100`, default `10`), `sc_pass`/`s` (bool, default `false`, "pass
//! the flag to pass scene change frames" — quoted from `-h`, an interface
//! fact D7 allows using verbatim).
//!
//! # Measured: `mafd`, and the equal-suppression rule for `score`
//!
//! `ffprobe -show_frames` on `gray` inputs, `threshold=0`:
//!
//! * A flat black frame followed by `Y=30` (`0x64,0,0` through `format=gray`,
//!   BT.601 luma) measures `lavfi.scd.mafd=11.719`. `30 * 100 / 256 =
//!   11.71875`, which rounds to exactly that — so `mafd` is the **mean
//!   absolute luma difference from the previous frame, scaled to `0..100`
//!   by `/256` (not `/255`)**. The first frame of a stream has no
//!   predecessor and measures `mafd=0`.
//! * A third frame (`Y=15`) measures `mafd=5.859 = 15*100/256`, confirming
//!   the scale factor a second time on a different magnitude.
//! * `score` equals `mafd` on the first two of those three frames (`0`,
//!   then `11.719`) but a **second, independent** three-frame probe
//!   (`testsrc`, whose synthetic content happens to produce the *same*
//!   `mafd` twice in a row) measures `score=0` on the repeat while `mafd`
//!   stays `5.188`. Both probes together pin the rule as
//!   `score = if mafd == previous_mafd { 0 } else { mafd }`
//!   (exact float equality, not "decreased"): the gray probe's third frame
//!   has a **different**, lower `mafd` than its second and still measures
//!   `score = mafd` in full, ruling out "clamp the decrease at zero" —
//!   only a literal repeat suppresses the score.
//! * `lavfi.scd.time` is attached whenever `score >= threshold`, matching
//!   both probes' `threshold=0` (which fires on every frame including the
//!   `score=0` one, since `0 >= 0`).
//!
//! # Not independently measured
//!
//! `sc_pass` is implemented from the `-h` text alone (drop every frame
//! whose `score < threshold` when set), not from a differential probe — see
//! this module's own test for the exact behaviour implemented.

use vaco_core::{MediaType, Rational, Result, Timestamp};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{VIDEO_PAD, f64_opt};

pub const DESC: FilterDesc = FilterDesc {
    name: "scdet",
    description: "Detect video scene change",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy)]
pub(crate) struct Options {
    threshold: f64,
    sc_pass: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            threshold: 10.0,
            sc_pass: false,
        }
    }
}

fn seconds(pts: Timestamp, tb: Rational) -> f64 {
    let Some(ticks) = pts.ticks() else { return 0.0 };
    #[allow(
        clippy::cast_precision_loss,
        reason = "display-scale timestamp conversion"
    )]
    {
        ticks as f64 * f64::from(tb.num) / f64::from(tb.den.max(1))
    }
}

/// `%.3f`, unlike this crate's `fixed6`/`g6` — measured directly against
/// `lavfi.scd.mafd`/`lavfi.scd.score` (`"11.719"`, three digits, not six).
fn fixed3(value: f64) -> String {
    format!("{value:.3}")
}

#[derive(Debug)]
pub(crate) struct Filter {
    opts: Options,
    prev: Option<Frame>,
    prev_mafd: f64,
}

impl Filter {
    pub(crate) fn new(opts: Options) -> Self {
        Self {
            opts,
            prev: None,
            prev_mafd: 0.0,
        }
    }

    /// The mean absolute luma difference from the previous frame, scaled to
    /// `0..100` by `/256` — `0.0` for the first frame (no predecessor).
    fn mafd(&self, frame: &Frame) -> f64 {
        let (Some(cur), Some(prev)) = (frame.plane(0), self.prev.as_ref().and_then(|p| p.plane(0)))
        else {
            return 0.0;
        };
        let rows = cur.rows().min(prev.rows());
        let cols = (0..rows)
            .filter_map(|y| Some(cur.row(y)?.len().min(prev.row(y)?.len())))
            .min()
            .unwrap_or(0);
        let samples = (rows as u64).saturating_mul(cols as u64);
        if samples == 0 {
            return 0.0;
        }
        let sad = vaco_filter_vdsp::plane_sad(cur, prev);
        #[allow(clippy::cast_precision_loss, reason = "sad/samples are frame-sized")]
        let mean = sad as f64 / samples as f64;
        mean * 100.0 / 256.0
    }

    fn step(&mut self, mut frame: Frame, tb: Rational) -> Option<Frame> {
        let mafd = self.mafd(&frame);
        #[allow(
            clippy::float_cmp,
            reason = "the measured rule is exact repetition, not proximity"
        )]
        let score = if self.prev.is_none() || mafd == self.prev_mafd {
            0.0
        } else {
            mafd
        };
        frame.set_metadata("lavfi.scd.mafd", fixed3(mafd));
        frame.set_metadata("lavfi.scd.score", fixed3(score));
        let is_change = score >= self.opts.threshold;
        if is_change {
            frame.set_metadata("lavfi.scd.time", fixed3(seconds(frame.pts, tb)));
        }
        self.prev = Some(frame.clone());
        self.prev_mafd = mafd;
        if self.opts.sc_pass && !is_change {
            None
        } else {
            Some(frame)
        }
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        let tb = match ctx.input_link(0) {
            Some(vaco_filter_core::LinkFormat::Video { time_base, .. }) => *time_base,
            _ => frame.time_base,
        };
        Ok(self.step(frame, tb).map_or(FrameOut::None, FrameOut::One))
    }

    fn flush_state(&mut self) {
        self.prev = None;
        self.prev_mafd = 0.0;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let opts = Options {
        threshold: f64_opt(
            req,
            "threshold",
            f64_opt(req, "t", Options::default().threshold),
        ),
        sc_pass: req
            .named("sc_pass")
            .or_else(|| req.named("s"))
            .is_some_and(|v| matches!(v.trim(), "1" | "true")),
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(opts))),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_frame::FramePool;
    use vaco_pixfmt::PixFmt;

    fn gray_frame(value: u8, w: u32, h: u32) -> Frame {
        let pool = FramePool::default();
        let mut frame = pool.acquire_video(PixFmt::Gray8, w, h).unwrap();
        if let Some(mut p) = frame.plane_mut(0) {
            p.fill(value);
        }
        frame.pts = Timestamp::new(0);
        frame.time_base = Rational::new(1, 1);
        frame
    }

    #[test]
    fn first_frame_has_zero_mafd_and_score() {
        let mut f = Filter::new(Options::default());
        let out = f.step(gray_frame(30, 2, 2), Rational::new(1, 1)).unwrap();
        assert_eq!(out.metadata_get("lavfi.scd.mafd"), Some("0.000"));
        assert_eq!(out.metadata_get("lavfi.scd.score"), Some("0.000"));
    }

    /// Measured: `Y=0 -> Y=30` scales to `mafd = 30*100/256 = 11.71875`.
    #[test]
    fn mafd_matches_the_measured_scale_factor() {
        let mut f = Filter::new(Options {
            threshold: 0.0,
            sc_pass: false,
        });
        let _ = f.step(gray_frame(0, 2, 2), Rational::new(1, 1));
        let out = f.step(gray_frame(30, 2, 2), Rational::new(1, 1)).unwrap();
        assert_eq!(out.metadata_get("lavfi.scd.mafd"), Some("11.719"));
        assert_eq!(out.metadata_get("lavfi.scd.score"), Some("11.719"));
    }

    /// Measured: an exact repeat of the previous `mafd` suppresses `score`
    /// to `0`, even though `mafd` itself stays nonzero.
    #[test]
    fn a_repeated_mafd_suppresses_the_score() {
        let mut f = Filter::new(Options {
            threshold: 0.0,
            sc_pass: false,
        });
        let _ = f.step(gray_frame(0, 2, 2), Rational::new(1, 1));
        let _ = f.step(gray_frame(30, 2, 2), Rational::new(1, 1));
        // Y goes 30 -> 60, an identical step size, so mafd repeats exactly.
        let out = f.step(gray_frame(60, 2, 2), Rational::new(1, 1)).unwrap();
        assert_eq!(out.metadata_get("lavfi.scd.mafd"), Some("11.719"));
        assert_eq!(out.metadata_get("lavfi.scd.score"), Some("0.000"));
    }

    /// Measured: a *different* (here, smaller) `mafd` is reported in full,
    /// not clamped to zero — ruling out a "decrease clamps" hypothesis.
    #[test]
    fn a_different_lower_mafd_is_not_clamped_to_zero() {
        let mut f = Filter::new(Options {
            threshold: 0.0,
            sc_pass: false,
        });
        let _ = f.step(gray_frame(0, 2, 2), Rational::new(1, 1));
        let _ = f.step(gray_frame(30, 2, 2), Rational::new(1, 1));
        let out = f.step(gray_frame(15, 2, 2), Rational::new(1, 1)).unwrap();
        assert_eq!(out.metadata_get("lavfi.scd.mafd"), Some("5.859"));
        assert_eq!(out.metadata_get("lavfi.scd.score"), Some("5.859"));
    }

    #[test]
    fn sc_pass_drops_frames_below_threshold() {
        let mut f = Filter::new(Options {
            threshold: 50.0,
            sc_pass: true,
        });
        assert!(f.step(gray_frame(0, 2, 2), Rational::new(1, 1)).is_none());
        // A small step keeps score well under threshold=50 -> dropped.
        assert!(f.step(gray_frame(10, 2, 2), Rational::new(1, 1)).is_none());
    }
}
