//! Scaling-list resolution (§7.4.5), dequantisation (§8.6.3), and the
//! inverse-transform hand-off to [`vaco_codec_dsp_idct::hevc`].
//!
//! Derived from the ITU-T H.265 specification, cross-checked against the HM
//! reference decoder's `TComTrQuant::xDeQuant`/`QpParam` (BSD-3-Clause, Tier
//! A — see `cabac_ctx`'s module doc). `extended_precision_processing` remains
//! refused at the SPS, so `maxLog2TrDynamicRange` is always 15.

use vaco_codec_dsp_idct::hevc::{ClipRange, idct2d_dct, idct2d_dst4};
use vaco_core::{Error, Result};
use vaco_parse_hevc::{Pps, ScalingListData, Sps};

use crate::scan::{self, ScanOrder};

/// Equation 8-309's `levelScale[QP % 6]`, cross-checked against HM's
/// `g_invQuantScales` in `TComRom.cpp`.
const INV_QUANT_SCALES: [i32; 6] = [40, 45, 51, 57, 64, 72];

/// Table 7-6's default list for intra matrices at sizes 8x8 through 32x32,
/// in the specification's up-right diagonal scan order.
const DEFAULT_SCALING_LIST_INTRA: [u8; 64] = [
    16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 16, 17, 16, 17, 18, 17, 18, 18, 17, 18, 21, 19, 20,
    21, 20, 19, 21, 24, 22, 22, 24, 24, 22, 22, 24, 25, 25, 27, 30, 27, 25, 25, 29, 31, 35, 35, 31,
    29, 36, 41, 44, 41, 36, 47, 54, 54, 47, 65, 70, 65, 88, 88, 115,
];

/// Table 7-6's default list for inter matrices at sizes 8x8 through 32x32,
/// in the specification's up-right diagonal scan order.
const DEFAULT_SCALING_LIST_INTER: [u8; 64] = [
    16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 17, 18, 18, 18, 18, 18, 18, 20, 20, 20,
    20, 20, 20, 20, 24, 24, 24, 24, 24, 24, 24, 24, 25, 25, 25, 25, 25, 25, 25, 28, 28, 28, 28, 28,
    28, 33, 33, 33, 33, 33, 41, 41, 41, 41, 54, 54, 54, 71, 71, 91,
];

/// Table 7-4's six choices. Keeping mode and component together makes an
/// impossible combination unrepresentable at the four reconstruction sites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScalingListKind {
    IntraY,
    IntraCb,
    IntraCr,
    InterY,
    InterCb,
    InterCr,
}

impl ScalingListKind {
    const fn matrix_id(self) -> usize {
        match self {
            Self::IntraY => 0,
            Self::IntraCb => 1,
            Self::IntraCr => 2,
            Self::InterY => 3,
            Self::InterCb => 4,
            Self::InterCr => 5,
        }
    }
}

/// Effective scaling factors for one active SPS/PPS pair.
///
/// PPS data overrides SPS data; if neither carries `scaling_list_data()`, the
/// Table 7-5/7-6 defaults apply. Copy-mode references and scan-to-raster
/// placement are resolved once here, rather than independently at every
/// transform leaf. Sizes 16 and 32 retain an 8x8 raster base plus the separate
/// DC coefficient; [`Self::factor`] applies equations 7-46 through 7-49.
#[derive(Debug)]
pub(crate) struct ScalingMatrices {
    enabled: bool,
    /// `[sizeId][matrixId][8 * y + x]`; sizeId 0 uses only x/y below 4.
    factors: [[[u8; 64]; 6]; 4],
    /// `[sizeId - 2][matrixId]` for 16x16 and 32x32.
    dc: [[u8; 6]; 2],
}

