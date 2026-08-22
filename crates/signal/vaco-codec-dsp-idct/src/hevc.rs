//! ITU-T H.265 (ISO/IEC 23008-2) (08/2021) §8.6.4 — the inverse transform
//! only (the QP-dependent scaling of §8.6.3 is a codec-level concern; see the
//! crate-level docs).
//!
//! # One matrix, four sizes
//!
//! HEVC's whole `4x4`–`32x32` DCT-II family is one stored `32x32` integer
//! matrix ([`TRANS_MATRIX_32`]) plus eq. (8-317)'s subsampling rule, which
//! strides the *summed* index rather than the output one — see the long
//! comment on [`dct1d`] for why, and for how that was determined rather than
//! assumed (an alternative reading matches a well-known small core exactly
//! and is still wrong, which is the interesting part).
//!
//! [`TRANS_MATRIX_32`] was transcribed by script from the primary standard
//! text (ITU-T H.265 (08/2021) §8.6.4.2, eq. (8-318)–(8-321)), not typed by
//! hand, and validated two ways before use: every row is (near-)orthogonal to
//! every other row (`row_a · row_b ≈ 0`, the defining property of a DCT-like
//! basis), and row 0 is the constant DC row `64` repeated 32 times. Both
//! properties hold for the table exactly as printed in the standard; the
//! correction below is entirely in how [`dct1d`] *reads* it, not in the
//! stored values.
//!
//! The 4×4 DST-VII used for intra luma ([`DST_MATRIX_4`], eq. (8-316),
//! `trType == 1`) is unrelated to this family — a separate, dedicated 4×4
//! matrix — and is cross-confirmed against an independent public description
//! of the standard, not only the primary text.
//!
//! # Arithmetic
//!
//! Unlike H.264, HEVC's 1-D transform (eq. 8-317) is a *pure* integer matrix
//! multiplication with no truncating shift inside it — every product is
//! widened to `i64` before accumulating, so 32 terms of `i32::MAX * 90`
//! cannot overflow, and only the final narrow back to `i32` (after the
//! mid-transform shift, and again after the second pass) can saturate. The
//! mid-transform shift is *always* by 7 regardless of size or bit depth
//! (§8.6.4.1 eq. 8-314) — bit-depth dependence lives entirely in §8.6.3's
//! coefficient scaling, upstream of this module.

use crate::util::{from_flat, map_rows, to_flat, transpose};
use vaco_tx::fixed::{clamp_i32, round_shift};

include!("hevc_matrix.rs");

/// `H.265 (08/2021) §8.6.4.2 eq. (8-316)`, the DST-VII used only for
/// `nTbS == 4` intra luma (`trType == 1`).
const DST_MATRIX_4: [[i32; 4]; 4] = [
    [29, 55, 74, 84],
    [74, 74, 0, -74],
    [84, -29, -74, 55],
    [55, -84, 74, -29],
];

/// The clip range applied between the two 1-D passes and (by convention of
/// this crate; see [`idct2d`]) to the final output — `CoeffMin`/`CoeffMax` in
/// the standard (§7.4.9.11 eq. 7-27/7-28), derived from `log2TransformRange`.
///
/// [`ClipRange::non_extended`] is the overwhelmingly common case
/// (`extended_precision_processing_flag == 0`, any bit depth): the standard
/// fixes `log2TransformRange` at 15 regardless of bit depth in that case, so
/// the range does not depend on `BitDepth`. A decoder that has set
/// `extended_precision_processing_flag` must compute its own range from
/// eq. (8-304) and pass it explicitly.
#[derive(Clone, Copy, Debug)]
pub struct ClipRange {
    pub min: i32,
    pub max: i32,
}

impl ClipRange {
    /// `log2TransformRange == 15`: `CoeffMin/Max = -2^15 .. 2^15 - 1`.
    #[must_use]
    pub const fn non_extended() -> Self {
        Self {
            min: -32768,
            max: 32767,
        }
    }

    #[inline]
    fn clip(self, x: i32) -> i32 {
        // `self.min <= self.max` for every value this type is constructed
        // with in this crate, so `i32::clamp`'s precondition holds.
        x.clamp(self.min, self.max)
    }
}

/// Row stride into [`TRANS_MATRIX_32`] for the `N`-point transform, per
/// eq. (8-317): `2^(5 - log2(N))`, i.e. `32/N`. Written as a match rather
/// than a division (`clippy::integer_division` is denied workspace-wide) —
/// and it doubles as the domain check: anything other than 4/8/16/32 falls
/// back to stride 1, a defined-but-meaningless result rather than a panic,
/// since the standard does not define this transform for other sizes.
#[must_use]
const fn row_stride(n: usize) -> usize {
    match n {
        4 => 8,
        8 => 4,
        16 => 2,
        // 32 (the identity stride) and every other value share this arm:
        // `n = 32` is legitimately stride 1, and anything else is out of the
        // standard's domain, for which any defined value is as good as
        // another.
        _ => 1,
    }
}

