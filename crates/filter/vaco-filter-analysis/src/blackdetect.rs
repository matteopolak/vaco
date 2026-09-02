//! `blackdetect` — detect intervals of (almost) black video.
//!
//! `ffmpeg -h filter=blackdetect`: one video pad in, one out.
//! `man ffmpeg-filters`' documented options (quoted verbatim, an interface
//! fact D7 allows using):
//!
//! * `black_min_duration`/`d` — minimum black run length in seconds
//!   (default `2.0`); affects only the log line, not the metadata (see
//!   below).
//! * `picture_black_ratio_th`/`pic_th` — minimum `nb_black_pixels /
//!   nb_pixels` ratio for the picture to count as "black" (default `0.98`).
//! * `pixel_black_th`/`pix_th` — a pixel counts as "black" when its luma is
//!   at most `luma_minimum_value + pixel_black_th * luma_range_size`, where
//!   the range is `[0,255]` for full-range formats and `[16,235]` otherwise
//!   (default `0.10`). This crate only implements the full-range case
//!   (`[0,255]`); limited-range detection would need `vaco-color`'s range
//!   signalling threaded through, left for a future extension.
//!
//! # Metadata export, measured against `ffmpeg 8.1`
//!
//! *"The filter also attaches metadata to the first frame of a black
//! segment with key `lavfi.black_start` and to the first frame after the
//! black segment ends with key `lavfi.black_end`. The value is the frame's
//! timestamp. This metadata is added regardless of the minimum duration
//! specified."* (`man ffmpeg-filters`) — so, unlike `black_min_duration`
//! gating the log line, the tags fire on every black/non-black transition.
//! Confirmed against `ffprobe -show_frames` at 5 fps (`black_start` on
//! frame 0, `black_end` on frame 5, `t=1.0s`, for a 1s-black-then-1s-white
//! stream). The seconds-conversion formula ([`seconds`]) is the same one
//! `freezedetect` uses.
//!
//! # Distinguishing input
//!
//! A black run whose last black frame and breaking frame are not evenly
//! spaced (`pts` 0,1,2 then a gap to `pts` 10, rather than 3) tells "the tag
//! carries the breaking frame's own pts" apart from "the tag carries the
//! last black frame's pts" — indistinguishable at even spacing. The
//! reference's own docs say which one it is ("the first frame *after* the
//! black segment ends"), so this is a regression guard, not a blind
//! measurement.

use vaco_core::{MediaType, Rational, Result, Timestamp};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::fmt::trimmed_time;
use crate::video::{VIDEO_PAD, f64_opt};

pub const DESC: FilterDesc = FilterDesc {
    name: "blackdetect",
    description: "Detect video intervals that are (almost) black.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy)]
pub(crate) struct Options {
    picture_black_ratio_th: f64,
    pixel_black_th: f64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            picture_black_ratio_th: 0.98,
            pixel_black_th: 0.10,
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

/// Whether this frame's luma plane counts as "black" under `opts`:
/// full-range `[0,255]` only (see this module's doc for the scope note).
fn frame_is_black(frame: &Frame, opts: Options) -> bool {
    let Some(plane) = frame.plane(0) else {
        return false;
    };
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "pixel_black_th is documented 0..=1, threshold fits in u8"
    )]
    let threshold = (opts.pixel_black_th * 255.0).round() as u8;
    let mut black: u64 = 0;
    let mut total: u64 = 0;
    for y in 0..plane.rows() {
        let Some(row) = plane.row(y) else { continue };
        for &sample in row {
            total += 1;
            if sample <= threshold {
                black += 1;
            }
        }
    }
    if total == 0 {
        return false;
    }
    #[allow(clippy::cast_precision_loss, reason = "sample counts are frame-sized")]
    let ratio = black as f64 / total as f64;
    ratio >= opts.picture_black_ratio_th
}

#[derive(Debug)]
pub(crate) struct Filter {
    opts: Options,
    was_black: bool,
}

impl Filter {
    pub(crate) fn new(opts: Options) -> Self {
        Self {
            opts,
            was_black: false,
        }
    }

