//! Dequantisation (§8.6.3, flat scaling list only — see the crate doc) and
//! the inverse-transform hand-off to [`vaco_codec_dsp_idct::hevc`].
//!
//! Derived from the ITU-T H.265 specification, cross-checked against the HM
//! reference decoder's `TComTrQuant::xDeQuant`/`QpParam` (BSD-3-Clause, Tier
//! A — see `cabac_ctx`'s module doc). Scope: `extended_precision_processing`
//! and custom scaling lists are refused at the SPS (both range-extension /
//! optional features this crate does not implement), so
//! `maxLog2TrDynamicRange` is always 15 and every scaling matrix is flat.

use vaco_codec_dsp_idct::hevc::{ClipRange, idct2d_dct, idct2d_dst4};

/// `g_invQuantScales`, HM `TComRom.cpp` — the flat dequantisation scale per
/// `QP % 6`.
const INV_QUANT_SCALES: [i32; 6] = [40, 45, 51, 57, 64, 72];

/// Table 8-10 (4:2:0 chroma QP mapping), HM's `g_aucChromaScale[CHROMA_420]`.
/// Index is the (already-offset, clamped 0..=57) luma-derived chroma QP;
/// value is the mapped chroma QP.
const CHROMA_QP_MAP_420: [u8; 58] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 29, 30, 31, 32, 33, 33, 34, 34, 35, 35, 36, 36, 37, 37, 38, 39, 40, 41, 42, 43,
    44, 45, 46, 47, 48, 49, 50, 51,
];

/// §8.6.1's chroma QP derivation for 4:2:0 (`QpParam`'s chroma branch, this
/// crate's `qpBdOffset == 0` throughout — 8-bit only, see the crate doc, so
/// the `baseQp < 0` early-return branch never triggers here).
#[must_use]
pub(crate) fn chroma_qp(luma_qp: i32, chroma_qp_offset: i32) -> i32 {
    let base = (luma_qp + chroma_qp_offset).clamp(0, 57);
    let base_u = usize::try_from(base).unwrap_or(0);
    i32::from(
        CHROMA_QP_MAP_420
            .get(base_u)
            .copied()
            .unwrap_or_else(|| u8::try_from(base).unwrap_or(0)),
    )
}

/// §8.6.3's scaling process for one `size x size` block with a flat scaling
/// list, `qp` already resolved for this block's component (luma QP directly,
/// or [`chroma_qp`]'s result for chroma).
#[allow(
    clippy::integer_division,
    reason = "QP % 6 / QP / 6 is eq. (8-283)'s own decomposition, not a truncation bug"
)]
pub(crate) fn dequant(coeffs: &[(u8, u8, i32)], size: usize, qp: i32, bit_depth: u32) -> Vec<i32> {
    let mut out = vec![0i32; size * size];
    let log2_size = i32::try_from(size.trailing_zeros()).unwrap_or(0);
    let max_log2_tr_dynamic_range = 15i32;
    let bit_depth_i = i32::try_from(bit_depth).unwrap_or(8);
    let transform_shift = max_log2_tr_dynamic_range - bit_depth_i - log2_size;
    let qp = qp.max(0);
    let per = qp / 6;
    let rem = qp % 6;
    let scale = INV_QUANT_SCALES
        .get(usize::try_from(rem).unwrap_or(0))
        .copied()
        .unwrap_or(64);
    let right_shift = 6 - (transform_shift + per);
    let (min, max) = (
        -(1i64 << max_log2_tr_dynamic_range),
        (1i64 << max_log2_tr_dynamic_range) - 1,
    );

    for &(x, y, level) in coeffs {
        let (x, y) = (usize::from(x), usize::from(y));
        if x >= size || y >= size {
            continue;
        }
        let product = i64::from(level) * i64::from(scale);
        let value = if right_shift > 0 {
            let add = 1i64 << (right_shift - 1);
            (product + add) >> right_shift
        } else {
            product << (-right_shift)
        };
        let clipped = value.clamp(min, max);
        if let Some(slot) = out.get_mut(y * size + x) {
            *slot = i32::try_from(clipped).unwrap_or(0);
        }
    }
    out
}

