//! Dequantisation and the inverse WHT/DCT, RFC 6386 §14.
//!
//! Every 4x4 block is handled by destructuring `[i32; 4]` rows rather than
//! indexing them — `let [a, b, c, d] = row;` reads every element in one
//! pattern match instead of four `[]` operations, which is what keeps this
//! module clear of `indexing_slicing` while still touching each of the 16
//! positions exactly once per pass. RFC 6386 §14's "row pass"/"column pass"
//! wording describes a flat, row-major `[i32; 16]`; here both passes work on
//! actual rows of a `[[i32; 4]; 4]`, with a transpose between them standing
//! in for whichever pass the RFC names "row" when it means "column" of the
//! flat layout (its row pass touches strided elements four apart — a column
//! of the row-major matrix).

use crate::tables::{AC_QLOOKUP, COSPI8_SQRT2_MINUS1, DC_QLOOKUP, SINPI8_SQRT2};

fn clamp_q(q: i32) -> usize {
    usize::try_from(q.clamp(0, 127)).unwrap_or(0)
}

fn dc_q(q: i32) -> i32 {
    i32::from(DC_QLOOKUP.get(clamp_q(q)).copied().unwrap_or(0))
}

fn ac_q(q: i32) -> i32 {
    i32::from(AC_QLOOKUP.get(clamp_q(q)).copied().unwrap_or(0))
}

/// The six dequantisation factors in effect for one macroblock, RFC 6386
/// §14.1 / §20.4's `dequant_init`.
#[derive(Debug, Clone, Copy)]
pub struct DequantFactors {
    pub y1_dc: i32,
    pub y1_ac: i32,
    pub y2_dc: i32,
    pub y2_ac: i32,
    pub uv_dc: i32,
    pub uv_ac: i32,
}

#[allow(
    clippy::integer_division,
    reason = "RFC 6386 §14.1's Y2 AC scale is exactly *155/100 with truncating division, not a size split"
)]
impl DequantFactors {
    /// `q` is the macroblock's base index (frame `y_ac_qi`, already combined
    /// with the segment quantizer adjustment); the five `*_delta` fields
    /// come from the frame header's `quant_indices()`.
    #[must_use]
    pub fn new(
        q: i32,
        y_dc_delta: i32,
        y2_dc_delta: i32,
        y2_ac_delta: i32,
        uv_dc_delta: i32,
        uv_ac_delta: i32,
    ) -> Self {
        let y2_ac = (ac_q(q + y2_ac_delta) * 155 / 100).max(8);
        let uv_dc = dc_q(q + uv_dc_delta).min(132);
        Self {
            y1_dc: dc_q(q + y_dc_delta),
            y1_ac: ac_q(q),
            y2_dc: dc_q(q + y2_dc_delta) * 2,
            y2_ac,
            uv_dc,
            uv_ac: ac_q(q + uv_ac_delta),
        }
    }
}

type Mat4 = [[i32; 4]; 4];

#[allow(clippy::many_single_char_names, reason = "naming all 16 flat positions is clearer than indexing them")]
fn to_mat(flat: &[i32; 16]) -> Mat4 {
    let [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p] = *flat;
    [[a, b, c, d], [e, f, g, h], [i, j, k, l], [m, n, o, p]]
}

#[allow(clippy::many_single_char_names, reason = "naming all 16 flat positions is clearer than indexing them")]
fn to_flat(mat: Mat4) -> [i32; 16] {
    let [[a, b, c, d], [e, f, g, h], [i, j, k, l], [m, n, o, p]] = mat;
    [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p]
}

#[allow(clippy::many_single_char_names, reason = "naming all 16 flat positions is clearer than indexing them")]
fn transpose(mat: Mat4) -> Mat4 {
    let [[a, b, c, d], [e, f, g, h], [i, j, k, l], [m, n, o, p]] = mat;
    [[a, e, i, m], [b, f, j, n], [c, g, k, o], [d, h, l, p]]
}

/// RFC 6386 §14.3's `vp8_short_inv_walsh4x4_c`: the Y2 (luma DC) block's
/// inverse Walsh-Hadamard transform. `coeffs` is in raster order (not
/// zigzag); returns the 16 values that become coefficient 0 of each of the
/// 16 Y subblocks, indexed the same raster way (`out[i]` -> Y subblock `i`).
#[must_use]
pub fn inverse_wht(coeffs: &[i32; 16]) -> [i32; 16] {
    // RFC's "row pass" touches elements four apart -- a column of the
    // row-major layout -- so it runs here on rows of the transpose.
    let mut stage1 = [[0i32; 4]; 4];
    for (out_row, in_row) in stage1.iter_mut().zip(transpose(to_mat(coeffs))) {
        let [c0, c1, c2, c3] = in_row;
        let a1 = c0 + c3;
        let b1 = c1 + c2;
        let c1_ = c1 - c2;
        let d1 = c0 - c3;
        *out_row = [a1 + b1, c1_ + d1, a1 - b1, d1 - c1_];
    }
    let stage1 = transpose(stage1);

    let mut out = [[0i32; 4]; 4];
    for (out_row, in_row) in out.iter_mut().zip(stage1) {
        let [c0, c1, c2, c3] = in_row;
        let a1 = c0 + c3;
        let b1 = c1 + c2;
        let c1_ = c1 - c2;
        let d1 = c0 - c3;
        let a2 = a1 + b1;
        let b2 = c1_ + d1;
        let c2_ = a1 - b1;
        let d2 = d1 - c1_;
        *out_row = [(a2 + 3) >> 3, (b2 + 3) >> 3, (c2_ + 3) >> 3, (d2 + 3) >> 3];
    }
    to_flat(out)
}

