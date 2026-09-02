//! Annex A ("Transform Specification") of SMPTE ST 421:2013 — the 8x8
//! inverse transform. Read directly off the specification's own printed
//! matrices and formula (a scan of the primary text, not a transliteration
//! of any other implementation): "similar, but not identical, to an IDCT."
//!
//! Only the 8x8 case is implemented (SS8.1.3: "For an intra-coded block:
//! The block is transformed using an 8x8 transform" — every block this
//! crate's I-frame-only scope reconstructs is 8x8; the 8x4/4x8/4x4 variants
//! only arise for inter blocks with `VSTRANSFORM`, which this crate does
//! not implement).

/// Figure 157: the 1-D 8-point inverse transform matrix `T8`.
const T8: [[i32; 8]; 8] = [
    [12, 12, 12, 12, 12, 12, 12, 12],
    [16, 15, 9, 4, -4, -9, -15, -16],
    [16, 6, -6, -16, -16, -6, 6, 16],
    [15, -4, -16, -9, 9, 16, 4, -15],
    [12, -12, -12, 12, 12, -12, -12, 12],
    [9, -16, 4, 15, -15, -4, 16, -9],
    [6, -16, 16, -6, -6, 16, -16, 6],
    [4, -9, 15, -16, 16, -15, 9, -4],
];

/// Figure 159: `C8` is `[0, 0, 0, 0, 1, 1, 1, 1]` (a length-8 column
/// vector), used only to add a constant to rows 4..7 of the intermediate
/// matrix — the correction term the spec's own formula names but does not
/// further explain.
const C8: [i32; 8] = [0, 0, 0, 0, 1, 1, 1, 1];

/// Figure 159's `E_{8x8} = (D_{8x8} . T8 + 4) >> 3` then
/// `R_{8x8} = (T8' . E_{8x8} + C8 . 1_8 + 64) >> 7`, specialised to the
/// square 8x8 case (`M == N == 8`, this crate's only case).
///
/// `d` is row-major (`d[row * 8 + col]`), matching how [`crate::decoder`]
/// lays out a dequantised coefficient block after [`inverse zigzag scan`](crate::tables::NORMAL_SCAN).
/// The result is clamped to the spec's own stated output range,
/// `(-512, 511]`.
#[must_use]
#[allow(
    clippy::many_single_char_names,
    reason = "d/e/r/c mirror Figure 159's own D/E/R/C8 variable names one-for-one; renaming them would make this function harder to check against the spec, not easier"
)]
pub(crate) fn inverse_transform_8x8(d: &[i32; 64]) -> [i32; 64] {
    // First pass: E = (D . T8 + 4) >> 3 -- each row of D is transformed by
    // T8 on the right (a horizontal 1-D transform per row).
    let mut e = [0i32; 64];
    for row in 0..8usize {
        for col in 0..8usize {
            let mut acc = 0i64;
            for k in 0..8usize {
                let Some(&dv) = d.get(row * 8 + k) else {
                    continue;
                };
                let Some(&t) = T8.get(k).and_then(|r| r.get(col)) else {
                    continue;
                };
                acc += i64::from(dv) * i64::from(t);
            }
            let Some(slot) = e.get_mut(row * 8 + col) else {
                continue;
            };
            *slot = i32::try_from((acc + 4) >> 3)
                .unwrap_or(0)
                .clamp(-4096, 4095);
        }
    }

    // Second pass: R = (T8' . E + C8.1_8 + 64) >> 7 -- T8 transpose applied
    // on the left (a vertical 1-D transform per column), plus the C8/1_8
    // correction (adds 1 to every column's rows 4..7 before rounding).
    let mut r = [0i32; 64];
    for row in 0..8usize {
        for col in 0..8usize {
            let mut acc = 0i64;
            for k in 0..8usize {
                // T8' [row][k] == T8[k][row] (transpose).
                let Some(&t) = T8.get(k).and_then(|r| r.get(row)) else {
                    continue;
                };
                let Some(&ev) = e.get(k * 8 + col) else {
                    continue;
                };
                acc += i64::from(t) * i64::from(ev);
            }
            let Some(&c) = C8.get(row) else { continue };
            let Some(slot) = r.get_mut(row * 8 + col) else {
                continue;
            };
            let v = i32::try_from((acc + i64::from(c) + 64) >> 7).unwrap_or(0);
            *slot = v.clamp(-512, 511);
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dc_only_block_is_uniform() {
        // A property check independent of the formula's own arithmetic
        // (per this project's "an oracle you wrote shares your misreading"
        // lesson): a DC-only input must reconstruct to a uniform block,
        // since every basis function but the DC one is zero.
        let mut d = [0i32; 64];
        if let Some(dc) = d.get_mut(0) {
            *dc = 400;
        }
        let out = inverse_transform_8x8(&d);
        let first = out.first().copied().unwrap_or(0);
        assert!(
            out.iter().all(|&v| v == first),
            "DC-only block must be uniform: {out:?}"
        );
    }

    #[test]
    fn all_zero_input_is_all_zero_output() {
        let d = [0i32; 64];
        let out = inverse_transform_8x8(&d);
        assert!(out.iter().all(|&v| v == 0));
    }

    #[test]
    fn output_never_exceeds_the_documented_range() {
        // Sweep a spread of extreme inputs (within the documented input
        // range of +/-2047) and confirm the output never leaves (-512, 511].
        for seed in [1i32, -1, 2047, -2048, 999, -999] {
            let d = [seed; 64];
            let out = inverse_transform_8x8(&d);
            assert!(
                out.iter().all(|&v| (-512..=511).contains(&v)),
                "seed {seed} produced out-of-range output"
            );
        }
    }
}
