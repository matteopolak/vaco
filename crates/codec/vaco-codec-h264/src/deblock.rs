//! Whole-picture wiring for clause 8.7's deblocking filter, built on
//! [`vaco_codec_dsp_deblock`]'s pure per-edge primitives.
//!
//! This module owns exactly what that crate's own doc says stays in the
//! caller: walking the picture in macroblock raster order, deriving
//! boundary strength from macroblock coding mode (clause 8.7.2.1),
//! honouring `disable_deblocking_filter_idc`, and the vertical-then-
//! horizontal, left-to-right-then-top-to-bottom filtering order clause
//! 8.7 itself specifies -- each edge's filtering uses samples already
//! modified by every earlier edge in that same order, which is why this
//! is a second, ordered pass over already-reconstructed pixels rather
//! than something foldable into [`crate::reconstruct::reconstruct_picture_luma`]'s
//! own single top-to-bottom, left-to-right walk: a macroblock's own
//! *right* and *bottom* edges are not yet known when reconstruction visits
//! it, but deblocking only ever needs a macroblock's *left*/*top* neighbour,
//! which reconstruction's own raster order already guarantees is complete
//! (finished decoding, *and* finished deblocking, since deblocking follows
//! the same raster order) by the time this pass reaches it.
//!
//! **Scope, explicitly, not merely unimplemented**: luma only (this
//! crate's own [`crate::reconstruct::PictureBuffer`] does not store
//! chroma samples yet -- a pre-existing gap, not one this module
//! introduces) and boundary strength derivation only for the case every
//! fixture this crate decodes actually is, an all-`Intra` picture: clause
//! 8.7.2.1's Table 8-18 collapses to `bS = 4` at every macroblock edge and
//! `bS = 3` at every internal 4x4 edge whenever *both* neighbouring
//! samples are intra, which is trivially true when the whole picture is.
//! The general derivation (inter macroblocks: transform-coefficient
//! presence for `bS = 2`, motion-vector/reference-index differences for
//! `bS = 1`) needs the neighbour's own coding mode and is deferred, the
//! same "revisit once inter reconstruction exists" scope note this
//! crate's other modules already use -- [`deblock_picture_luma`] returns
//! [`vaco_core::Error::Unsupported`] rather than silently guessing if it
//! ever sees a non-intra macroblock, the same fail-loud shape as
//! [`crate::reconstruct::reconstruct_picture_luma`]'s own `I_PCM`/skipped
//! refusals right next to it.

#![allow(
    dead_code,
    reason = "exercised by this module's own tests via reconstruct.rs; not yet wired into \
              vaco-codec-h264's own public decode/receive_frame surface, the same gap \
              reconstruct.rs itself already notes"
)]

use core::num::NonZeroU8;
use vaco_codec_dsp_deblock::{EdgeThresholds, LumaLine, filter_luma_line};

use crate::mb::MbSummary;