fn dct_butterfly(c0: i32, c1: i32, c2: i32, c3: i32) -> (i32, i32, i32, i32) {
    let a1 = c0 + c2;
    let b1 = c0 - c2;
    let t1 = (c1 * SINPI8_SQRT2) >> 16;
    let t2 = c3 + ((c3 * COSPI8_SQRT2_MINUS1) >> 16);
    let c1_out = t1 - t2;
    let t1 = c1 + ((c1 * COSPI8_SQRT2_MINUS1) >> 16);
    let t2 = (c3 * SINPI8_SQRT2) >> 16;
    let d1 = t1 + t2;
    (a1, b1, c1_out, d1)
}

/// RFC 6386 §14.4's `short_idct4x4llm_c`: the inverse DCT for one 4x4
/// residue block, raster order in and out.
#[must_use]
pub fn inverse_dct(coeffs: &[i32; 16]) -> [i32; 16] {
    let mut stage1 = [[0i32; 4]; 4];
    for (out_row, in_row) in stage1.iter_mut().zip(transpose(to_mat(coeffs))) {
        let [c0, c1, c2, c3] = in_row;
        let (a1, b1, c1_out, d1) = dct_butterfly(c0, c1, c2, c3);
        *out_row = [a1 + d1, b1 + c1_out, b1 - c1_out, a1 - d1];
    }
    let stage1 = transpose(stage1);

    let mut out = [[0i32; 4]; 4];
    for (out_row, in_row) in out.iter_mut().zip(stage1) {
        let [c0, c1, c2, c3] = in_row;
        let (a1, b1, c1_out, d1) = dct_butterfly(c0, c1, c2, c3);
        *out_row = [
            (a1 + d1 + 4) >> 3,
            (b1 + c1_out + 4) >> 3,
            (b1 - c1_out + 4) >> 3,
            (a1 - d1 + 4) >> 3,
        ];
    }
    to_flat(out)
}

/// RFC 6386 §14.5: `clamp(predictor + residue, 0, 255)`, per pixel, done in
/// a 32-bit accumulator before narrowing.
#[must_use]
pub fn add_residue(predictor: u8, residue: i32) -> u8 {
    u8::try_from((i32::from(predictor) + residue).clamp(0, 255)).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wht_of_all_zero_is_all_zero() {
        assert_eq!(inverse_wht(&[0; 16]), [0; 16]);
    }

    #[test]
    fn dct_of_all_zero_is_all_zero() {
        assert_eq!(inverse_dct(&[0; 16]), [0; 16]);
    }

    #[test]
    fn wht_dc_only_matches_the_fast_path_formula() {
        let mut coeffs = [0; 16];
        if let Some(c0) = coeffs.first_mut() {
            *c0 = 40;
        }
        let out = inverse_wht(&coeffs);
        let expected = (40 + 3) >> 3;
        assert!(out.iter().all(|&v| v == expected));
    }

    #[test]
    fn dequant_special_cases_match_the_spec_formulas() {
        let d = DequantFactors::new(0, 0, 0, 0, 0, 0);
        assert_eq!(d.y2_dc, dc_q(0) * 2);
        #[allow(clippy::integer_division, reason = "matches the spec formula under test")]
        let expected_y2_ac = (ac_q(0) * 155 / 100).max(8);
        assert_eq!(d.y2_ac, expected_y2_ac);
        assert_eq!(d.uv_dc, dc_q(0).min(132));
    }

    #[test]
    fn add_residue_saturates() {
        assert_eq!(add_residue(250, 100), 255);
        assert_eq!(add_residue(10, -100), 0);
        assert_eq!(add_residue(100, 10), 110);
    }

    proptest::proptest! {
        #[test]
        fn transforms_never_panic_on_arbitrary_i16_input(coeffs in proptest::collection::vec(-2048i32..=2047, 16)) {
            let mut arr = [0i32; 16];
            arr.copy_from_slice(&coeffs);
            let _ = inverse_wht(&arr);
            let _ = inverse_dct(&arr);
        }
    }
}
