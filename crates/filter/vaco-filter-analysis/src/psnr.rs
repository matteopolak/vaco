//! `psnr` — peak signal-to-noise ratio between two video streams.
//!
//! `ffmpeg -h filter=psnr`: `Inputs: #0: main (video) #1: reference
//! (video)`, `Outputs: #0: default (video)`, one option (`stats_file`, not
//! implemented — this crate has no file I/O surface and the metadata export
//! is the part interface gap 11 exists for).
//!
//! # Not `vaco-filter-framesync`
//!
//! `ffmpeg -h filter=psnr` carries **no** `eof_action`/`shortest`/
//! `repeatlast`/`ts_sync_mode` section — contrast `ffmpeg -h
//! filter=alphamerge`, which has the full framesync surface verbatim. `psnr`
//! measures identically to `framepack`
//! (`vaco_filter_core::adapt::Paired`'s own doc): two inputs, strict
//! lockstep, no independent per-input timeline. So this filter is a
//! [`vaco_filter_core::adapt::PairedFilter`], not a
//! `vaco_filter_framesync::FrameSyncFilter`, and needed no change to either
//! crate. The same measurement applies to `ssim`, `identity` and `msad` in
//! this crate — checked individually, not assumed to generalise from this
//! one.
//!
//! # Metadata export, measured against `ffmpeg 8.1`
//!
//! ```text
//! $ ffprobe -of json -show_frames -f lavfi \
//!     -i "color=gray@1.0:s=16x16,format=gray[a];color=0x6E6E6E:s=16x16,format=gray[b];[a][b]psnr"
//! "tags": {
//!     "lavfi.psnr.mse_avg":  "324.000000",
//!     "lavfi.psnr.mse.y":    "324.000000",
//!     "lavfi.psnr.psnr.y":   "23.025354",
//!     "lavfi.psnr.psnr_avg": "23.025354"
//! }
//! ```
//!
//! * Key order: `mse_avg` first, then `mse.<c>`/`psnr.<c>` interleaved per
//!   component in **ascending plane order** (`y`,`u`,`v` for planar YUV;
//!   `r`,`g`,`b` for `gbrp` — measured on both, see [`crate::video::component_labels`]),
//!   then `psnr_avg` last.
//! * `mse.<c>` is the mean squared error of that component, `%f` (six
//!   decimals, never trimmed — [`crate::fmt::fixed6`]).
//! * `psnr.<c>` is `10 * log10(MAX^2 / mse)` where `MAX = 255` for 8-bit
//!   samples, or the literal string `"inf"` when `mse == 0` — measured: a
//!   self-identical pair prints `psnr.y: "inf"`, not a very large finite
//!   number.
//! * `mse_avg` is the **sample-count-weighted** average of the per-component
//!   MSEs, not a plain mean — measured by feeding an asymmetric yuv420p
//!   input (luma differs, chroma does not) and finding `mse_avg` matches
//!   `(mse_y*n_y + mse_u*n_u + mse_v*n_v) / (n_y+n_u+n_v)` exactly
//!   (`21675.0` for `mse_y=32512.5` over 256 luma samples and `mse_u=mse_v=0`
//!   over 64 chroma samples each) and does **not** match the plain mean
//!   (`10837.5`). `psnr_avg` is then `10*log10(255^2/mse_avg)` — i.e. it is
//!   *not* an average of the per-component PSNRs either; it is recomputed
//!   from `mse_avg`. See [`crate::fmt::weighted_average`]'s doc for the same
//!   measurement, shared with `ssim`.
//!
//! # Distinguishing input built for this filter
//!
//! A flat pair (every pixel the same two values, `a` vs `b`) has a
//! **closed-form** PSNR: `MSE = (a-b)^2` exactly (no averaging error to get
//! wrong), so `10*log10(255^2/(a-b)^2)` is checked bit-for-bit against this
//! filter's output — this is what rules out an off-by-one in the MAX
//! constant (255 vs 256, the exact class of error
//! `planning/AGENT-CONSTRAINTS.md` flags for `tblend`) and rules out
//! confusing `log` (natural) with `log10`. The self-identical case alone
//! cannot catch either bug, since `0/0`-style MSE is `0` regardless of which
//! constant or logarithm base is used.

use smallvec::SmallVec;
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameOut, Paired, PairedFilter};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::fmt::{fixed6, weighted_average};
use crate::video::{REFERENCE_PADS, component_labels, copy_meta, video_shape};

pub const DESC: FilterDesc = FilterDesc {
    name: "psnr",
    description: "Calculate the PSNR between two video streams.",
    inputs: REFERENCE_PADS,
    outputs: crate::video::VIDEO_PAD,
    flags: FilterFlags::empty(),
};

/// 8-bit peak value; every format this filter is measured against is 8-bit
/// planar (see [`crate::video::component_labels`]'s own scope note).
const MAX: f64 = 255.0;

#[derive(Debug, Default)]
pub(crate) struct Psnr;