/// Runs clause 8.7's deblocking filter over an already-fully-reconstructed
/// luma plane, in place.
///
/// `disable_deblocking_filter_idc` is the slice header field verbatim (`0`
/// = filter everything this crate can see, including what would be a
/// slice boundary if this decoder supported multiple slices per picture
/// yet; `1` = do not filter this slice's own macroblocks at all; `2` =
/// filter internal edges but not the picture's own slice-boundary edges
/// -- indistinguishable from `0` here today, since every fixture this
/// crate decodes is one slice per whole picture, so there is no internal
/// slice boundary within a picture to treat differently). `slice_alpha_c0_offset_div2`/
/// `slice_beta_offset_div2` are the slice header fields verbatim; this
/// function applies clause 8.7.2.2's own `* 2` itself.
///
/// # Errors
///
/// Returns [`vaco_core::Error::Unsupported`] if any macroblock in
/// `macroblocks` is not `Intra_4x4`/`Intra_16x16` -- see this module's own
/// doc for why that case is out of scope for now rather than merely
/// unhandled.
pub(crate) fn deblock_picture_luma(
    luma: &mut [u8],
    macroblocks: &[MbSummary],
    mbs_wide: u32,
    mbs_high: u32,
    disable_deblocking_filter_idc: u32,
    slice_alpha_c0_offset_div2: i32,
    slice_beta_offset_div2: i32,
) -> vaco_core::Result<()> {
    if disable_deblocking_filter_idc == 1 {
        return Ok(());
    }

    let n_mb = usize::try_from(mbs_wide.saturating_mul(mbs_high)).unwrap_or(0);
    let mut qpy_grid = vec![0i32; n_mb];
    for mb in macroblocks {
        if !(mb.is_intra4x4 || mb.is_intra16x16) {
            return Err(vaco_core::Error::Unsupported(
                "vaco-codec-h264: deblocking boundary-strength derivation for non-intra \
                 macroblocks (clause 8.7.2.1's transform-coefficient/motion-vector cases) is not \
                 implemented",
            ));
        }
        let idx = (mb.mb_y * mbs_wide + mb.mb_x) as usize;
        if let Some(slot) = qpy_grid.get_mut(idx) {
            *slot = mb.qpy;
        }
    }
    let qpy_at = |mx: u32, my: u32| -> u8 {
        let v = qpy_grid
            .get((my * mbs_wide + mx) as usize)
            .copied()
            .unwrap_or(0);
        u8::try_from(v.clamp(0, 51)).unwrap_or(51)
    };

    let filter_offset_a = slice_alpha_c0_offset_div2.saturating_mul(2);
    let filter_offset_b = slice_beta_offset_div2.saturating_mul(2);
    let width = mbs_wide.saturating_mul(16);

    let get = |luma: &[u8], x: u32, y: u32| -> u8 {
        luma.get((y * width + x) as usize).copied().unwrap_or(0)
    };
    let set = |luma: &mut [u8], x: u32, y: u32, v: u8| {
        if let Some(slot) = luma.get_mut((y * width + x) as usize) {
            *slot = v;
        }
    };

    for my in 0..mbs_high {
        for mx in 0..mbs_wide {
            let qp_here = qpy_at(mx, my);

            // Vertical edges first, left to right (clause 8.7's own
            // filtering order) -- edge `local == 0` is this macroblock's
            // shared boundary with its left neighbour; `4`/`8`/`12` are
            // internal to this macroblock alone.
            for local in [0u32, 4, 8, 12] {
                if local == 0 && mx == 0 {
                    continue;
                }
                let bs = if local == 0 { 4u8 } else { 3u8 };
                let qp_p = if local == 0 {
                    qpy_at(mx - 1, my)
                } else {
                    qp_here
                };
                let edge = EdgeThresholds::derive(qp_p, qp_here, filter_offset_a, filter_offset_b);
                let x = mx * 16 + local;
                for row in 0..16u32 {
                    let y = my * 16 + row;
                    let mut line = LumaLine {
                        p: [
                            get(luma, x - 1, y),
                            get(luma, x - 2, y),
                            get(luma, x - 3, y),
                            get(luma, x - 4, y),
                        ],
                        q: [
                            get(luma, x, y),
                            get(luma, x + 1, y),
                            get(luma, x + 2, y),
                            get(luma, x + 3, y),
                        ],
                    };
                    #[allow(clippy::unwrap_used, reason = "bs is 3 or 4 here, never 0")]
                    filter_luma_line(&mut line, NonZeroU8::new(bs).unwrap(), edge);
                    set(luma, x - 1, y, line.p[0]);
                    set(luma, x - 2, y, line.p[1]);
                    set(luma, x - 3, y, line.p[2]);
                    set(luma, x, y, line.q[0]);
                    set(luma, x + 1, y, line.q[1]);
                    set(luma, x + 2, y, line.q[2]);
                }
            }

            // Then horizontal edges, top to bottom -- edge `local == 0` is
            // this macroblock's shared boundary with its above neighbour,
            // which by raster order has already had *both* its vertical
            // and horizontal edges filtered.
            for local in [0u32, 4, 8, 12] {
                if local == 0 && my == 0 {
                    continue;
                }
                let bs = if local == 0 { 4u8 } else { 3u8 };
                let qp_p = if local == 0 {
                    qpy_at(mx, my - 1)
                } else {
                    qp_here
                };
                let edge = EdgeThresholds::derive(qp_p, qp_here, filter_offset_a, filter_offset_b);
                let y = my * 16 + local;
                for col in 0..16u32 {
                    let x = mx * 16 + col;
                    let mut line = LumaLine {
                        p: [
                            get(luma, x, y - 1),
                            get(luma, x, y - 2),
                            get(luma, x, y - 3),
                            get(luma, x, y - 4),
                        ],
                        q: [
                            get(luma, x, y),
                            get(luma, x, y + 1),
                            get(luma, x, y + 2),
                            get(luma, x, y + 3),
                        ],
                    };
                    #[allow(clippy::unwrap_used, reason = "bs is 3 or 4 here, never 0")]
                    #[allow(clippy::unwrap_used, reason = "bs is 3 or 4 here, never 0")]
                    filter_luma_line(&mut line, NonZeroU8::new(bs).unwrap(), edge);
                    set(luma, x, y - 1, line.p[0]);
                    set(luma, x, y - 2, line.p[1]);
                    set(luma, x, y - 3, line.p[2]);
                    set(luma, x, y, line.q[0]);
                    set(luma, x, y + 1, line.q[1]);
                    set(luma, x, y + 2, line.q[2]);
                }
            }
        }
    }

    Ok(())
}
