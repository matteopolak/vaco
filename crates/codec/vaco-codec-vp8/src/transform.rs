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

#[allow(
    clippy::many_single_char_names,
    reason = "naming all 16 flat positions is clearer than indexing them"
)]
fn to_mat(flat: &[i32; 16]) -> Mat4 {
    let [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p] = *flat;
    [[a, b, c, d], [e, f, g, h], [i, j, k, l], [m, n, o, p]]
}

#[allow(
    clippy::many_single_char_names,
    reason = "naming all 16 flat positions is clearer than indexing them"
)]
fn to_flat(mat: Mat4) -> [i32; 16] {
    let [[a, b, c, d], [e, f, g, h], [i, j, k, l], [m, n, o, p]] = mat;
    [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p]
}

#[allow(
    clippy::many_single_char_names,
    reason = "naming all 16 flat positions is clearer than indexing them"
)]
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

/// The forward DCT, `libvpx`'s `vp8_short_fdct4x4_c` (`vp8/encoder/dct.c`,
/// BSD-licensed, Tier A per `planning/AGENT-CONSTRAINTS.md`'s clean-room
/// section) — the mathematical partner [`inverse_dct`] needs, since RFC 6386
/// specifies only the decoder side (see `crate::encode`'s module doc). Raster
/// order in and out, spatial residue in, unquantised frequency coefficients
/// out.
#[must_use]
pub fn forward_dct(residue: &[i32; 16]) -> [i32; 16] {
    let mut rows = [[0i32; 4]; 4];
    for (out_row, in_row) in rows.iter_mut().zip(to_mat(residue)) {
        let [c0, c1, c2, c3] = in_row;
        let a1 = (c0 + c3) * 8;
        let b1 = (c1 + c2) * 8;
        let c1v = (c1 - c2) * 8;
        let d1 = (c0 - c3) * 8;
        let op1 = (c1v * 2217 + d1 * 5352 + 14500) >> 12;
        let op3 = (d1 * 2217 - c1v * 5352 + 7500) >> 12;
        *out_row = [a1 + b1, op1, a1 - b1, op3];
    }
    let mut cols = [[0i32; 4]; 4];
    for (out_row, in_row) in cols.iter_mut().zip(transpose(rows)) {
        let [c0, c1, c2, c3] = in_row;
        let a1 = c0 + c3;
        let b1 = c1 + c2;
        let c1v = c1 - c2;
        let d1 = c0 - c3;
        let out0 = (a1 + b1 + 7) >> 4;
        let out2 = (a1 - b1 + 7) >> 4;
        let out1 = ((c1v * 2217 + d1 * 5352 + 12000) >> 16) + i32::from(d1 != 0);
        let out3 = (d1 * 2217 - c1v * 5352 + 51000) >> 16;
        *out_row = [out0, out1, out2, out3];
    }
    to_flat(transpose(cols))
}

/// The forward Walsh-Hadamard transform, `libvpx`'s `vp8_short_walsh4x4_c`
/// (`vp8/encoder/dct.c`, same provenance as [`forward_dct`]) — the partner
/// [`inverse_wht`] needs. `dcs` is the 16 luma-subblock DC values (raster
/// order, one per Y subblock); output is the Y2 block's raster-order
/// coefficients.
#[must_use]
pub fn forward_wht(dcs: &[i32; 16]) -> [i32; 16] {
    let mut rows = [[0i32; 4]; 4];
    for (out_row, in_row) in rows.iter_mut().zip(to_mat(dcs)) {
        let [c0, c1, c2, c3] = in_row;
        let a1 = (c0 + c2) * 4;
        let d1 = (c1 + c3) * 4;
        let c1v = (c1 - c3) * 4;
        let b1 = (c0 - c2) * 4;
        let out0 = a1 + d1 + i32::from(a1 != 0);
        *out_row = [out0, b1 + c1v, b1 - c1v, a1 - d1];
    }
    let mut cols = [[0i32; 4]; 4];
    for (out_row, in_row) in cols.iter_mut().zip(transpose(rows)) {
        let [c0, c1, c2, c3] = in_row;
        let a1 = c0 + c2;
        let d1 = c1 + c3;
        let c1v = c1 - c3;
        let b1 = c0 - c2;
        let mut a2 = a1 + d1;
        let mut b2 = b1 + c1v;
        let mut c2v = b1 - c1v;
        let mut d2 = a1 - d1;
        a2 += i32::from(a2 < 0);
        b2 += i32::from(b2 < 0);
        c2v += i32::from(c2v < 0);
        d2 += i32::from(d2 < 0);
        *out_row = [(a2 + 3) >> 3, (b2 + 3) >> 3, (c2v + 3) >> 3, (d2 + 3) >> 3];
    }
    to_flat(transpose(cols))
}

