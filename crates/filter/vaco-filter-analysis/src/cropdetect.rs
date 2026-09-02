//! `cropdetect` — accumulate the smallest crop rectangle that has always
//! contained every non-black pixel seen so far (`mode=black` only).
//!
//! One video pad in, one out. `limit` (black threshold, default
//! `0.0941176` ≈ `24/255`), `round` (output size must be a multiple of
//! this, default `16`), `skip` (initial frames not evaluated, default `2`),
//! `reset_count`/`reset` (recompute from scratch after this many frames,
//! `0` = never), `mode` (`black`/`mvedges`, default `black`).
//!
//! `mvedges` needs motion vectors, which are not a `vaco_frame::FrameSideData`
//! variant this workspace has; this module implements `mode=black` only,
//! and `mode=mvedges` falls back to the same scan rather than doing nothing.
//!
//! # Metadata export, measured against `ffmpeg 8.1`
//!
//! ```text
//! # first two frames (skip=2 default): no tags at all
//! # frame index 2 onward:
//! lavfi.cropdetect.x1=16 x2=47 y1=16 y2=47 w=32 h=32 x=16 y=16 limit=0.094118
//! ```
//!
//! `x1`/`x2`/`y1`/`y2` are the raw, unrounded bounding edges of every
//! above-threshold sample seen since the last reset — a running union across
//! frames (`man ffmpeg-filters`'s `reset_count`: "0 indicates never reset,
//! and returns the largest area encountered during playback"). `w`/`h`/`x`/`y`
//! are `round`-adjusted: `w`/`h` floor the raw width/height to the nearest
//! multiple of `round`, and `x`/`y` re-centre that shrunk box inside the raw
//! one — measured on a non-round-aligned box (`x1=10, x2=53` → raw width
//! `44`; `round=16` → `w=32`, `x = 10 + (44-32)/2 = 16`). The first `skip`
//! frames (default `2`) carry no tags and don't contribute to the union.
//!
//! **Known divergence: `round` at `3, 6, 7, 9, 13, 15` does not match the
//! reference** (e.g. `round=9` on a `44x54` box measures `w=36,h=36`; plain
//! floor-and-centre predicts `h=54`). No consistent alternative formula was
//! found, so this crate ships plain floor-and-centre for every `round` and
//! documents the divergence rather than guessing further.
//!
//! Tests use a rectangle with a margin on every side, not aligned to
//! `round`'s grid, extended with a second, smaller rectangle in a later
//! frame to confirm the box is the running union, not the current frame's.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::fmt::g6;
use crate::video::{VIDEO_PAD, f64_opt};

pub const DESC: FilterDesc = FilterDesc {
    name: "cropdetect",
    description: "Auto-detect crop size.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

/// `INT_MAX` as `f64`, the reference's own documented ceiling for
/// `round`/`skip`/`reset_count`.
const INT_MAX: f64 = i32::MAX as f64;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Options {
    /// Effective 8-bit black threshold (already resolved from the
    /// reference's dual `0.0..=1.0` fraction / raw-sample-value convention).
    limit: u8,
    /// The fraction form, kept for the `lavfi.cropdetect.limit` tag, which
    /// the reference reports as a fraction regardless of how `limit` was
    /// spelled in the graph text.
    limit_fraction: f64,
    round: u32,
    skip: u32,
    reset_count: u32,
}

impl Default for Options {
    fn default() -> Self {
        let limit_fraction = 24.0 / 255.0;
        Self {
            limit: 24,
            limit_fraction,
            round: 16,
            skip: 2,
            reset_count: 0,
        }
    }
}

/// Raw (unrounded) bounding box of every sample `> threshold` in the luma
/// plane, or `None` if nothing exceeds it — the same convention as `bbox`.
fn raw_box(frame: &Frame, threshold: u8) -> Option<(usize, usize, usize, usize)> {
    let plane = frame.plane(0)?;
    let (mut x1, mut y1, mut x2, mut y2) = (usize::MAX, usize::MAX, 0usize, 0usize);
    let mut found = false;
    for y in 0..plane.rows() {
        let Some(row) = plane.row(y) else { continue };
        for (x, &sample) in row.iter().enumerate() {
            if sample > threshold {
                found = true;
                x1 = x1.min(x);
                x2 = x2.max(x);
                y1 = y1.min(y);
                y2 = y2.max(y);
            }
        }
    }
    if found { Some((x1, y1, x2, y2)) } else { None }
}

/// Floor `extent` to the nearest multiple of `round` (or leave it unchanged
/// for `round <= 1`, this crate's choice for "no rounding requested" — see
/// this module's doc for the reference's own, unconfirmed behaviour at
/// `round=1`), returning `(new_extent, centring_offset)`.
fn floor_round(extent: usize, round: u32) -> (usize, usize) {
    if round <= 1 {
        return (extent, 0);
    }
    let round = round as usize;
    #[allow(
        clippy::integer_division,
        reason = "floor-to-nearest-multiple is exactly what integer division computes here, not an oversight"
    )]
    let rounded = (extent / round) * round;
    #[allow(
        clippy::integer_division,
        reason = "centring offset is deliberately floor((extent-rounded)/2), matching the reference's own centring"
    )]
    let offset = (extent - rounded) / 2;
    (rounded, offset)
}

