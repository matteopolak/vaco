//! §8.6.1's dequantization functions and §8.6.2's reconstruct process,
//! built on [`vaco_codec_dsp_idct::vp9`] for the actual transform math.

use crate::tables;
use vaco_codec_dsp_idct::vp9::{TxType, inverse_transform_2d};

/// §8.6.1's `dc_q(b)`/`ac_q(b)`.
fn dc_q(bit_depth: u8, b: i32) -> i32 {
    let row = usize::from((bit_depth - 8) >> 1).min(2);
    let idx = usize::try_from(b.clamp(0, 255)).unwrap_or(0);
    tables::DC_QLOOKUP.get(row).and_then(|r| r.get(idx)).copied().unwrap_or(4)
}

fn ac_q(bit_depth: u8, b: i32) -> i32 {
    let row = usize::from((bit_depth - 8) >> 1).min(2);
    let idx = usize::try_from(b.clamp(0, 255)).unwrap_or(0);
    tables::AC_QLOOKUP.get(row).and_then(|r| r.get(idx)).copied().unwrap_or(4)
}

/// The per-plane, per-coefficient-position quantizer values a block needs:
/// `(dc, ac)`.
#[must_use]
pub fn dequant_factors(bit_depth: u8, qindex: i32, delta_dc: i32, delta_ac: i32) -> (i32, i32) {
    (dc_q(bit_depth, qindex + delta_dc), ac_q(bit_depth, qindex + delta_ac))
}

/// §8.6.2's reconstruct process: dequantize `tokens` (raster-order,
/// `n0 * n0` where `n0 = 1 << (tx_sz + 2)`), inverse-transform in place, and
/// return the residual (still raster order) ready to be added to the
/// prediction and clipped.
#[must_use]
#[allow(clippy::integer_division, reason = "§8.6.2's reconstruct process: dqDenom is 1 or 2, truncating toward zero (the spec's own '/' operator)")]
pub fn reconstruct(tokens: &[i32], tx_sz: i32, dc_quant: i32, ac_quant: i32, tx_type: TxType, lossless: bool) -> Vec<i64> {
    let n = u32::try_from(tx_sz + 2).unwrap_or(2);
    let n0 = 1usize << n;
    let dq_denom: i64 = if tx_sz == tables::TX_32X32 { 2 } else { 1 };
    let mut dequant: Vec<i64> = tokens
        .iter()
        .take(n0 * n0)
        .map(|&t| i64::from(t) * i64::from(ac_quant) / dq_denom)
        .collect();
    if dequant.len() < n0 * n0 {
        dequant.resize(n0 * n0, 0);
    }
    if let Some(slot) = dequant.first_mut() {
        let t0 = tokens.first().copied().unwrap_or(0);
        *slot = i64::from(t0) * i64::from(dc_quant) / dq_denom;
    }
    inverse_transform_2d(&mut dequant, n, tx_type, lossless);
    dequant
}
