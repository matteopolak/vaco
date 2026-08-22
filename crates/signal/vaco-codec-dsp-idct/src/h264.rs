//! ITU-T H.264 (ISO/IEC 14496-10) (02/2016) §8.5.10, §8.5.11.1, §8.5.12.2,
//! §8.5.13.2 — the inverse transforms only.
//!
//! Each of these clauses is split, in the standard, into a **scaling**
//! process (QP-dependent, uses the `LevelScale4x4`/`LevelScale8x8` tables) and
//! a **transformation** process (a pure function of the already-scaled
//! coefficients). This module implements only the transformation half; see
//! the crate-level docs for why.
//!
//! # Arithmetic
//!
//! The standard's own butterfly is a fixed sequence of add/subtract/shift —
//! no multiplication table, which is why H.264's 4×4 and 8×8 transforms carry
//! essentially no transcription risk: the constants involved are `1` and `2`
//! (as shift amounts), not a hand-tuned coefficient table. Every add/subtract
//! here uses [`i32::wrapping_add`]/[`i32::wrapping_sub`] rather than `+`/`-`:
//! the standard says "the bitstream shall not contain data" that pushes an
//! intermediate value out of its documented range, but this crate must not
//! panic even when that requirement is violated by adversarial input, and
//! wrapping arithmetic is bit-identical to plain arithmetic for every input
//! that respects the range (plan 13 §2.2.1).

use crate::util::{from_flat, map_rows, to_flat, transpose};
use vaco_tx::fixed::round_shift;

/// The 1-D inverse transform for one row (or, transposed, one column) of a
/// 4×4 residual block. ITU-T H.264 §8.5.12.2, eq. (8-338)–(8-345).
#[must_use]
fn row4(d: [i32; 4]) -> [i32; 4] {
    let [d0, d1, d2, d3] = d;
    let e0 = d0.wrapping_add(d2);
    let e1 = d0.wrapping_sub(d2);
    let e2 = (d1 >> 1).wrapping_sub(d3);
    let e3 = d1.wrapping_add(d3 >> 1);
    [
        e0.wrapping_add(e3),
        e1.wrapping_add(e2),
        e1.wrapping_sub(e2),
        e0.wrapping_sub(e3),
    ]
}

/// The 1-D inverse transform for one row/column of an 8×8 residual block.
/// ITU-T H.264 §8.5.13.2, eq. (8-358)–(8-381) (also used, transposed, for
/// eq. (8-382)–(8-405): the standard applies the identical 1-D operator to
/// rows and then to columns).
#[must_use]
fn row8(d: [i32; 8]) -> [i32; 8] {
    let [d0, d1, d2, d3, d4, d5, d6, d7] = d;

    // `wrapping_neg`, not unary `-`: `-i32::MIN` overflows and unary negation
    // panics under the fuzz profile's overflow checks (plan 13 §2.2.1) — the
    // exact shape of bug the brief calls out from `vaco-scale`'s fuzzer.
    let e0 = d0.wrapping_add(d4);
    let e1 = d3
        .wrapping_neg()
        .wrapping_add(d5)
        .wrapping_sub(d7)
        .wrapping_sub(d7 >> 1);
    let e2 = d0.wrapping_sub(d4);
    let e3 = d1.wrapping_add(d7).wrapping_sub(d3).wrapping_sub(d3 >> 1);
    let e4 = (d2 >> 1).wrapping_sub(d6);
    let e5 = d1
        .wrapping_neg()
        .wrapping_add(d7)
        .wrapping_add(d5)
        .wrapping_add(d5 >> 1);
    let e6 = d2.wrapping_add(d6 >> 1);
    let e7 = d3.wrapping_add(d5).wrapping_add(d1).wrapping_add(d1 >> 1);

    let f0 = e0.wrapping_add(e6);
    let f1 = e1.wrapping_add(e7 >> 2);
    let f2 = e2.wrapping_add(e4);
    let f3 = e3.wrapping_add(e5 >> 2);
    let f4 = e2.wrapping_sub(e4);
    let f5 = (e3 >> 2).wrapping_sub(e5);
    let f6 = e0.wrapping_sub(e6);
    let f7 = e7.wrapping_sub(e1 >> 2);

    [
        f0.wrapping_add(f7),
        f2.wrapping_add(f5),
        f4.wrapping_add(f3),
        f6.wrapping_add(f1),
        f6.wrapping_sub(f1),
        f4.wrapping_sub(f3),
        f2.wrapping_sub(f5),
        f0.wrapping_sub(f7),
    ]
}

