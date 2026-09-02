//! `identity` — fraction of bit-exact pixels between two video streams.
//!
//! `ffmpeg -h filter=identity`: same pad shape as `psnr`
//! (`main`/`reference` in, `default` out), no options, no framesync surface
//! (measured — see [`crate::psnr`]'s doc for the general finding). Strict
//! lockstep via [`vaco_filter_core::adapt::Paired`].
//!
//! # What "identity" measures — distinguished from a continuous difference
//!
//! Measured against `ffmpeg 8.1`: a plane where half the pixels are
//! bit-identical and the other half differ by exactly `1` scores
//! `identity.Y = 0.5`, and a plane where half the pixels are identical and
//! the other half differ by the *maximum possible* (`255`) **also** scores
//! `0.5`. A continuous "how different" metric (e.g. `1 - mean_abs_diff/255`)
//! would score those two inputs differently (`~0.998` vs `0.5`); a binary
//! "were these samples exactly equal" metric scores them identically. That
//! is the distinguishing input: it rules out the continuous-metric
//! hypothesis that a self-identical/fully-different pair alone cannot rule
//! out (both hypotheses agree at the extremes). So `identity` is
//! [`vaco_filter_vdsp::identical_count`]'s fraction, per plane.
//!
//! # Averaging: unweighted, unlike `psnr`/`ssim`
//!
//! Measured on an asymmetric yuv420p input (luma differs at exactly half
//! its pixels, chroma is untouched): `identity.Y=0.5`, `identity.U=1.0`,
//! `identity.V=1.0`, `identity_avg=0.833333`. `(0.5+1+1)/3 = 0.833333`
//! matches; the sample-count-weighted formula `psnr`/`ssim` use does not
//! (`(0.5*256+64+64)/384 = 0.666667`). See [`crate::fmt::simple_average`]'s
//! doc for the same measurement, shared with `msad`.
//!
//! # Key order note
//!
//! Measured tag order for `identity` is **not** ascending plane order — for
//! yuv420p it is `V`, `Y`, `U`; for `gbrp` it is `B`, `R`, `G`. This crate
//! writes ascending order (`Y`, `U`, `V` / `G`, `B`, `R`) instead: the
//! *values* are byte-exact, the *insertion order* of the tag block is not,
//! and that is recorded here rather than silently claimed. See
//! `docs/filter/vaco-filter-analysis.md`.

use smallvec::SmallVec;
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameOut, Paired, PairedFilter};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::fmt::{fixed6, simple_average};
use crate::video::{REFERENCE_PADS, component_labels, copy_meta, video_shape};

pub const DESC: FilterDesc = FilterDesc {
    name: "identity",
    description: "Calculate the Identity between two video streams.",
    inputs: REFERENCE_PADS,
    outputs: crate::video::VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Default)]
pub(crate) struct Identity;

impl PairedFilter for Identity {
    fn filter_frames(
        &mut self,
        _ctx: &mut FilterContext<'_>,
        inputs: SmallVec<[Frame; 4]>,
    ) -> Result<FrameOut> {
        let [main, reference] = <[Frame; 2]>::try_from(inputs.into_vec())
            .unwrap_or_else(|_| unreachable!("Paired guarantees exactly input_count() frames"));
        Ok(FrameOut::One(measure(&main, &reference)))
    }
}

