//! `vmafmotion` — the VMAF temporal-motion feature.
//!
//! One video pad in, one out. The VMAF feature definition calls this the
//! average absolute pixel difference of adjacent luminance frames. The first
//! frame has no predecessor and therefore scores zero. `stats_file`/`f` are
//! accepted for graph compatibility but deliberately have no effect: this
//! framework has no filter-owned file-output channel, while the per-frame
//! metadata is the observable result.
//!
//! # Reference probes
//!
//! `ffprobe -show_frames` through `vmafmotion` confirms the direct mean, not
//! a normalised score: uniform luma transitions of `6`, `14`, `84`, and `184`
//! yield exactly `6.00`, `14.00`, `84.00`, and `184.00`. A `64x64` frame whose
//! left half alone changes by `84` yields `42.00`, ruling out peak difference
//! and an unweighted per-row average. These probes also confirm that each
//! frame compares only to its predecessor: `16 -> 100 -> 40 -> 16` yields
//! `0.00, 84.00, 60.00, 24.00`.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::{Frame, PlaneRef};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::VIDEO_PAD;

pub const DESC: FilterDesc = FilterDesc {
    name: "vmafmotion",
    description: "Calculate the VMAF Motion score.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

/// Mean absolute difference over the common luma rectangle. The VMAF motion
/// score is expressed in sample values, not divided by the 8-bit peak.
fn luma_mean_abs_difference(current: &Frame, previous: &Frame) -> Option<f64> {
    let (current, previous) = (current.plane(0)?, previous.plane(0)?);
    mean_abs_difference(current, previous)
}

fn mean_abs_difference(current: PlaneRef<'_>, previous: PlaneRef<'_>) -> Option<f64> {
    let rows = current.rows().min(previous.rows());
    let mut samples: u64 = 0;
    for y in 0..rows {
        let (Some(current_row), Some(previous_row)) = (current.row(y), previous.row(y)) else {
            continue;
        };
        let cols = current_row.len().min(previous_row.len());
        samples = samples.saturating_add(u64::try_from(cols).ok()?);
    }
    if samples == 0 {
        return None;
    }
    let total = vaco_filter_vdsp::plane_sad(current, previous);
    #[allow(
        clippy::cast_precision_loss,
        reason = "frame-sized sums are display-scale values"
    )]
    Some(total as f64 / samples as f64)
}

/// `vmafmotion` writes two fractional digits, unlike this crate's six-digit
/// measurement filters.
fn fixed2(value: f64) -> String {
    format!("{value:.2}")
}

#[derive(Debug, Default)]
pub(crate) struct Filter {
    previous: Option<Frame>,
}

impl Filter {
    fn step(&mut self, mut frame: Frame) -> Frame {
        let score = self
            .previous
            .as_ref()
            .and_then(|previous| luma_mean_abs_difference(&frame, previous))
            .unwrap_or(0.0);
        frame.set_metadata("lavfi.vmafmotion.score", fixed2(score));
        self.previous = Some(frame.clone());
        frame
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        Ok(FrameOut::One(self.step(frame)))
    }

    fn flush_state(&mut self) {
        self.previous = None;
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
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_frame::FramePool;
    use vaco_pixfmt::PixFmt;

    fn gray_frame(value: u8, w: u32, h: u32) -> Frame {
        let pool = FramePool::default();
        let mut frame = pool.acquire_video(PixFmt::Gray8, w, h).unwrap();
        if let Some(mut plane) = frame.plane_mut(0) {
            plane.fill(value);
        }
        frame
    }

    #[test]
    fn first_frame_is_zero_and_uniform_delta_is_unscaled() {
        let mut filter = Filter::default();
        let first = filter.step(gray_frame(16, 8, 8));
        assert_eq!(first.metadata_get("lavfi.vmafmotion.score"), Some("0.00"));

        let second = filter.step(gray_frame(100, 8, 8));
        // Independent black-box oracle: ffprobe reports 84.00 for 16 -> 100,
        // proving the score is not normalised by 255.
        assert_eq!(second.metadata_get("lavfi.vmafmotion.score"), Some("84.00"));
    }

    #[test]
    fn changed_area_weights_the_mean_by_pixel_count() {
        let mut filter = Filter::default();
        let _ = filter.step(gray_frame(16, 8, 4));
        let mut half_changed = gray_frame(16, 8, 4);
        if let Some(mut plane) = half_changed.plane_mut(0) {
            for y in 0..plane.rows() {
                if let Some(row) = plane.row_mut(y) {
                    row[..4].fill(100);
                }
            }
        }
        let out = filter.step(half_changed);
        // Half of the samples changes by 84: mean = 42.00. This separates a
        // mean from a maximum or a changed-pixels-only mean.
        assert_eq!(out.metadata_get("lavfi.vmafmotion.score"), Some("42.00"));
    }

    #[test]
    fn each_frame_compares_only_to_its_predecessor() {
        let mut filter = Filter::default();
        let first = filter.step(gray_frame(16, 4, 4));
        let second = filter.step(gray_frame(100, 4, 4));
        let third = filter.step(gray_frame(40, 4, 4));
        let fourth = filter.step(gray_frame(16, 4, 4));
        assert_eq!(first.metadata_get("lavfi.vmafmotion.score"), Some("0.00"));
        assert_eq!(second.metadata_get("lavfi.vmafmotion.score"), Some("84.00"));
        assert_eq!(third.metadata_get("lavfi.vmafmotion.score"), Some("60.00"));
        assert_eq!(fourth.metadata_get("lavfi.vmafmotion.score"), Some("24.00"));
    }

    #[test]
    fn flush_discards_the_previous_frame() {
        let mut filter = Filter::default();
        let _ = filter.step(gray_frame(16, 4, 4));
        filter.flush_state();
        let out = filter.step(gray_frame(100, 4, 4));
        assert_eq!(out.metadata_get("lavfi.vmafmotion.score"), Some("0.00"));
    }
}