/// Which §8.6.4.2 branch turns this block's scaled coefficients into residual
/// samples. One value rather than a `use_dst` flag beside a separate
/// `transform_skip` flag, so "DST-VII *and* transform-skip" is not a state
/// this crate's callers can construct.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransformKind {
    /// `trType == 0`.
    Dct,
    /// `trType == 1` — 4x4 intra luma only.
    Dst4,
    /// `transform_skip_flag == 1`: no transform, only the `tsShift` scaling.
    Skip,
}

/// Run the inverse transform (or §8.6.4.2's `transform_skip_flag` branch),
/// then §8.6.5's final residual-modification shift —
/// `vaco_codec_dsp_idct::hevc`'s own module doc is explicit that its two-pass
/// engine stops at the transform clause's own last step and applies no
/// further shift, because that final `bdShift` is a *residual reconstruction*
/// concern (clause 8.6.5), one level up from "the inverse transform" itself.
/// `bdShift = 20 - BitDepth` in this crate's non-extended, 8-bit-only scope
/// (`extended_precision_processing_flag` is refused at the SPS), i.e. `12`
/// always here — kept as an explicit function of `bit_depth` anyway so the
/// one non-obvious constant in this file has a name.
///
/// [`TransformKind::Skip`] shares that same `bdShift` tail deliberately: the
/// spec reaches it by the same route, and at 8-bit 4x4 the pair collapses to
/// HM's own `xITransformSkip` shift of `(d + 16) >> 5` (`<< 7` then
/// `+ 2048 >> 12`), which is how this was checked before it was measured.
#[must_use]
pub(crate) fn inverse_transform(
    dequantised: &[i32],
    size: usize,
    kind: TransformKind,
    bit_depth: u32,
) -> Vec<i32> {
    let mut out = vec![0i32; size * size];
    let clip = ClipRange::non_extended();
    match (kind, size) {
        (TransformKind::Skip, _) => {
            // §8.6.4.2: `r[x][y] = d[x][y] << tsShift`, with
            // `tsShift = 5 + Log2(nTbS)`. The
            // `extended_precision_processing_flag`-dependent
            // `Min(5, bdShift - 2)` alternative never applies — that flag is
            // an SPS range extension, refused by `decoder::check_scope`.
            let ts_shift = 5 + size.trailing_zeros();
            for (o, &d) in out.iter_mut().zip(dequantised.iter()) {
                *o = i32::try_from(i64::from(d) << ts_shift).unwrap_or(0);
            }
        }
        (TransformKind::Dst4, 4) => idct2d_dst4(dequantised, &mut out, clip),
        (_, 4) => idct2d_dct::<4>(dequantised, &mut out, clip),
        (_, 8) => idct2d_dct::<8>(dequantised, &mut out, clip),
        (_, 16) => idct2d_dct::<16>(dequantised, &mut out, clip),
        (_, 32) => idct2d_dct::<32>(dequantised, &mut out, clip),
        _ => {}
    }
    let bd_shift = 20i32
        .saturating_sub(i32::try_from(bit_depth).unwrap_or(8))
        .max(0);
    if bd_shift > 0 {
        let round = 1i64 << (bd_shift - 1);
        for v in &mut out {
            *v = i32::try_from((i64::from(*v) + round) >> bd_shift).unwrap_or(0);
        }
    }
    out
}

/// Add a residual block to an already-predicted block and clip to the valid
/// sample range for `bit_depth` (§8.6.5's `Clip1`).
pub(crate) fn add_residual_clip(pred: &mut [u16], residual: &[i32], size: usize, bit_depth: u32) {
    let max = (1i32 << bit_depth) - 1;
    for (p, &r) in pred.iter_mut().zip(residual.iter()).take(size * size) {
        let v = i32::from(*p) + r;
        *p = u16::try_from(v.clamp(0, max)).unwrap_or(0);
    }
}
