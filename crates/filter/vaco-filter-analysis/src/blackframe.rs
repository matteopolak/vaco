//! `blackframe` — detect frames that are (almost) completely black.
//!
//! `ffmpeg -h filter=blackframe`: one video pad in, one out.
//! `man ffmpeg-filters`, quoted verbatim: *"This filter exports frame
//! metadata `lavfi.blackframe.pblack`. The value represents the percentage
//! of pixels in the picture that are below the threshold value."*
//! `amount` (default `98`) and `threshold`/`thresh` (default `32`, the
//! maximum luma for a pixel to count as "black").
//!
//! # Metadata export, measured against `ffmpeg 8.1`
//!
//! ```text
//! $ ffprobe -of json -show_frames -f lavfi -i "color=black:s=32x32,blackframe"
//! "tags": { "lavfi.blackframe.pblack": "100" }
//! ```
//!
//! `pblack` is an **integer percentage** (`"100"`, not `"100.000000"` or
//! `"100.0"`) — measured to be a plain `%d`-style value, not
//! [`crate::fmt::fixed6`] or [`crate::fmt::g6`] (both of which would print
//! extra digits `blackframe` never does). Unlike `blackdetect`, `blackframe`
//! exports **every frame's** `pblack` unconditionally — there is no
//! "nothing to report" case for this filter, since every frame has *some*
//! percentage of black pixels, including `0`. `amount` gates nothing about
//! the metadata either (it is documented as the reference's own console-log
//! threshold, which this crate does not reproduce — no log output exists in
//! this workspace's filter model).
//!
//! # Distinguishing input built for this filter
//!
//! `amount`/`threshold` interact multiplicatively (a pixel counts if
//! `<= threshold`; the picture counts as a log-worthy "black frame" if
//! `>= amount` percent of pixels qualify) — but since the metadata itself
//! is unconditional, the property to check is simpler and sharper: `pblack`
//! must be the **exact integer percentage**, not a rounded approximation
//! that happens to agree at `0` and `100`. A frame where exactly one third
//! of the pixels are black (`33.33...%`) distinguishes floor/round/ceil —
//! measured against `ffmpeg 8.1`, which floors (`33`, not `33.33` or `34`).

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{VIDEO_PAD, u8_opt};

pub const DESC: FilterDesc = FilterDesc {
    name: "blackframe",
    description: "Detect frames that are (almost) black.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy)]
pub(crate) struct Options {
    threshold: u8,
}

impl Default for Options {
    fn default() -> Self {
        Self { threshold: 32 }
    }
}

fn pblack(frame: &Frame, threshold: u8) -> Option<u64> {
    let plane = frame.plane(0)?;
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
        return None;
    }
    // Measured: floors, does not round. `black*100/total` in integer
    // arithmetic is exactly that floor.
    #[allow(
        clippy::integer_division,
        reason = "the reference is measured to floor this percentage, not round it; \
                  integer division is the intended operation, not an oversight"
    )]
    let percentage = black.saturating_mul(100) / total;
    Some(percentage)
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
        if let Some(percentage) = pblack(&frame, self.opts.threshold) {
            frame.set_metadata("lavfi.blackframe.pblack", percentage.to_string());
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
    let threshold = u8_opt(req, "threshold", u8_opt(req, "thresh", 32));
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(Options { threshold }))),
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
        let mut f = pool.acquire_video(PixFmt::Gray8, w, h).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            p.fill(value);
        }
        f
    }

    /// Independent oracle: an all-black synthetic frame must score 100%; a
    /// mid-grey one must score 0%.
    #[test]
    fn all_black_is_100_mid_grey_is_0() {
        let mut filt = Filter::new(Options::default());
        let out = filt.step(gray_frame(0, 8, 8));
        assert_eq!(out.metadata_get("lavfi.blackframe.pblack"), Some("100"));

        let mut filt = Filter::new(Options::default());
        let out = filt.step(gray_frame(128, 8, 8));
        assert_eq!(out.metadata_get("lavfi.blackframe.pblack"), Some("0"));
    }

    /// Distinguishing input: exactly one third of the pixels black
    /// (`33.33...%`) tells floor apart from round (`33` vs `33`, agrees —
    /// so also check a case round and floor disagree on: `2/3`, which
    /// floors to `66`, not `67`).
    #[test]
    fn percentage_floors_rather_than_rounds() {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 3, 1).unwrap();
        if let Some(mut p) = f.plane_mut(0)
            && let Some(row) = p.row_mut(0)
        {
            row[0] = 0;
            row[1] = 0;
            row[2] = 255;
        }
        let mut filt = Filter::new(Options::default());
        let out = filt.step(f);
        // 2 of 3 pixels black = 66.67%, floors to 66 (round would give 67).
        assert_eq!(out.metadata_get("lavfi.blackframe.pblack"), Some("66"));
    }
}