fn measure(main: &Frame, reference: &Frame) -> Frame {
    let mut out = main.clone();
    if let (Some((fmt, _, _, main_planes)), Some((_, _, _, ref_planes))) =
        (video_shape(main), video_shape(reference))
    {
        let labels = component_labels(fmt, main_planes.min(ref_planes));
        let mut values: Vec<f64> = Vec::new();
        let mut tags: Vec<(String, String)> = Vec::new();
        for (plane_idx, label) in labels.iter().enumerate().take(main_planes.min(ref_planes)) {
            let (Some(a), Some(b)) = (main.plane(plane_idx), reference.plane(plane_idx)) else {
                continue;
            };
            let (same, total) = vaco_filter_vdsp::identical_count(a, b);
            #[allow(clippy::cast_precision_loss, reason = "sample counts are frame-sized")]
            let fraction = if total == 0 {
                1.0
            } else {
                same as f64 / total as f64
            };
            values.push(fraction);
            tags.push((format!("lavfi.identity.identity.{label}"), fixed6(fraction)));
        }
        if !values.is_empty() {
            for (key, value) in tags {
                out.set_metadata(key, value);
            }
            out.set_metadata(
                "lavfi.identity.identity_avg",
                fixed6(simple_average(&values)),
            );
        }
    }
    copy_meta(&mut out, main);
    out
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let _ = req;
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(2, 1, MediaType::Video, req.instance),
        filter: Box::new(Paired::new(Identity)),
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

    #[test]
    fn self_identical_is_one() {
        let a = gray_frame(128, 8, 8);
        let b = a.clone();
        let out = measure(&a, &b);
        assert_eq!(
            out.metadata_get("lavfi.identity.identity.Y"),
            Some("1.000000")
        );
        assert_eq!(
            out.metadata_get("lavfi.identity.identity_avg"),
            Some("1.000000")
        );
    }

    /// Distinguishing input: half the plane bit-identical, the other half
    /// differing by a *small* amount (`1`, not the maximum `255`). A
    /// continuous difference metric would score this close to `1.0`
    /// (`1 - 1/255/2 ≈ 0.998`); `identity` scores it `0.5`, matching
    /// "exactly half the pixels are exactly equal" regardless of how far
    /// off the other half is. Measured against `ffmpeg 8.1`.
    #[test]
    fn half_identical_small_and_large_diff_both_score_half() {
        let pool = FramePool::default();
        let mut a_small_diff = pool.acquire_video(PixFmt::Gray8, 4, 4).unwrap();
        let mut b_small_diff = pool.acquire_video(PixFmt::Gray8, 4, 4).unwrap();
        if let Some(mut p) = a_small_diff.plane_mut(0) {
            p.fill(10);
        }
        if let Some(mut p) = b_small_diff.plane_mut(0) {
            p.fill(10);
            for y in 0..2 {
                if let Some(row) = p.row_mut(y) {
                    row.fill(11);
                }
            }
        }
        let out_small = measure(&a_small_diff, &b_small_diff);

        let a_max_diff = gray_frame(0, 4, 4);
        let mut b_max_diff = gray_frame(0, 4, 4);
        if let Some(mut p) = b_max_diff.plane_mut(0) {
            for y in 0..2 {
                if let Some(row) = p.row_mut(y) {
                    row.fill(255);
                }
            }
        }
        let out_max = measure(&a_max_diff, &b_max_diff);

        assert_eq!(
            out_small.metadata_get("lavfi.identity.identity.Y"),
            Some("0.500000")
        );
        assert_eq!(
            out_max.metadata_get("lavfi.identity.identity.Y"),
            Some("0.500000")
        );
    }

    /// `identity_avg` averages per-component values unweighted by sample
    /// count, unlike `psnr`'s `mse_avg` — see this module's doc.
    #[test]
    fn average_is_unweighted() {
        // Simulate three "components" with a synthetic three-plane frame by
        // reusing the yuv420p shape directly.
        let pool = FramePool::default();
        let mut a = pool.acquire_video(PixFmt::Yuv420p, 16, 16).unwrap();
        let mut b = pool.acquire_video(PixFmt::Yuv420p, 16, 16).unwrap();
        for idx in 0..3 {
            if let Some(mut p) = a.plane_mut(idx) {
                p.fill(10);
            }
            if let Some(mut p) = b.plane_mut(idx) {
                p.fill(10);
            }
        }
        // Half of luma (plane 0) differs.
        if let Some(mut p) = b.plane_mut(0) {
            for y in 0..8 {
                if let Some(row) = p.row_mut(y) {
                    row.fill(200);
                }
            }
        }
        let out = measure(&a, &b);
        assert_eq!(
            out.metadata_get("lavfi.identity.identity.Y"),
            Some("0.500000")
        );
        assert_eq!(
            out.metadata_get("lavfi.identity.identity.U"),
            Some("1.000000")
        );
        assert_eq!(
            out.metadata_get("lavfi.identity.identity.V"),
            Some("1.000000")
        );
        // (0.5 + 1 + 1) / 3, not sample-count-weighted.
        assert_eq!(
            out.metadata_get("lavfi.identity.identity_avg"),
            Some("0.833333")
        );
    }
}
