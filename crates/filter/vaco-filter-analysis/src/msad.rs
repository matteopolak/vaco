//! `msad` — mean sum of absolute differences between two video streams.
//!
//! `ffmpeg -h filter=msad`: same pad shape as `psnr`/`identity`
//! (`main`/`reference` in, `default` out), no options, no framesync surface
//! (measured, same finding as [`crate::psnr`]).
//!
//! # Formula: exactly `vaco_filter_vdsp::normalised_sad`
//!
//! Measured against `ffmpeg 8.1`: a flat pair differing by `18` everywhere
//! scores `msad.Y = 0.070588`, and `18/255 = 0.0705882...` — mean absolute
//! difference normalised to `[0,1]` by the sample range, which is exactly
//! what `vaco-filter-vdsp::normalised_sad` (built for `freezedetect`)
//! already computes. This filter is a direct reuse, per this crate's brief:
//! no new kernel needed.
//!
//! # Averaging: unweighted, like `identity` and unlike `psnr`/`ssim`
//!
//! Same measurement as [`crate::identity`]'s doc, run through `msad`
//! instead: `msad.Y=0.5, msad.U=0.0, msad.V=0.0` (asymmetric yuv420p input,
//! same fixture) gives `msad_avg=0.166667 = (0.5+0+0)/3`, not the
//! sample-weighted `0.333333` `psnr`'s formula would produce. See
//! [`crate::fmt::simple_average`].
//!
//! # Key order note
//!
//! Same measured divergence as `identity`: reference order is `V Y U`
//! (yuv420p) / `B R G` (`gbrp`); this crate writes ascending order. Values
//! are byte-exact, tag order is not — see `docs/filter/vaco-filter-analysis.md`.

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
    name: "msad",
    description: "Calculate the MSAD between two video streams.",
    inputs: REFERENCE_PADS,
    outputs: crate::video::VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Default)]
pub(crate) struct Msad;

impl PairedFilter for Msad {
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
            let sad = vaco_filter_vdsp::normalised_sad(a, b);
            values.push(sad);
            tags.push((format!("lavfi.msad.msad.{label}"), fixed6(sad)));
        }
        if !values.is_empty() {
            for (key, value) in tags {
                out.set_metadata(key, value);
            }
            out.set_metadata("lavfi.msad.msad_avg", fixed6(simple_average(&values)));
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
        filter: Box::new(Paired::new(Msad)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
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
    fn self_identical_is_zero() {
        let a = gray_frame(128, 8, 8);
        let b = a.clone();
        let out = measure(&a, &b);
        assert_eq!(out.metadata_get("lavfi.msad.msad.Y"), Some("0.000000"));
        assert_eq!(out.metadata_get("lavfi.msad.msad_avg"), Some("0.000000"));
    }

    /// Distinguishing input: a flat pair differing by exactly `18` has a
    /// closed form (`18/255`), which pins down the normalisation constant —
    /// a self-identical pair alone cannot, since `0/255` and `0/256` are
    /// both `0`. Measured against `ffmpeg 8.1`.
    #[test]
    fn flat_pair_matches_the_closed_form() {
        let a = gray_frame(128, 4, 4);
        let b = gray_frame(110, 4, 4);
        let out = measure(&a, &b);
        assert_eq!(out.metadata_get("lavfi.msad.msad.Y"), Some("0.070588"));
        assert_eq!(out.metadata_get("lavfi.msad.msad_avg"), Some("0.070588"));
    }

    #[test]
    fn average_is_unweighted() {
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
        if let Some(mut p) = b.plane_mut(0) {
            for y in 0..8 {
                if let Some(row) = p.row_mut(y) {
                    row.fill(200);
                }
            }
        }
        let out = measure(&a, &b);
        assert_eq!(out.metadata_get("lavfi.msad.msad.Y"), Some("0.372549"));
        assert_eq!(out.metadata_get("lavfi.msad.msad.U"), Some("0.000000"));
        assert_eq!(out.metadata_get("lavfi.msad.msad.V"), Some("0.000000"));
        // (0.372549 + 0 + 0) / 3, unweighted by sample count.
        assert_eq!(out.metadata_get("lavfi.msad.msad_avg"), Some("0.124183"));
    }
}