/// Apply `row1d` to every row, then (via transpose) to every column, of a
/// flattened `N x N` block — the "first horizontal, then vertical" shape both
/// §8.5.12.2 and §8.5.13.2 specify, with no rounding in between (H.264 rounds
/// only once, after both passes).
#[must_use]
fn separable<const N: usize>(
    coeffs: &[i32],
    row1d: impl Fn([i32; N]) -> [i32; N] + Copy,
) -> [[i32; N]; N] {
    let rows = map_rows(from_flat::<N>(coeffs), row1d);
    transpose(&map_rows(transpose(&rows), row1d))
}

/// Inverse transform for a 4×4 residual block of already-scaled coefficients.
/// ITU-T H.264 §8.5.12.2.
///
/// `coeffs` and the result are row-major, `d[i][j]` at index `4*i + j`.
#[must_use]
pub fn idct4x4(coeffs: &[i32; 16]) -> [i32; 16] {
    let h = separable::<4>(coeffs, row4);
    let mut out = [0i32; 16];
    to_flat(&h, &mut out);
    for v in &mut out {
        *v = round_shift(i64::from(*v), 6);
    }
    out
}

/// Inverse transform for an 8×8 residual block of already-scaled
/// coefficients (High Profile). ITU-T H.264 §8.5.13.2.
#[must_use]
pub fn idct8x8(coeffs: &[i32; 64]) -> [i32; 64] {
    let m = separable::<8>(coeffs, row8);
    let mut out = [0i32; 64];
    to_flat(&m, &mut out);
    for v in &mut out {
        *v = round_shift(i64::from(*v), 6);
    }
    out
}

/// `H * v` for the 4-point Hadamard `H = [[1,1,1,1],[1,1,-1,-1],[1,-1,-1,1],[1,-1,1,-1]]`
/// used by both the luma 16×16 DC transform (§8.5.10) and, on one axis, the
/// 4:2:2 chroma DC transform (§8.5.11.1). `H` is symmetric, so applying it as
/// the row operator on both passes of [`separable`] computes exactly `H c H`
/// (the sandwich the standard writes), not merely something proportional to
/// it — checked in `tests/golden.rs` against a direct matrix-sandwich
/// evaluation.
#[must_use]
fn hadamard4(v: [i32; 4]) -> [i32; 4] {
    let [v0, v1, v2, v3] = v;
    [
        v0.wrapping_add(v1).wrapping_add(v2).wrapping_add(v3),
        v0.wrapping_add(v1).wrapping_sub(v2).wrapping_sub(v3),
        v0.wrapping_sub(v1).wrapping_sub(v2).wrapping_add(v3),
        v0.wrapping_sub(v1).wrapping_add(v2).wrapping_sub(v3),
    ]
}

/// `A * v` for the 2-point Hadamard `A = [[1,1],[1,-1]]`, used by the 4:2:0
/// chroma DC transform (§8.5.11.1, `ChromaArrayType == 1`) and, on the other
/// axis, the 4:2:2 case.
#[must_use]
fn hadamard2(v: [i32; 2]) -> [i32; 2] {
    let [v0, v1] = v;
    [v0.wrapping_add(v1), v0.wrapping_sub(v1)]
}

/// Inverse transform for the 4×4 `Intra_16x16` luma DC coefficients.
/// ITU-T H.264 §8.5.10, eq. (8-320): `f = H c H`.
///
/// This is the transform only — the QP-dependent scaling of eq. (8-321)/(8-322)
/// is a codec-level concern (it needs `LevelScale4x4` and `qP`) and is not
/// implemented here.
#[must_use]
pub fn luma_dc_hadamard4x4(coeffs: &[i32; 16]) -> [i32; 16] {
    let f = separable::<4>(coeffs, hadamard4);
    let mut out = [0i32; 16];
    to_flat(&f, &mut out);
    out
}