/// The 1-D `N`-point HEVC DCT-II, `N` one of 4, 8, 16, 32. ITU-T H.265
/// §8.6.4.2 eq. (8-317).
///
/// **On the transpose.** [`TRANS_MATRIX_32`] is transcribed exactly as
/// printed (row `m`, column `n`), and reproduces the well-known small cores
/// when read as `M[i*(32/N)][j]` (row strided by output index, column
/// unstrided by input index) — that reading was checked first and matched
/// the H.264 8-point core exactly. But that reading fails an independent,
/// unarguable requirement: feeding a pure DC coefficient (`x = [c, 0, …]`,
/// the *only* nonzero frequency) through a correct inverse transform must
/// produce a spatially uniform block, because the frequency-0 basis function
/// is the constant function by definition. It does not — `M[i*(32/N)][0]`
/// varies with `i`. The reading that **does** satisfy DC-uniformity, checked
/// computationally for all of `N ∈ {4, 8, 16, 32}` before being trusted, is
/// the transposed one: `y[i] = Σⱼ M[j·(32/N)][i]·x[j]` — the stride lands on
/// the *summed* index, not the output index. Kept here rather than fixed by
/// transposing the stored table, so the table stays a literal transcription
/// and the (checkable, tested) correction is visible at the point of use.
#[must_use]
pub fn dct1d<const N: usize>(x: &[i32; N]) -> [i32; N] {
    let stride = row_stride(N);
    core::array::from_fn(|i| {
        let acc: i64 = x
            .iter()
            .enumerate()
            .map(|(j, &xv)| {
                let row = TRANS_MATRIX_32.get(j * stride).unwrap_or(&[0; 32]);
                let mv = row.get(i).copied().unwrap_or(0);
                i64::from(xv) * i64::from(mv)
            })
            .sum();
        clamp_i32(acc)
    })
}

/// The 1-D 4-point DST-VII used for intra luma. ITU-T H.265 §8.6.4.2
/// eq. (8-316) — the same equation shape as eq. (8-317) (`trType` only
/// switches which table is read), so the transpose [`dct1d`] needed applies
/// here too: `y[i] = Σⱼ DST_MATRIX_4[j][i]·x[j]`.
#[must_use]
pub fn dst1d(x: &[i32; 4]) -> [i32; 4] {
    core::array::from_fn(|i| {
        let acc: i64 = x
            .iter()
            .enumerate()
            .map(|(j, &xv)| {
                let row = DST_MATRIX_4.get(j).unwrap_or(&[0; 4]);
                let mv = row.get(i).copied().unwrap_or(0);
                i64::from(xv) * i64::from(mv)
            })
            .sum();
        clamp_i32(acc)
    })
}

/// The shared 2-D engine: transform every column, shift-and-clip, transform
/// every row (ITU-T H.265 §8.6.4.1, the order the standard specifies — column
/// first, unlike H.264's row-first order, and *with* a clip/shift between the
/// two passes, unlike H.264's none).
///
/// `coeffs`/`out` are row-major `N x N`; a length other than `N*N` is handled
/// by zero-padding/truncating rather than panicking (see [`crate::util`]).
fn idct2d<const N: usize>(
    coeffs: &[i32],
    out: &mut [i32],
    row1d: impl Fn([i32; N]) -> [i32; N] + Copy,
    clip: ClipRange,
) {
    let m = from_flat::<N>(coeffs);
    // Step 1: each column of `m` is a row of `transpose(m)`.
    let e_t = map_rows(transpose(&m), row1d);
    // Step 2: g[x][y] = Clip3(coeffMin, coeffMax, (e[x][y] + 64) >> 7), eq. (8-314).
    let g_t = e_t.map(|row| row.map(|v| clip.clip(round_shift(i64::from(v), 7))));
    // Undo the transpose so pass 2 walks the *original* rows.
    let g = transpose(&g_t);
    // Step 3: each row of `g` is transformed directly into the residual —
    // the standard applies no further shift or clip here (§8.6.4.1 step 3).
    // `row1d` already saturates its `i64` accumulator into `i32`, so the
    // result can never overflow even though it is not re-clipped to
    // `coeffMin/coeffMax`.
    let r = map_rows(g, row1d);
    to_flat(&r, out);
}

/// 2-D inverse DCT-II for an `N x N` block, `N` one of 4, 8, 16, 32.
pub fn idct2d_dct<const N: usize>(coeffs: &[i32], out: &mut [i32], clip: ClipRange) {
    idct2d::<N>(coeffs, out, |x| dct1d::<N>(&x), clip);
}