/// Round `coeff` to the nearest multiple of `step` (its quantised level
/// times `step`), returning the level. `step <= 0` (never legitimate, but
/// callers pass a per-macroblock table looked up by index) quantises to
/// zero rather than dividing by zero. A plain nearest-integer quantiser —
/// simpler than `libvpx`'s zero-bin/zbin-boost run-length scheme, and not
/// meant to match it: `AGENT-CONSTRAINTS.md`'s owner ruling is explicit that
/// byte-exactness against the reference encoder is not the bar, and this
/// crate's dequantiser (this module, above) only ever multiplies a level
/// back out, so any rounding rule that inverts it is a valid encoder.
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "nearest-integer quantisation, not a size split"
)]
fn quantize_one(coeff: i32, step: i32) -> i32 {
    if step <= 0 {
        return 0;
    }
    let sign = if coeff < 0 { -1 } else { 1 };
    let level = (coeff.abs() + step / 2) / step;
    sign * level
}

/// Quantise one 4x4 block's forward-transformed coefficients: position 0
/// (raster DC) by `dc`, every other position by `ac`.
#[must_use]
pub fn quantize_block(coeffs: &[i32; 16], dc: i32, ac: i32) -> [i32; 16] {
    let mut out = [0i32; 16];
    for (i, (o, &c)) in out.iter_mut().zip(coeffs.iter()).enumerate() {
        *o = quantize_one(c, if i == 0 { dc } else { ac });
    }
    out
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
        #[allow(
            clippy::integer_division,
            reason = "matches the spec formula under test"
        )]
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

    #[test]
    fn forward_dct_of_all_zero_has_a_single_rounding_bias_artifact() {
        // Not the all-zero output a linear transform would suggest:
        // `libvpx`'s real fixed-point `vp8_short_fdct4x4_c` adds an
        // unconditional rounding-bias constant (14500/7500/12000/51000)
        // before shifting, so an exactly-zero residue still produces one
        // nonzero coefficient at position 1. Confirmed against the literal
        // flat-index port below, not merely asserted.
        let out = forward_dct(&[0; 16]);
        let mut expected = [0; 16];
        if let Some(c) = expected.get_mut(1) {
            *c = 1;
        }
        assert_eq!(out, expected);
    }

    #[test]
    fn forward_wht_of_all_zero_is_all_zero() {
        assert_eq!(forward_wht(&[0; 16]), [0; 16]);
    }

    #[test]
    fn quantize_then_dequantize_then_inverse_recovers_a_flat_residue_within_one_quant_step() {
        // A residue block that is a constant offset should round-trip
        // through forward DCT -> quantize -> dequantize -> inverse DCT
        // close to the original constant (DCT preserves a flat signal
        // almost entirely in the DC coefficient).
        let residue = [10i32; 16];
        let freq = forward_dct(&residue);
        let q = quantize_block(&freq, 8, 8);
        let mut dequant = [0i32; 16];
        for (d, &c) in dequant.iter_mut().zip(q.iter()) {
            *d = c * 8;
        }
        let back = inverse_dct(&dequant);
        for &v in &back {
            assert!((v - 10).abs() <= 4, "expected ~10, got {v}");
        }
    }

    #[test]
    fn quantize_one_rounds_to_nearest_and_keeps_sign() {
        assert_eq!(quantize_one(0, 8), 0);
        assert_eq!(quantize_one(4, 8), 1); // (4+4)/8 = 1
        assert_eq!(quantize_one(3, 8), 0); // (3+4)/8 = 0
        assert_eq!(quantize_one(-4, 8), -1);
        assert_eq!(quantize_one(100, 0), 0);
    }

    proptest::proptest! {
        #[test]
        fn forward_transforms_never_panic(coeffs in proptest::collection::vec(-255i32..=255, 16)) {
            let mut arr = [0i32; 16];
            arr.copy_from_slice(&coeffs);
            let _ = forward_dct(&arr);
            let _ = forward_wht(&arr);
        }
    }

    /// A literal, flat-indexed transcription of `libvpx`'s
    /// `vp8_short_fdct4x4_c`/`vp8_short_walsh4x4_c`, kept only as a
    /// differential oracle for [`forward_dct`]/[`forward_wht`]'s
    /// transpose-based restructuring: two implementations shaped
    /// differently enough (raw index arithmetic vs. matrix
    /// transpose-around-a-shared-pass) that a transcription slip in either
    /// one is very unlikely to agree with the other by accident, unlike two
    /// verbatim copies of the same code (`AGENT-CONSTRAINTS.md`'s "an oracle
    /// you wrote shares your misreading").
    #[allow(
        clippy::indexing_slicing,
        reason = "test-only literal port, mirroring the C source's own indexing"
    )]
    mod literal_reference {
        pub(super) fn fdct(input: &[i32; 16]) -> [i32; 16] {
            let mut tmp = [0i32; 16];
            for i in 0..4 {
                let ip = [
                    input[i * 4],
                    input[i * 4 + 1],
                    input[i * 4 + 2],
                    input[i * 4 + 3],
                ];
                let a1 = (ip[0] + ip[3]) * 8;
                let b1 = (ip[1] + ip[2]) * 8;
                let c1 = (ip[1] - ip[2]) * 8;
                let d1 = (ip[0] - ip[3]) * 8;
                tmp[i * 4] = a1 + b1;
                tmp[i * 4 + 2] = a1 - b1;
                tmp[i * 4 + 1] = (c1 * 2217 + d1 * 5352 + 14500) >> 12;
                tmp[i * 4 + 3] = (d1 * 2217 - c1 * 5352 + 7500) >> 12;
            }
            let mut out = [0i32; 16];
            for i in 0..4 {
                let a1 = tmp[i] + tmp[i + 12];
                let b1 = tmp[i + 4] + tmp[i + 8];
                let c1 = tmp[i + 4] - tmp[i + 8];
                let d1 = tmp[i] - tmp[i + 12];
                out[i] = (a1 + b1 + 7) >> 4;
                out[i + 8] = (a1 - b1 + 7) >> 4;
                out[i + 4] = ((c1 * 2217 + d1 * 5352 + 12000) >> 16) + i32::from(d1 != 0);
                out[i + 12] = (d1 * 2217 - c1 * 5352 + 51000) >> 16;
            }
            out
        }

        pub(super) fn fwht(input: &[i32; 16]) -> [i32; 16] {
            let mut tmp = [0i32; 16];
            for i in 0..4 {
                let ip = [
                    input[i * 4],
                    input[i * 4 + 1],
                    input[i * 4 + 2],
                    input[i * 4 + 3],
                ];
                let a1 = (ip[0] + ip[2]) * 4;
                let d1 = (ip[1] + ip[3]) * 4;
                let c1 = (ip[1] - ip[3]) * 4;
                let b1 = (ip[0] - ip[2]) * 4;
                tmp[i * 4] = a1 + d1 + i32::from(a1 != 0);
                tmp[i * 4 + 1] = b1 + c1;
                tmp[i * 4 + 2] = b1 - c1;
                tmp[i * 4 + 3] = a1 - d1;
            }
            let mut out = [0i32; 16];
            for i in 0..4 {
                let a1 = tmp[i] + tmp[i + 8];
                let d1 = tmp[i + 4] + tmp[i + 12];
                let c1 = tmp[i + 4] - tmp[i + 12];
                let b1 = tmp[i] - tmp[i + 8];
                let mut a2 = a1 + d1;
                let mut b2 = b1 + c1;
                let mut c2 = b1 - c1;
                let mut d2 = a1 - d1;
                a2 += i32::from(a2 < 0);
                b2 += i32::from(b2 < 0);
                c2 += i32::from(c2 < 0);
                d2 += i32::from(d2 < 0);
                out[i] = (a2 + 3) >> 3;
                out[i + 4] = (b2 + 3) >> 3;
                out[i + 8] = (c2 + 3) >> 3;
                out[i + 12] = (d2 + 3) >> 3;
            }
            out
        }
    }

    proptest::proptest! {
        #[test]
        fn forward_dct_matches_the_literal_flat_index_port(coeffs in proptest::collection::vec(-255i32..=255, 16)) {
            let mut arr = [0i32; 16];
            arr.copy_from_slice(&coeffs);
            assert_eq!(forward_dct(&arr), literal_reference::fdct(&arr));
        }

        #[test]
        fn forward_wht_matches_the_literal_flat_index_port(coeffs in proptest::collection::vec(-2048i32..=2047, 16)) {
            let mut arr = [0i32; 16];
            arr.copy_from_slice(&coeffs);
            assert_eq!(forward_wht(&arr), literal_reference::fwht(&arr));
        }
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