/// Inverse transform for the 2×2 chroma DC coefficients (`ChromaArrayType == 1`,
/// i.e. 4:2:0). ITU-T H.264 §8.5.11.1, eq. (8-324): `f = A c A`.
#[must_use]
pub fn chroma_dc_hadamard2x2(coeffs: &[i32; 4]) -> [i32; 4] {
    let f = separable::<2>(coeffs, hadamard2);
    let mut out = [0i32; 4];
    to_flat(&f, &mut out);
    out
}

/// Inverse transform for the 2×4 chroma DC coefficients (`ChromaArrayType == 2`,
/// i.e. 4:2:2). ITU-T H.264 §8.5.11.1, eq. (8-325): a 4-point Hadamard down
/// the 4-long axis (`i`, rows here) and a 2-point Hadamard across the 2-long
/// axis (`j`, columns here), reusing the same two building blocks as
/// [`luma_dc_hadamard4x4`] and [`chroma_dc_hadamard2x2`].
///
/// `coeffs` and the result are row-major with 2 columns: `c[i][j]` at index
/// `2*i + j`, `i = 0..=3`, `j = 0..=1`.
#[must_use]
pub fn chroma_dc_hadamard2x4(coeffs: &[i32; 8]) -> [i32; 8] {
    // Rows (length 2) get the 2-point Hadamard; columns (length 4) get the
    // 4-point Hadamard — the non-square analogue of `separable`.
    let rows: [[i32; 2]; 4] = core::array::from_fn(|r| {
        let start = r * 2;
        hadamard2(core::array::from_fn(|c| {
            coeffs.get(start + c).copied().unwrap_or(0)
        }))
    });
    let mut cols_out = [[0i32; 2]; 4];
    for c in 0..2 {
        let column: [i32; 4] =
            core::array::from_fn(|r| rows.get(r).and_then(|row| row.get(c)).copied().unwrap_or(0));
        let transformed = hadamard4(column);
        for (slot, v) in cols_out.iter_mut().zip(transformed.iter()) {
            if let Some(dst) = slot.get_mut(c) {
                *dst = *v;
            }
        }
    }
    let mut out = [0i32; 8];
    for (r, row) in cols_out.iter().enumerate() {
        let start = r * 2;
        for (c, v) in row.iter().enumerate() {
            if let Some(slot) = out.get_mut(start + c) {
                *slot = *v;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dc_only_input_gives_a_uniform_4x4_block() {
        // Only d[0][0] set: the standard's algebra (§8.5.12.2) guarantees a
        // spatially uniform residual — the DC coefficient carries no
        // frequency information for the transform to shape.
        let mut c = [0i32; 16];
        if let Some(v) = c.first_mut() {
            *v = 77;
        }
        let r = idct4x4(&c);
        let expected = round_shift(77, 6);
        assert!(r.iter().all(|&v| v == expected), "{r:?}");
    }

    #[test]
    fn dc_only_input_gives_a_uniform_8x8_block() {
        let mut c = [0i32; 64];
        if let Some(v) = c.first_mut() {
            *v = 123;
        }
        let r = idct8x8(&c);
        let expected = round_shift(123, 6);
        assert!(r.iter().all(|&v| v == expected), "{r:?}");
    }

    #[test]
    fn all_zero_is_all_zero() {
        assert_eq!(idct4x4(&[0; 16]), [0; 16]);
        assert_eq!(idct8x8(&[0; 64]), [0; 64]);
        assert_eq!(luma_dc_hadamard4x4(&[0; 16]), [0; 16]);
        assert_eq!(chroma_dc_hadamard2x2(&[0; 4]), [0; 4]);
        assert_eq!(chroma_dc_hadamard2x4(&[0; 8]), [0; 8]);
    }

    #[test]
    fn extreme_inputs_never_panic() {
        let _ = idct4x4(&[i32::MIN; 16]);
        let _ = idct4x4(&[i32::MAX; 16]);
        let _ = idct8x8(&[i32::MIN; 64]);
        let _ = idct8x8(&[i32::MAX; 64]);
        let _ = luma_dc_hadamard4x4(&[i32::MIN; 16]);
        let _ = chroma_dc_hadamard2x2(&[i32::MIN; 4]);
        let _ = chroma_dc_hadamard2x4(&[i32::MIN; 8]);
    }
}