    fn step(&mut self, mut frame: Frame, tb: Rational) -> Frame {
        let now_black = frame_is_black(&frame, self.opts);
        if now_black && !self.was_black {
            frame.set_metadata("lavfi.black_start", trimmed_time(seconds(frame.pts, tb)));
        } else if !now_black && self.was_black {
            frame.set_metadata("lavfi.black_end", trimmed_time(seconds(frame.pts, tb)));
        }
        self.was_black = now_black;
        frame
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        let tb = match ctx.input_link(0) {
            Some(vaco_filter_core::LinkFormat::Video { time_base, .. }) => *time_base,
            _ => frame.time_base,
        };
        Ok(FrameOut::One(self.step(frame, tb)))
    }

    fn flush_state(&mut self) {
        self.was_black = false;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let picture_black_ratio_th =
        f64_opt(req, "picture_black_ratio_th", f64_opt(req, "pic_th", 0.98));
    let pixel_black_th = f64_opt(req, "pixel_black_th", f64_opt(req, "pix_th", 0.10));
    let opts = Options {
        picture_black_ratio_th,
        pixel_black_th,
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

    fn frame_at(value: u8, pts: i64, tb: Rational) -> Frame {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 4, 4).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            p.fill(value);
        }
        f.pts = Timestamp::new(pts);
        f.time_base = tb;
        f
    }

    /// Independent oracle: an all-black synthetic stream must fire
    /// (`black_start` on the first frame); a mid-grey one must not.
    #[test]
    fn all_black_fires_mid_grey_does_not() {
        let tb = Rational::new(1, 5);
        let mut black = Filter::new(Options::default());
        let out = black.step(frame_at(0, 0, tb), tb);
        assert_eq!(out.metadata_get("lavfi.black_start"), Some("0"));

        let mut grey = Filter::new(Options::default());
        let out = grey.step(frame_at(128, 0, tb), tb);
        assert!(out.metadata().is_empty());
    }

    /// Reproduces the documented reference transcript exactly: 5 black
    /// frames (0.0..0.8s) then 5 white frames (1.0..1.8s) at 5fps.
    /// `black_start` on frame 0, `black_end` on frame 5 (t=1.0s).
    #[test]
    fn transition_tags_land_on_the_documented_frames() {
        let tb = Rational::new(1, 5);
        let mut f = Filter::new(Options::default());
        let mut tagged = Vec::new();
        for n in 0..10i64 {
            let value = if n < 5 { 0 } else { 255 };
            let out = f.step(frame_at(value, n, tb), tb);
            if !out.metadata().is_empty() {
                tagged.push((n, out.metadata().to_vec()));
            }
        }
        assert_eq!(tagged.len(), 2);
        assert_eq!(
            tagged[0],
            (0, vec![("lavfi.black_start".to_owned(), "0".to_owned())])
        );
        assert_eq!(
            tagged[1],
            (5, vec![("lavfi.black_end".to_owned(), "1".to_owned())])
        );
    }

    /// Distinguishing input: an irregular gap between the last black frame
    /// (pts 2) and the frame that breaks the run (pts 10, not 3) tells "the
    /// tag carries the breaking frame's own pts" apart from "the tag
    /// carries the last black frame's pts" — the two agree whenever frames
    /// are evenly spaced. The reference's docs say which one it is ("the
    /// first frame after the black segment ends"), so this is a regression
    /// guard against reintroducing the wrong-neighbour bug.
    #[test]
    fn black_end_uses_the_breaking_frame_not_the_last_black_one() {
        let tb = Rational::new(1, 1);
        let mut f = Filter::new(Options::default());
        let _ = f.step(frame_at(0, 0, tb), tb);
        let _ = f.step(frame_at(0, 1, tb), tb);
        let _ = f.step(frame_at(0, 2, tb), tb);
        // The wrong-neighbour hypothesis would print "2" here (the last
        // black frame's pts); the frame that actually breaks the run has
        // pts 10.
        let out = f.step(frame_at(255, 10, tb), tb);
        assert_eq!(out.metadata_get("lavfi.black_end"), Some("10"));
    }

    #[test]
    fn frames_outside_a_transition_carry_no_metadata() {
        let tb = Rational::new(1, 5);
        let mut f = Filter::new(Options::default());
        let _ = f.step(frame_at(0, 0, tb), tb);
        let out = f.step(frame_at(0, 1, tb), tb);
        assert!(out.metadata().is_empty());
    }
}