/// 2-D inverse DST-VII for a 4×4 intra-luma block (`trType == 1`).
pub fn idct2d_dst4(coeffs: &[i32], out: &mut [i32], clip: ClipRange) {
    idct2d::<4>(coeffs, out, |x| dst1d(&x), clip);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dot(a: &[i32; 32], b: &[i32; 32]) -> i64 {
        a.iter()
            .zip(b.iter())
            .map(|(&x, &y)| i64::from(x) * i64::from(y))
            .sum()
    }

    #[test]
    fn trans_matrix_row0_is_constant_dc() {
        assert_eq!(TRANS_MATRIX_32.first(), Some(&[64i32; 32]));
    }

    #[test]
    fn trans_matrix_is_near_orthogonal() {
        // Hand-tuned integer approximation, not a perfectly orthogonal real
        // DCT matrix, so cross terms are small relative to the diagonal
        // (~131072) rather than exactly zero.
        for (a, row_a) in TRANS_MATRIX_32.iter().enumerate() {
            for (b, row_b) in TRANS_MATRIX_32.iter().enumerate() {
                if a == b {
                    continue;
                }
                let d = dot(row_a, row_b).abs();
                assert!(d < 30_000, "row {a} . row {b} = {d}");
            }
        }
    }

    /// Probe `dct1d`'s impulse response and lay the results out as
    /// `matrix[i][k]` = "output `i` when only frequency `k` is 1" — i.e. the
    /// matrix `dct1d` actually computes, read off empirically rather than
    /// assumed. This is orientation-*revealing* (unlike checking against a
    /// remembered table, which is orientation-blind: see the long comment on
    /// [`dct1d`] for how that blindness hid the transpose bug this test would
    /// have caught had it existed then).
    fn probe<const N: usize>() -> [[i32; N]; N] {
        let unit = |k: usize| -> [i32; N] { core::array::from_fn(|i| i32::from(i == k)) };
        let by_k: [[i32; N]; N] = core::array::from_fn(|k| dct1d(&unit(k)));
        crate::util::transpose(&by_k)
    }

    #[test]
    fn dct1d_4_reduces_to_the_well_known_4_point_core_transposed() {
        // These are the well-known H.264/HEVC 4-point core's *columns* (its
        // rows read [64,64,64,64;83,36,-36,-83;64,-64,-64,64;36,-83,83,-36] —
        // see the module docs for why the inverse transform needs it this
        // way around).
        assert_eq!(
            probe::<4>(),
            [
                [64, 83, 64, 36],
                [64, 36, -64, -83],
                [64, -36, -64, 83],
                [64, -83, 64, -36]
            ]
        );
    }

    #[test]
    fn dct1d_8_reduces_to_the_h264_high_profile_core_transposed() {
        assert_eq!(
            probe::<8>(),
            [
                [64, 89, 83, 75, 64, 50, 36, 18],
                [64, 75, 36, -18, -64, -89, -83, -50],
                [64, 50, -36, -89, -64, 18, 83, 75],
                [64, 18, -83, -50, 64, 75, -36, -89],
                [64, -18, -83, 50, 64, -75, -36, 89],
                [64, -50, -36, 89, -64, -18, 83, -75],
                [64, -75, 36, 18, -64, 89, -83, 50],
                [64, -89, 83, -75, 64, -50, 36, -18],
            ]
        );
    }

    #[test]
    fn dc_only_32x32_gives_a_uniform_block() {
        let mut c = [0i32; 1024];
        if let Some(v) = c.first_mut() {
            *v = 1000;
        }
        let mut out = [0i32; 1024];
        idct2d_dct::<32>(&c, &mut out, ClipRange::non_extended());
        // 1000 * 64 -> round_shift(.., 7) -> * 64 -> clip.
        let expected = out.first().copied().unwrap_or(0);
        assert!(out.iter().all(|&v| v == expected), "not uniform: {out:?}");
    }

    #[test]
    fn dct_is_linear() {
        // No rounding lives inside `dct1d` itself (only the caller's
        // mid-transform shift is non-linear), so superposition holds exactly.
        let a: [i32; 8] = [3, -1, 4, -1, 5, -9, 2, -6];
        let b: [i32; 8] = [1, -2, 3, -4, 5, -6, 7, -8];
        let sum: [i32; 8] =
            core::array::from_fn(|i| a.get(i).unwrap_or(&0) + b.get(i).unwrap_or(&0));
        let ya = dct1d(&a);
        let yb = dct1d(&b);
        let ysum = dct1d(&sum);
        let combined: [i32; 8] =
            core::array::from_fn(|i| ya.get(i).unwrap_or(&0) + yb.get(i).unwrap_or(&0));
        assert_eq!(ysum, combined);
    }

    #[test]
    fn extreme_inputs_never_panic() {
        let _ = dct1d(&[i32::MIN; 32]);
        let _ = dct1d(&[i32::MAX; 32]);
        let _ = dst1d(&[i32::MIN; 4]);
        let mut out = [0i32; 1024];
        idct2d_dct::<32>(&[i32::MIN; 1024], &mut out, ClipRange::non_extended());
        idct2d_dct::<32>(&[i32::MAX; 1024], &mut out, ClipRange::non_extended());
    }
}