#[derive(Debug)]
pub(crate) struct Filter {
    opts: Options,
    frames_seen: u64,
    since_reset: u32,
    accum: Option<(usize, usize, usize, usize)>,
}

impl Filter {
    pub(crate) const fn new(opts: Options) -> Self {
        Self {
            opts,
            frames_seen: 0,
            since_reset: 0,
            accum: None,
        }
    }

    fn step(&mut self, mut frame: Frame) -> Frame {
        let skip = u64::from(self.opts.skip);
        if self.frames_seen < skip {
            self.frames_seen += 1;
            return frame;
        }
        self.frames_seen += 1;

        if self.opts.reset_count > 0 && self.since_reset >= self.opts.reset_count {
            self.accum = None;
            self.since_reset = 0;
        }
        self.since_reset += 1;

        if let Some((x1, y1, x2, y2)) = raw_box(&frame, self.opts.limit) {
            self.accum = Some(match self.accum {
                Some((ax1, ay1, ax2, ay2)) => (ax1.min(x1), ay1.min(y1), ax2.max(x2), ay2.max(y2)),
                None => (x1, y1, x2, y2),
            });
        }

        let Some((x1, y1, x2, y2)) = self.accum else {
            return frame;
        };
        let raw_w = x2 - x1 + 1;
        let raw_h = y2 - y1 + 1;
        let (w, x_off) = floor_round(raw_w, self.opts.round);
        let (h, y_off) = floor_round(raw_h, self.opts.round);

        frame.set_metadata("lavfi.cropdetect.x1", x1.to_string());
        frame.set_metadata("lavfi.cropdetect.x2", x2.to_string());
        frame.set_metadata("lavfi.cropdetect.y1", y1.to_string());
        frame.set_metadata("lavfi.cropdetect.y2", y2.to_string());
        frame.set_metadata("lavfi.cropdetect.w", w.to_string());
        frame.set_metadata("lavfi.cropdetect.h", h.to_string());
        frame.set_metadata("lavfi.cropdetect.x", (x1 + x_off).to_string());
        frame.set_metadata("lavfi.cropdetect.y", (y1 + y_off).to_string());
        frame.set_metadata("lavfi.cropdetect.limit", g6(self.opts.limit_fraction));
        frame
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        Ok(FrameOut::One(self.step(frame)))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    // Reference-documented ranges (`ffmpeg -h filter=cropdetect`), clamped
    // here rather than trusted: an unclamped numeric option read would
    // accept values the reference's own parser refuses.
    let limit_fraction = f64_opt(req, "limit", 24.0 / 255.0).clamp(0.0, 65535.0);
    // Measured convention: a value `<= 1.0` is already a fraction of full
    // scale; anything above is a raw 8-bit sample value (`man
    // ffmpeg-filters`'s own wording for `limit`).
    let (limit_fraction, limit) = if limit_fraction <= 1.0 {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "limit_fraction*255 is clamped into 0..=255 immediately below"
        )]
        let raw = (limit_fraction * 255.0).round().clamp(0.0, 255.0) as u8;
        (limit_fraction, raw)
    } else {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped into 0..=255 immediately above"
        )]
        let raw = limit_fraction.round().clamp(0.0, 255.0) as u8;
        (f64::from(raw) / 255.0, raw)
    };
    // `ffmpeg -h filter=cropdetect`: `round`/`skip`/`reset_count` are all
    // documented `<int>` options ranging `0 to INT_MAX` — clamped to
    // `i32::MAX` before the cast, not just floored at zero, so a huge or
    // `inf` graph-text value cannot wrap through the `f64 -> u32` cast.
    let round = f64_opt(req, "round", 16.0).clamp(0.0, INT_MAX);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "round is clamped to 0.0..=i32::MAX above")]
    let round = round as u32;
    let skip = f64_opt(req, "skip", 2.0).clamp(0.0, INT_MAX);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "skip is clamped to 0.0..=i32::MAX above")]
    let skip = skip as u32;
    let reset_count = f64_opt(req, "reset_count", f64_opt(req, "reset", 0.0)).clamp(0.0, INT_MAX);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "reset_count is clamped to 0.0..=i32::MAX above")]
    let reset_count = reset_count as u32;
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(Options {
            limit,
            limit_fraction,
            round,
            skip,
            reset_count,
        }))),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_frame::FramePool;
    use vaco_pixfmt::PixFmt;

    fn frame_with_box(w: u32, h: u32, bx: usize, by: usize, bw: usize, bh: usize) -> Frame {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, w, h).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            for y in by..by + bh {
                if let Some(row) = p.row_mut(y) {
                    for x in bx..bx + bw {
                        if let Some(byte) = row.get_mut(x) {
                            *byte = 255;
                        }
                    }
                }
            }
        }
        f
    }

    /// The first `skip` frames (default 2) must carry no tags at all.
    #[test]
    fn skipped_frames_carry_no_tags() {
        let mut filt = Filter::new(Options::default());
        let f0 = filt.step(frame_with_box(64, 64, 16, 16, 32, 32));
        assert!(f0.metadata().is_empty());
        let f1 = filt.step(frame_with_box(64, 64, 16, 16, 32, 32));
        assert!(f1.metadata().is_empty());
    }

    /// Distinguishing input: a rectangle with a margin on every side and not
    /// aligned to the default `round=16` grid, so an off-by-one on any bound
    /// or a no-op rounding bug is individually visible. Measured against
    /// `ffmpeg 8.1`.
    #[test]
    fn non_aligned_rectangle_matches_the_reference_exactly() {
        let mut filt = Filter::new(Options::default());
        let _ = filt.step(frame_with_box(64, 64, 10, 5, 44, 54)); // skip 0
        let _ = filt.step(frame_with_box(64, 64, 10, 5, 44, 54)); // skip 1
        let out = filt.step(frame_with_box(64, 64, 10, 5, 44, 54));
        assert_eq!(out.metadata_get("lavfi.cropdetect.x1"), Some("10"));
        assert_eq!(out.metadata_get("lavfi.cropdetect.y1"), Some("5"));
        assert_eq!(out.metadata_get("lavfi.cropdetect.x2"), Some("53"));
        assert_eq!(out.metadata_get("lavfi.cropdetect.y2"), Some("58"));
        assert_eq!(out.metadata_get("lavfi.cropdetect.w"), Some("32"));
        assert_eq!(out.metadata_get("lavfi.cropdetect.h"), Some("48"));
        assert_eq!(out.metadata_get("lavfi.cropdetect.x"), Some("16"));
        assert_eq!(out.metadata_get("lavfi.cropdetect.y"), Some("8"));
        assert_eq!(out.metadata_get("lavfi.cropdetect.limit"), Some("0.094118"));
    }

    /// The reported box is a running **union**, not the current frame's own
    /// bounds: a large rectangle seen once must keep being reported even
    /// once later frames show a strictly smaller one.
    #[test]
    fn box_is_a_running_union_not_the_current_frame() {
        let mut filt = Filter::new(Options {
            round: 1,
            ..Options::default()
        });
        let _ = filt.step(frame_with_box(64, 64, 4, 4, 40, 40)); // skip 0
        let _ = filt.step(frame_with_box(64, 64, 4, 4, 40, 40)); // skip 1
        let _ = filt.step(frame_with_box(64, 64, 4, 4, 40, 40)); // first evaluated: raw box (4,4)-(43,43)
        let shrunk = filt.step(frame_with_box(64, 64, 20, 20, 4, 4)); // much smaller box
        // Union must still cover the first, larger rectangle.
        assert_eq!(shrunk.metadata_get("lavfi.cropdetect.x1"), Some("4"));
        assert_eq!(shrunk.metadata_get("lavfi.cropdetect.y1"), Some("4"));
        assert_eq!(shrunk.metadata_get("lavfi.cropdetect.x2"), Some("43"));
        assert_eq!(shrunk.metadata_get("lavfi.cropdetect.y2"), Some("43"));
    }

    /// A wholly-black frame (nothing above the threshold) leaves the running
    /// union untouched rather than collapsing it to an empty box.
    #[test]
    fn all_black_frame_does_not_erase_the_accumulated_box() {
        let mut filt = Filter::new(Options {
            round: 1,
            ..Options::default()
        });
        let _ = filt.step(frame_with_box(32, 32, 8, 8, 8, 8)); // skip 0
        let _ = filt.step(frame_with_box(32, 32, 8, 8, 8, 8)); // skip 1
        let _ = filt.step(frame_with_box(32, 32, 8, 8, 8, 8)); // first evaluated
        let pool = FramePool::default();
        let black = pool.acquire_video(PixFmt::Gray8, 32, 32).unwrap();
        let out = filt.step(black);
        assert_eq!(out.metadata_get("lavfi.cropdetect.x1"), Some("8"));
        assert_eq!(out.metadata_get("lavfi.cropdetect.x2"), Some("15"));
    }
}