impl ScalingMatrices {
    /// Resolve §7.4.3.3.1's PPS/SPS/default precedence and §7.4.5's copy
    /// references for the active parameter-set pair.
    pub(crate) fn from_parameter_sets(sps: &Sps, pps: &Pps) -> Result<Self> {
        let mut out = Self {
            enabled: sps.scaling_list_enabled,
            factors: [[[16; 64]; 6]; 4],
            dc: [[16; 6]; 2],
        };
        if !out.enabled {
            return Ok(out);
        }

        let data = pps.scaling_list.as_deref().or(sps.scaling_list.as_deref());
        for size_id in 0usize..4 {
            let step = if size_id == 3 { 3 } else { 1 };
            let mut matrix_id = 0usize;
            while matrix_id < 6 {
                out.resolve_matrix(data, size_id, matrix_id, step)?;
                matrix_id += step;
            }
        }
        Ok(out)
    }

    fn resolve_matrix(
        &mut self,
        data: Option<&ScalingListData>,
        size_id: usize,
        matrix_id: usize,
        reference_step: usize,
    ) -> Result<()> {
        let Some(data) = data else {
            self.write_default(size_id, matrix_id);
            return Ok(());
        };
        let pred_mode = data
            .pred_mode
            .get(size_id)
            .and_then(|row| row.get(matrix_id))
            .copied()
            .ok_or(Error::InvalidData("scaling-list index out of range"))?;
        if pred_mode {
            self.write_explicit(data, size_id, matrix_id)?;
            return Ok(());
        }

        let delta = data
            .pred_matrix_id_delta
            .get(size_id)
            .and_then(|row| row.get(matrix_id))
            .copied()
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(Error::InvalidData("scaling-list index out of range"))?;
        if delta == 0 {
            self.write_default(size_id, matrix_id);
            return Ok(());
        }
        let distance = delta.checked_mul(reference_step).ok_or(Error::InvalidData(
            "scaling-list reference distance overflow",
        ))?;
        let reference = matrix_id
            .checked_sub(distance)
            .ok_or(Error::InvalidData("invalid scaling-list reference"))?;
        let referenced_factors = self
            .factors
            .get(size_id)
            .and_then(|row| row.get(reference))
            .copied()
            .ok_or(Error::InvalidData("invalid scaling-list reference"))?;
        let target_factors = self
            .factors
            .get_mut(size_id)
            .and_then(|row| row.get_mut(matrix_id))
            .ok_or(Error::InvalidData("scaling-list index out of range"))?;
        *target_factors = referenced_factors;
        if size_id >= 2 {
            let dc_size_id = size_id - 2;
            let referenced_dc = self
                .dc
                .get(dc_size_id)
                .and_then(|row| row.get(reference))
                .copied()
                .ok_or(Error::InvalidData("invalid scaling-list DC reference"))?;
            let target_dc = self
                .dc
                .get_mut(dc_size_id)
                .and_then(|row| row.get_mut(matrix_id))
                .ok_or(Error::InvalidData("scaling-list DC index out of range"))?;
            *target_dc = referenced_dc;
        }
        Ok(())
    }

    fn write_explicit(
        &mut self,
        data: &ScalingListData,
        size_id: usize,
        matrix_id: usize,
    ) -> Result<()> {
        let count = if size_id == 0 { 16 } else { 64 };
        let coefficients = data
            .coef
            .get(size_id)
            .and_then(|row| row.get(matrix_id))
            .and_then(|values| values.get(..count))
            .ok_or(Error::InvalidData(
                "scaling-list coefficient index out of range",
            ))?;
        if coefficients.contains(&0) {
            return Err(Error::InvalidData("zero scaling-list coefficient"));
        }
        self.write_scanned(size_id, matrix_id, coefficients);
        if size_id >= 2 {
            let dc = data
                .dc_coef
                .get(size_id - 2)
                .and_then(|row| row.get(matrix_id))
                .copied()
                .and_then(|value| u8::try_from(value).ok())
                .ok_or(Error::InvalidData("invalid scaling-list DC coefficient"))?;
            if dc == 0 {
                return Err(Error::InvalidData("zero scaling-list DC coefficient"));
            }
            let slot = self
                .dc
                .get_mut(size_id - 2)
                .and_then(|row| row.get_mut(matrix_id))
                .ok_or(Error::InvalidData("scaling-list DC index out of range"))?;
            *slot = dc;
        }
        Ok(())
    }