impl PairedFilter for Psnr {
    fn filter_frames(&mut self, _ctx: &mut FilterContext<'_>, inputs: SmallVec<[Frame; 4]>) -> Result<FrameOut> {
        let [main, reference] = <[Frame; 2]>::try_from(inputs.into_vec())
            .unwrap_or_else(|_| unreachable!("Paired guarantees exactly input_count() frames"));
        Ok(FrameOut::One(measure(&main, &reference)))
    }
}

/// The actual measurement, factored out of [`PairedFilter::filter_frames`]
/// so it can be unit-tested without constructing a [`FilterContext`] — this
/// filter never touches `ctx` (it does not change geometry or timing), so
/// nothing is lost by testing the pure function directly, matching
/// `vaco-filter-temporal::freezedetect::Filter::step`'s precedent.
fn measure(main: &Frame, reference: &Frame) -> Frame {
    let mut out = main.clone();
    if let (Some((fmt, _, _, main_planes)), Some((_, _, _, ref_planes))) =
        (video_shape(main), video_shape(reference))
    {
        let labels = component_labels(fmt, main_planes.min(ref_planes));
        let mut per_component: Vec<(f64, u64)> = Vec::new();
        let mut tags: Vec<(String, String)> = Vec::new();
        for (plane_idx, label) in labels.iter().enumerate().take(main_planes.min(ref_planes)) {
            let (Some(a), Some(b)) = (main.plane(plane_idx), reference.plane(plane_idx)) else {
                continue;
            };
            let sse = vaco_filter_vdsp::plane_sse(a, b);
            let rows = a.rows().min(b.rows());
            let cols = (0..rows)
                .filter_map(|y| Some(a.row(y)?.len().min(b.row(y)?.len())))
                .min()
                .unwrap_or(0);
            let samples = (rows as u64).saturating_mul(cols as u64);
            #[allow(clippy::cast_precision_loss, reason = "sse/samples are frame-sized")]
            let mse = if samples == 0 {
                0.0
            } else {
                sse as f64 / samples as f64
            };
            per_component.push((mse, samples));
            tags.push((format!("lavfi.psnr.mse.{}", label.to_lowercase()), fixed6(mse)));
            tags.push((
                format!("lavfi.psnr.psnr.{}", label.to_lowercase()),
                psnr_string(mse),
            ));
        }
        if !per_component.is_empty() {
            let mse_avg = weighted_average(&per_component);
            out.set_metadata("lavfi.psnr.mse_avg", fixed6(mse_avg));
            for (key, value) in tags {
                out.set_metadata(key, value);
            }
            out.set_metadata("lavfi.psnr.psnr_avg", psnr_string(mse_avg));
        }
    }
    copy_meta(&mut out, main);
    out
}

/// `10*log10(MAX^2/mse)`, or the literal `"inf"` when `mse` is exactly `0` —
/// measured: the reference never prints a very large finite number for a
/// perfect match, it prints `inf`.
fn psnr_string(mse: f64) -> String {
    if mse == 0.0 {
        return "inf".to_owned();
    }
    fixed6(10.0 * (MAX * MAX / mse).log10())
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let _ = req;
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(2, 1, MediaType::Video, req.instance),
        filter: Box::new(Paired::new(Psnr)),
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

    /// Independent oracle: a self-identical pair has `MSE = 0` by
    /// definition of "the same picture", so PSNR must be the reference's
    /// spelling of infinite, not a large finite number.
    #[test]
    fn self_identical_is_infinite() {
        let a = gray_frame(128, 8, 8);
        let b = a.clone();
        let out = measure(&a, &b);
        assert_eq!(out.metadata_get("lavfi.psnr.mse.y"), Some("0.000000"));
        assert_eq!(out.metadata_get("lavfi.psnr.psnr.y"), Some("inf"));
        assert_eq!(out.metadata_get("lavfi.psnr.psnr_avg"), Some("inf"));
    }

    /// Distinguishing input: a flat pair has a closed-form MSE = (a-b)^2
    /// with no averaging error, which rules out both an off-by-one in the
    /// MAX constant (255 vs 256) and a `ln` vs `log10` mixup — the
    /// self-identical case alone cannot distinguish either, since 0/0-style
    /// MSE hides both bugs. Measured against `ffmpeg 8.1`:
    /// `gray@1.0`(=128) vs `0x6E6E6E`(luma 110) scores mse=324,
    /// psnr=23.025354.
    #[test]
    fn flat_pair_matches_the_closed_form() {
        let a = gray_frame(128, 4, 4);
        let b = gray_frame(110, 4, 4);
        let out = measure(&a, &b);
        assert_eq!(out.metadata_get("lavfi.psnr.mse.y"), Some("324.000000"));
        assert_eq!(out.metadata_get("lavfi.psnr.psnr.y"), Some("23.025354"));
        assert_eq!(out.metadata_get("lavfi.psnr.mse_avg"), Some("324.000000"));
        assert_eq!(out.metadata_get("lavfi.psnr.psnr_avg"), Some("23.025354"));
    }
}
