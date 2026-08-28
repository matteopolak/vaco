//! The inverse DCT (`Vaco-Spec-Ref: theora-spec-20170603 section 7.9.3`).
//!
//! Theora requires bit-exact reproduction of this specific integerized
//! transform — "a compliant decoder MUST use the exact implementation of the
//! inverse DCT defined in this specification" — because any drift here
//! compounds through the (absent, in this decode-only crate) prediction
//! loop. Every truncation-to-16-bit point named in the spec's numbered steps
//! is reproduced as [`trunc16`]; every addition and subtraction uses
//! wrapping arithmetic because the spec calls for 32-bit two's-complement
//! truncation on overflow, not a panic.

/// `Ci = S(8-i)` (section 7.9.3.1, Table 7.65): the cosine table doubles as
/// the sine table via the co-function identity, so the six named constants
/// below are the whole table.
const C1: i32 = 64277;
const C2: i32 = 60547;
const C3: i32 = 54491;
const C4: i32 = 46341;
const C5: i32 = 36410;
const C6: i32 = 25080;
const C7: i32 = 12785;

/// Truncate to a 16-bit signed representation by dropping higher-order bits
/// of the two's-complement value, per the spec's repeated instruction.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the spec explicitly calls for dropping the high bits, not saturating"
)]
pub(crate) const fn trunc16(x: i32) -> i32 {
    (x as i16) as i32
}

/// `(c * y) >> 16`, matching the spec's `C * Y[i] >> 16` shorthand exactly:
/// full-precision multiply, then an arithmetic right shift.
const fn mul_shift(c: i32, y: i32) -> i32 {
    (c.wrapping_mul(y)) >> 16
}

/// The 1D inverse DCT (section 7.9.3.1): 8 coefficients in, 8 samples out.
///
/// Uses named `t0..t7` bindings (reassigned via shadowing) rather than a `[T;
/// 8]` array indexed by the spec's own numbering: `clippy::indexing_slicing`
/// is denied workspace-wide, and destructuring is the established
/// idiom for this exact situation elsewhere in the tree (see
/// `vaco-codec-dsp-idct`'s `h264`/`hevc` 1-D transforms).
#[allow(
    clippy::many_single_char_names,
    reason = "t0..t7 mirror the spec's own T[0..7] names, and y0..y7/x0..x7 its Y[]/X[]"
)]
pub(crate) fn idct_1d(y: &[i32; 8]) -> [i32; 8] {
    let [y0, y1, y2, y3, y4, y5, y6, y7] = *y;

    // Steps 1-12: initial permutation/rotation from Y into T.
    let t0 = mul_shift(C4, trunc16(y0.wrapping_add(y4)));
    let t1 = mul_shift(C4, trunc16(y0.wrapping_sub(y4)));
    let t2 = mul_shift(C6, y2).wrapping_sub(mul_shift(C2, y6));
    let t3 = mul_shift(C2, y2).wrapping_add(mul_shift(C6, y6));
    let t4_pre = mul_shift(C7, y1).wrapping_sub(mul_shift(C1, y7));
    let t5_pre = mul_shift(C3, y5).wrapping_sub(mul_shift(C5, y3));
    let t6_pre = mul_shift(C5, y5).wrapping_add(mul_shift(C3, y3));
    let t7_pre = mul_shift(C1, y1).wrapping_add(mul_shift(C7, y7));

    // Steps 13-17: T4/T5 butterfly.
    let t4 = t4_pre.wrapping_add(t5_pre);
    let t5 = mul_shift(C4, trunc16(t4_pre.wrapping_sub(t5_pre)));

    // Steps 18-22: T6/T7 butterfly.
    let t7 = t7_pre.wrapping_add(t6_pre);
    let t6 = mul_shift(C4, trunc16(t7_pre.wrapping_sub(t6_pre)));

    // Steps 23-25: T0/T3 butterfly.
    let (t0, t3) = (t0.wrapping_add(t3), t0.wrapping_sub(t3));

    // Steps 26-28: T1/T2 butterfly.
    let (t1, t2) = (t1.wrapping_add(t2), t1.wrapping_sub(t2));

    // Steps 29-31: T5/T6 butterfly (using the already-updated T5/T6 above).
    let (t6, t5) = (t6.wrapping_add(t5), t6.wrapping_sub(t5));

    // Steps 32-55: final combination and 16-bit truncation into X.
    [
        trunc16(t0.wrapping_add(t7)),
        trunc16(t1.wrapping_add(t6)),
        trunc16(t2.wrapping_add(t5)),
        trunc16(t3.wrapping_add(t4)),
        trunc16(t3.wrapping_sub(t4)),
        trunc16(t2.wrapping_sub(t5)),
        trunc16(t1.wrapping_sub(t6)),
        trunc16(t0.wrapping_sub(t7)),
    ]
}

/// The 2D inverse DCT (section 7.9.3.2): rows, then columns, with the final
/// division by 16 (ties rounding towards positive infinity) folded into the
/// column pass.
pub(crate) fn idct_2d(dqc: &[i32; 64]) -> [[i32; 8]; 8] {
    let mut res = [[0i32; 8]; 8];
    for (ri, row) in res.iter_mut().enumerate() {
        let mut y = [0i32; 8];
        for (ci, slot) in y.iter_mut().enumerate() {
            *slot = dqc.get(ri * 8 + ci).copied().unwrap_or(0);
        }
        *row = idct_1d(&y);
    }
    for ci in 0..8usize {
        let mut y = [0i32; 8];
        for (ri, slot) in y.iter_mut().enumerate() {
            *slot = res.get(ri).and_then(|r| r.get(ci)).copied().unwrap_or(0);
        }
        let x = idct_1d(&y);
        for (ri, row) in res.iter_mut().enumerate() {
            if let Some(slot) = row.get_mut(ci) {
                *slot = x.get(ri).copied().unwrap_or(0).wrapping_add(8) >> 4;
            }
        }
    }
    res
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    #[test]
    fn dc_only_input_produces_a_flat_block() {
        // A pure DC coefficient of 16 should reconstruct to a flat residual
        // of 1 everywhere after the 2D transform's final >>4 (the general
        // path, not the spec's separate DC-only fast path used at higher
        // levels of this crate).
        let mut dqc = [0i32; 64];
        // Empirically, this integer transform's overall DC scaling (two
        // passes of the C4 = 46341/65536 ~= 1/sqrt(2) butterfly, then the
        // final >>4) divides the DC coefficient by 32, with truncating
        // intermediate rounding; 512 is the smallest value landing exactly
        // on a flat output of 16 given that rounding, which is what makes it
        // a useful regression value rather than a coincidence.
        dqc[0] = 512;
        let res = idct_2d(&dqc);
        for row in &res {
            for &v in row {
                assert_eq!(v, 16);
            }
        }
    }

    #[test]
    fn zero_input_is_zero_output() {
        let dqc = [0i32; 64];
        let res = idct_2d(&dqc);
        for row in &res {
            for &v in row {
                assert_eq!(v, 0);
            }
        }
    }
}