    fn write_default(&mut self, size_id: usize, matrix_id: usize) {
        if size_id == 0 {
            self.write_scanned(size_id, matrix_id, &[16; 16]);
        } else {
            let values = if matrix_id < 3 {
                &DEFAULT_SCALING_LIST_INTRA
            } else {
                &DEFAULT_SCALING_LIST_INTER
            };
            self.write_scanned(size_id, matrix_id, values);
        }
        if size_id >= 2
            && let Some(slot) = self
                .dc
                .get_mut(size_id - 2)
                .and_then(|row| row.get_mut(matrix_id))
        {
            *slot = 16;
        }
    }

    fn write_scanned(&mut self, size_id: usize, matrix_id: usize, values: &[u8]) {
        let base_size = if size_id == 0 { 4 } else { 8 };
        for ((x, y), &value) in scan::generate(base_size, ScanOrder::Diag)
            .into_iter()
            .zip(values)
        {
            let index = usize::from(y) * 8 + usize::from(x);
            if let Some(slot) = self
                .factors
                .get_mut(size_id)
                .and_then(|row| row.get_mut(matrix_id))
                .and_then(|matrix| matrix.get_mut(index))
            {
                *slot = value;
            }
        }
    }

    fn factor(&self, size: usize, kind: ScalingListKind, x: usize, y: usize) -> i32 {
        if !self.enabled {
            return 16;
        }
        let Some(size_id) = size
            .checked_ilog2()
            .and_then(|log2| log2.checked_sub(2))
            .and_then(|id| usize::try_from(id).ok())
            .filter(|&id| id < 4)
        else {
            return 16;
        };
        let matrix_id = kind.matrix_id();
        if size_id >= 2 && x == 0 && y == 0 {
            return self
                .dc
                .get(size_id - 2)
                .and_then(|row| row.get(matrix_id))
                .copied()
                .map_or(16, i32::from);
        }
        let replication_shift = size_id.saturating_sub(1);
        let base_x = x >> replication_shift;
        let base_y = y >> replication_shift;
        self.factors
            .get(size_id)
            .and_then(|row| row.get(matrix_id))
            .and_then(|matrix| matrix.get(base_y * 8 + base_x))
            .copied()
            .map_or(16, i32::from)
    }
}

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

/// §8.6.3's scaling process for one `size x size` block. `qp` is already
/// resolved for this block's component (luma QP directly, or [`chroma_qp`]'s
/// result for chroma); `matrices` is the active SPS/PPS pair's single resolved
/// scaling-list value.
#[allow(
    clippy::integer_division,
    reason = "QP % 6 / QP / 6 is eq. (8-283)'s own decomposition, not a truncation bug"
)]
pub(crate) fn dequant(
    coeffs: &[(u8, u8, i32)],
    size: usize,
    qp: i32,
    bit_depth: u32,
    matrices: &ScalingMatrices,
    kind: ScalingListKind,
) -> Vec<i32> {
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
    let right_shift = 10 - (transform_shift + per);
    let (min, max) = (
        -(1i64 << max_log2_tr_dynamic_range),
        (1i64 << max_log2_tr_dynamic_range) - 1,
    );

    for &(x, y, level) in coeffs {
        let (x, y) = (usize::from(x), usize::from(y));
        if x >= size || y >= size {
            continue;
        }
        let factor = matrices.factor(size, kind, x, y);
        let product = i64::from(level) * i64::from(factor) * i64::from(scale);
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

/// §8.6.4.1 equation 8-297's transquant-bypass branch: transform coefficient
/// levels are already residual samples, with no scaling or inverse transform.
/// Rotation is unavailable because the SPS range-extension flag that enables
/// it is refused by the decoder's scope check.
#[must_use]
pub(crate) fn transquant_bypass(coeffs: &[(u8, u8, i32)], size: usize) -> Vec<i32> {
    let mut out = vec![0i32; size * size];
    for &(x, y, level) in coeffs {
        let (x, y) = (usize::from(x), usize::from(y));
        if x >= size || y >= size {
            continue;
        }
        if let Some(slot) = out.get_mut(y * size + x) {
            *slot = level;
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
