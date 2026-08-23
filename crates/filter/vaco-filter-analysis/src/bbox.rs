//! `bbox` — bounding box of the non-black pixels in the luma plane.
//!
//! `ffmpeg -h filter=bbox`: one video pad in, one out. `man ffmpeg-filters`:
//! *"This filter computes the bounding box containing all the pixels with a
//! luma value greater than the minimum allowed value [`min_val`, default
//! `16`]."* Reference behaviour is documented as printing to the log; this
//! crate measured the same behaviour via the metadata channel instead
//! (interface gap 11's whole point) — see below.
//!
//! # Metadata export, measured against `ffmpeg 8.1`
//!
//! ```text
//! $ ffprobe -of json -show_frames -f lavfi -i "movie=x.png,bbox"   # x.png: black bg, white 20x16 box at (10,8)
//! "tags": {
//!     "lavfi.bbox.x1": "10", "lavfi.bbox.x2": "29",
//!     "lavfi.bbox.y1": "8",  "lavfi.bbox.y2": "23",
//!     "lavfi.bbox.w":  "20", "lavfi.bbox.h":  "16"
//! }
//! ```
//!
//! `x2`/`y2` are **inclusive** (the last lit column/row, not one past it —
//! `x1=10, w=20` gives `x2=29`, not `30`). A frame with **no** pixel above
//! `min_val` carries no tags at all (measured: an all-black frame through
//! `bbox` with the default `min_val=16` produces an empty tag block, the
//! same "nothing to report, no tags" convention `freezedetect` established).
//!
//! # Distinguishing input built for this filter
//!
//! The brief's own invariant ("bbox of a synthetic rectangle is that
//! rectangle") is exactly what is checked, plus the boundary case a
//! single-pixel-off implementation would fail: a rectangle placed with a
//! **non-zero margin on every side** (not touching any edge), so an
//! off-by-one in any of the four bounds is individually visible rather than
//! being masked by an edge coinciding with the frame boundary.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{VIDEO_PAD, u8_opt};

pub const DESC: FilterDesc = FilterDesc {
    name: "bbox",
    description: "Compute bounding box for each frame.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy)]
pub(crate) struct Options {
    min_val: u8,
}

impl Default for Options {
    fn default() -> Self {
        Self { min_val: 16 }
    }
}

/// `(x1, y1, x2, y2)`, inclusive on all sides, or `None` if no sample
/// exceeds `min_val`.
fn bounding_box(frame: &Frame, min_val: u8) -> Option<(usize, usize, usize, usize)> {
    let plane = frame.plane(0)?;
    let (mut x1, mut y1, mut x2, mut y2) = (usize::MAX, usize::MAX, 0usize, 0usize);
    let mut found = false;
    for y in 0..plane.rows() {
        let Some(row) = plane.row(y) else { continue };
        for (x, &sample) in row.iter().enumerate() {
            if sample > min_val {
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

#[derive(Debug)]
pub(crate) struct Filter {
    opts: Options,
}

impl Filter {
    pub(crate) const fn new(opts: Options) -> Self {
        Self { opts }
    }

    fn step(&mut self, mut frame: Frame) -> Frame {
        if let Some((x1, y1, x2, y2)) = bounding_box(&frame, self.opts.min_val) {
            frame.set_metadata("lavfi.bbox.x1", x1.to_string());
            frame.set_metadata("lavfi.bbox.x2", x2.to_string());
            frame.set_metadata("lavfi.bbox.y1", y1.to_string());
            frame.set_metadata("lavfi.bbox.y2", y2.to_string());
            frame.set_metadata("lavfi.bbox.w", (x2 - x1 + 1).to_string());
            frame.set_metadata("lavfi.bbox.h", (y2 - y1 + 1).to_string());
        }
        frame
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        Ok(FrameOut::One(self.step(frame)))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let min_val = u8_opt(req, "min_val", 16);
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(Options { min_val }))),
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
                    for x in bx..bw + bx {
                        if let Some(byte) = row.get_mut(x) {
                            *byte = 255;
                        }
                    }
                }
            }
        }
        f
    }

    /// The brief's own invariant: bbox of a synthetic rectangle is that
    /// rectangle. Placed with a margin on every side so an off-by-one on
    /// any of the four edges is individually visible rather than masked by
    /// coinciding with the frame boundary.
    #[test]
    fn bbox_of_a_rectangle_is_that_rectangle() {
        let f = frame_with_box(64, 48, 10, 8, 20, 16);
        let mut filt = Filter::new(Options::default());
        let out = filt.step(f);
        assert_eq!(out.metadata_get("lavfi.bbox.x1"), Some("10"));
        assert_eq!(out.metadata_get("lavfi.bbox.y1"), Some("8"));
        assert_eq!(out.metadata_get("lavfi.bbox.x2"), Some("29"));
        assert_eq!(out.metadata_get("lavfi.bbox.y2"), Some("23"));
        assert_eq!(out.metadata_get("lavfi.bbox.w"), Some("20"));
        assert_eq!(out.metadata_get("lavfi.bbox.h"), Some("16"));
    }

    /// An all-black frame (nothing above `min_val`) carries no tags at all,
    /// not an empty/zeroed bbox.
    #[test]
    fn all_black_frame_carries_no_tags() {
        let pool = FramePool::default();
        let f = pool.acquire_video(PixFmt::Gray8, 16, 16).unwrap();
        let mut filt = Filter::new(Options::default());
        let out = filt.step(f);
        assert!(out.metadata().is_empty());
    }

    /// A single lit pixel is a valid (degenerate, 1x1) bounding box —
    /// distinguishes an inclusive-bounds implementation from one that
    /// silently requires at least two lit pixels.
    #[test]
    fn single_pixel_is_a_1x1_box() {
        let f = frame_with_box(16, 16, 5, 5, 1, 1);
        let mut filt = Filter::new(Options::default());
        let out = filt.step(f);
        assert_eq!(out.metadata_get("lavfi.bbox.x1"), Some("5"));
        assert_eq!(out.metadata_get("lavfi.bbox.x2"), Some("5"));
        assert_eq!(out.metadata_get("lavfi.bbox.w"), Some("1"));
        assert_eq!(out.metadata_get("lavfi.bbox.h"), Some("1"));
    }

    /// A sample exactly equal to `min_val` must NOT count as "above" it —
    /// the brief says "greater than", not "greater than or equal to".
    /// Distinguishes `sample > min_val` from `sample >= min_val`: with the
    /// correct strict `>`, a frame whose only non-zero sample sits exactly
    /// at `min_val` has nothing above threshold, so it must carry no tags
    /// at all (the same "nothing to report" convention as an all-black
    /// frame) — an off-by-one `>=` would instead report a spurious 1x1 box.
    #[test]
    fn pixel_exactly_at_min_val_is_excluded() {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 16, 16).unwrap();
        if let Some(mut p) = f.plane_mut(0)
            && let Some(row) = p.row_mut(5)
            && let Some(byte) = row.get_mut(5)
        {
            *byte = Options::default().min_val;
        }
        let mut filt = Filter::new(Options::default());
        let out = filt.step(f);
        assert!(out.metadata().is_empty());
    }
}
