//! The coefficient pipeline shared after entropy decoding (D-22b): inverse
//! scan, dequantisation, and the hand-off into `vaco-codec-dsp-idct`.
//!
//! What is and is not in this module matters more than usual here. Inverse
//! scan (a fixed 64-entry permutation) and the MPEG-1/2 weighting-matrix
//! dequantisation formula are ported near-verbatim from `vaco-codec-mpeg12`,
//! which has already been checked against `ffmpeg`-decoded fixtures — the
//! confidence level this module claims for both. H.263's own dequantisation
//! formula is a **different**, flatter shape (no weighting matrix, a
//! sign-dependent step function of `quant` alone) — genuinely one of D-22b's
//! two named families, not a variant of the MPEG one — but this module does
//! not hardcode it: nobody has yet measured H.263's exact rounding constants
//! against a real decoder in this codebase, and shipping a "recalled"
//! numeric formula un-verified is exactly the mistake
//! `planning/AGENT-CONSTRAINTS.md` warns against ("if a format's tables
//! cannot be honestly sourced, return `Error::Unsupported` ... that is an
//! acceptable outcome"). [`flat_step_dequantise`] is the generic shape
//! instead: a family that *has* measured its own step/rounding rule
//! supplies it as a closure, and gets the same bounds-checked iteration this
//! module already gives the MPEG formula, without this crate asserting a
//! constant nobody here has verified.

use vaco_codec_dsp_idct::mpeg2::Idct8x8;

/// H.262 Table 7-2 / ISO 13818-2's zigzag scan (also H.263 Figure 6, and
/// MPEG-4 Part 2's `zigzag_scan`, which are the identical order): natural
/// `[v][u]` position, indexed by scan order `n`, i.e. `QF[zigzag[n]] =
/// QFS[n]`.
///
/// **Convention note**: this is the *scan-index-to-natural-position* form
/// (the classic "zigzag\[n\]" table), not H.262's own `scan[0][v][u]`
/// naming, which is indexed the other way round (*natural-position-to-
/// scan-index* — `vaco-codec-mpeg12::tables::ZIGZAG_SCAN` uses that
/// convention directly, which is why its own values are not
/// byte-identical to this table even though both encode the same H.262
/// clause). The two are exact inverse permutations of each other, and
/// [`inverse_scan`] is written for *this* table's convention — see
/// `tests::agrees_with_vaco_codec_mpeg12s_own_convention` for a computed
/// proof the two produce identical output.
#[rustfmt::skip]
pub const ZIGZAG_SCAN: [u8; 64] = [
     0,  1,  8, 16,  9,  2,  3, 10,
    17, 24, 32, 25, 18, 11,  4,  5,
    12, 19, 26, 33, 40, 48, 41, 34,
    27, 20, 13,  6,  7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36,
    29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46,
    53, 60, 61, 54, 47, 55, 62, 63,
];

/// H.262 Table 7-3: the alternate scan, used when `alternate_scan == 1`
/// (MPEG-1/2 only within this family — H.263/MPEG-4 have no equivalent
/// mode).
#[rustfmt::skip]
pub const ALTERNATE_SCAN: [u8; 64] = [
     0,  8, 16, 24,  1,  9,  2, 10,
    17, 25, 32, 40, 48, 56, 57, 49,
    41, 33, 26, 18,  3, 11,  4, 12,
    19, 27, 34, 42, 50, 58, 35, 43,
    51, 59, 20, 28,  5, 13,  6, 14,
    21, 29, 36, 44, 52, 60, 37, 45,
    53, 61, 22, 30,  7, 15, 23, 31,
    38, 46, 54, 62, 39, 47, 55, 63,
];

/// Inverse-scan `qfs` (coefficients in the order they were entropy-decoded)
/// into natural `[v][u]` order using `scan` (typically [`ZIGZAG_SCAN`] or
/// [`ALTERNATE_SCAN`], but any 64-entry permutation a family defines works):
/// `QF[scan[n]] = QFS[n]`.
#[must_use]
pub fn inverse_scan(qfs: &[i32; 64], scan: &[u8; 64]) -> [i32; 64] {
    let mut qf = [0i32; 64];
    for (n, &pos) in scan.iter().enumerate() {
        if let Some(slot) = qf.get_mut(usize::from(pos)) {
            *slot = qfs.get(n).copied().unwrap_or(0);
        }
    }
    qf
}

/// H.262 §7.4's weighting-matrix dequantisation, shared by MPEG-1 and
/// MPEG-2 (the two formats differ only in the mismatch-control step applied
/// afterward — see [`mismatch_control_mpeg1`]/[`mismatch_control_mpeg2`],
/// deliberately kept separate rather than folded in here, since H.263 calls
/// neither). `intra_dc` is `Some(value)` for the `[0][0]` position of an
/// intra block, which uses the `intra_dc_mult` rule instead of the
/// weighting-matrix rule that applies to everything else.
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "the reconstruction formula, H.262 §7.4.2.3, is defined with the '/' operator, itself defined by §4.1 as truncation toward zero — this is that exact division, not an approximation"
)]
pub fn dequantise_mpeg(
    qf: &[i32; 64],
    matrix: &[u8; 64],
    quantiser_scale: u16,
    intra: bool,
    intra_dc_mult: u16,
) -> [i32; 64] {
    let mut f = [0i32; 64];
    for (i, slot) in f.iter_mut().enumerate() {
        let coeff = qf.get(i).copied().unwrap_or(0);
        let raw = if intra && i == 0 {
            i64::from(coeff) * i64::from(intra_dc_mult)
        } else {
            let k = if intra {
                0
            } else if coeff > 0 {
                1
            } else if coeff < 0 {
                -1
            } else {
                0
            };
            let w = i64::from(matrix.get(i).copied().unwrap_or(16));
            ((i64::from(2 * coeff) + i64::from(k)) * w * i64::from(quantiser_scale)) / 32
        };
        *slot = i32::try_from(raw.clamp(-2048, 2047)).unwrap_or(0);
    }
    f
}

/// H.262 Annex D.9.1's MPEG-1 mismatch-control rule: move every non-zero,
/// non-DC AC coefficient one step toward zero if it would otherwise be even.
/// Never applied to an intra block's own DC coefficient (`i == 0`), which
/// [`dequantise_mpeg`] already reconstructs through `intra_dc_mult`, a
/// mechanism this correction's own text does not describe.
pub fn mismatch_control_mpeg1(f: &mut [i32; 64], intra: bool) {
    for (i, slot) in f.iter_mut().enumerate() {
        if intra && i == 0 {
            continue;
        }
        let v = *slot;
        if v != 0 && v % 2 == 0 {
            *slot = v - v.signum();
        }
    }
}

/// H.262 §7.4.4's MPEG-2 mismatch-control rule: if the sum of all 64
/// dequantised coefficients is even, adjust `F[7][7]` by ±1 to make it odd.
/// Applies to every block, intra and non-intra alike, including the
/// trivial all-zero block (sum 0 is even, so `F[7][7]` toggles to `+1`) —
/// the standard's own summary loop does not special-case it either.
pub fn mismatch_control_mpeg2(f: &mut [i32; 64]) {
    let sum: i64 = f.iter().map(|&v| i64::from(v)).sum();
    if sum & 1 == 0
        && let Some(last) = f.get_mut(63)
    {
        *last = if *last & 1 != 0 { *last - 1 } else { *last + 1 };
    }
}

/// The generic shape of a "flat-step" dequantisation formula (no weighting
/// matrix — every coefficient scales by the same, `quant`-derived step): for
/// each coefficient, call `step(coeff, quant)` and clamp the result to
/// `clamp_range`. This is the shape H.263's own dequantisation follows
/// (Recommendation H.263 §6.2's odd/even-`quant` step function), but the
/// exact step function is the caller's own, measured responsibility — see
/// the module docs for why this crate does not supply H.263's constants
/// itself.
#[must_use]
pub fn flat_step_dequantise(
    qf: &[i32; 64],
    quant: u16,
    clamp_range: (i32, i32),
    step: impl Fn(i32, u16) -> i32,
) -> [i32; 64] {
    let mut f = [0i32; 64];
    let (lo, hi) = clamp_range;
    for (i, slot) in f.iter_mut().enumerate() {
        let coeff = qf.get(i).copied().unwrap_or(0);
        *slot = step(coeff, quant).clamp(lo, hi);
    }
    f
}

/// This family's common IDCT output convention: the classical 8x8 inverse
/// DCT, run in `f32` (H.262 Annex A specifies an accuracy bound rather than
/// a mandated integer algorithm, and every family sharing this pipeline
/// inherits that same "any sufficiently accurate transform conforms" rule —
/// see `vaco-codec-dsp-idct::mpeg2`'s own docs), rounded to the nearest
/// integer (halving the average rounding error against truncation) and
/// saturated to a caller-supplied range — `(-256, 255)` for every member of
/// this family measured so far (8-bit source samples plus one bit of
/// residual headroom).
pub fn run_idct(idct: &mut Idct8x8<f32>, f: &[i32; 64], clamp_range: (i32, i32)) -> [i32; 64] {
    let mut coeffs = [0f32; 64];
    for (dst, &src) in coeffs.iter_mut().zip(f.iter()) {
        #[allow(
            clippy::cast_precision_loss,
            reason = "a dequantised coefficient is well within f32's exact integer range (|value| < 2^24) for every family this pipeline serves"
        )]
        {
            *dst = src as f32;
        }
    }
    let mut out_f = [0f32; 64];
    idct.apply(&coeffs, &mut out_f);
    let mut out = [0i32; 64];
    let (lo, hi) = clamp_range;
    for (dst, &src) in out.iter_mut().zip(out_f.iter()) {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "rounded then clamped to a caller-supplied i32 range before the cast"
        )]
        {
            *dst = (src.round() as i32).clamp(lo, hi);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, reason = "test code")]
    use super::*;

    #[test]
    fn zigzag_and_alternate_scans_are_permutations_of_0_to_63() {
        for scan in [ZIGZAG_SCAN, ALTERNATE_SCAN] {
            let mut seen = [false; 64];
            for &v in &scan {
                let idx = usize::from(v);
                assert!(!seen[idx], "duplicate position {idx}");
                seen[idx] = true;
            }
            assert!(
                seen.iter().all(|&s| s),
                "scan does not cover every position"
            );
        }
    }

    /// `vaco-codec-mpeg12::tables::ZIGZAG_SCAN`/`ALTERNATE_SCAN`, reproduced
    /// here verbatim (as plain data, not a dependency — layering runs the
    /// other way) purely to prove this crate's own, differently-conventioned
    /// tables agree with that already-`ffmpeg`-checked crate's output. That
    /// crate's own `inverse_scan` computes `qf[pos] = qfs[table[pos]]`
    /// (natural-position-to-scan-index); this crate's [`inverse_scan`]
    /// computes `qf[scan[n]] = qfs[n]` (scan-index-to-natural-position) — an
    /// inverse-permutation, inverse-formula pair that this test confirms
    /// nets out identically rather than merely asserting it does.
    #[rustfmt::skip]
    const MPEG12_ZIGZAG: [u8; 64] = [
         0,  1,  5,  6, 14, 15, 27, 28,
         2,  4,  7, 13, 16, 26, 29, 42,
         3,  8, 12, 17, 25, 30, 41, 43,
         9, 11, 18, 24, 31, 40, 44, 53,
        10, 19, 23, 32, 39, 45, 52, 54,
        20, 22, 33, 38, 46, 51, 55, 60,
        21, 34, 37, 47, 50, 56, 59, 61,
        35, 36, 48, 49, 57, 58, 62, 63,
    ];

    #[rustfmt::skip]
    const MPEG12_ALTERNATE: [u8; 64] = [
         0,  4,  6, 20, 22, 36, 38, 52,
         1,  5,  7, 21, 23, 37, 39, 53,
         2,  8, 19, 24, 34, 40, 50, 54,
         3,  9, 18, 25, 35, 41, 51, 55,
        10, 17, 26, 30, 42, 46, 56, 60,
        11, 16, 27, 31, 43, 47, 57, 61,
        12, 15, 28, 32, 44, 48, 58, 62,
        13, 14, 29, 33, 45, 49, 59, 63,
    ];

    /// `vaco-codec-mpeg12::block::inverse_scan`'s own formula, reproduced
    /// here (not imported — that crate is not a dependency) only as the
    /// oracle this test checks [`inverse_scan`] against.
    fn mpeg12_style_inverse_scan(qfs: &[i32; 64], table: &[u8; 64]) -> [i32; 64] {
        let mut qf = [0i32; 64];
        for (pos, slot) in qf.iter_mut().enumerate() {
            let n = table[pos];
            *slot = qfs[usize::from(n)];
        }
        qf
    }

    #[test]
    fn agrees_with_vaco_codec_mpeg12s_own_convention() {
        let qfs: [i32; 64] = core::array::from_fn(|i| i32::try_from(i).unwrap_or(0) * 3 - 90);

        assert_eq!(
            inverse_scan(&qfs, &ZIGZAG_SCAN),
            mpeg12_style_inverse_scan(&qfs, &MPEG12_ZIGZAG),
            "zigzag scan: the two conventions must produce the same natural-order block"
        );
        assert_eq!(
            inverse_scan(&qfs, &ALTERNATE_SCAN),
            mpeg12_style_inverse_scan(&qfs, &MPEG12_ALTERNATE),
            "alternate scan: the two conventions must produce the same natural-order block"
        );
    }

    #[test]
    fn inverse_scan_places_each_coefficient_at_its_scan_position() {
        let mut qfs = [0i32; 64];
        if let Some(v) = qfs.first_mut() {
            *v = 100;
        }
        if let Some(v) = qfs.get_mut(1) {
            *v = 200;
        }
        let qf = inverse_scan(&qfs, &ZIGZAG_SCAN);
        // ZIGZAG_SCAN[0] = 0, ZIGZAG_SCAN[1] = 1 — both map to themselves at
        // the start of the table, so this doubles as an identity check.
        assert_eq!(qf.first().copied(), Some(100));
        assert_eq!(qf.get(1).copied(), Some(200));
    }

    #[test]
    fn dequantise_dc_only_intra_uses_the_mult_not_the_matrix() {
        let mut qf = [0i32; 64];
        if let Some(v) = qf.first_mut() {
            *v = 10;
        }
        let matrix = [16u8; 64];
        let f = dequantise_mpeg(&qf, &matrix, 1, true, 8);
        assert_eq!(f.first().copied(), Some(80));
    }

    #[test]
    fn mpeg2_mismatch_control_toggles_f77_on_an_even_sum() {
        let mut f = [0i32; 64];
        mismatch_control_mpeg2(&mut f);
        assert_eq!(f.get(63).copied(), Some(1));
    }

    #[test]
    fn mpeg1_mismatch_control_moves_even_ac_coefficients_toward_zero() {
        let mut f = [0i32; 64];
        if let Some(v) = f.get_mut(1) {
            *v = 4;
        }
        if let Some(v) = f.get_mut(2) {
            *v = -4;
        }
        mismatch_control_mpeg1(&mut f, false);
        assert_eq!(f.get(1).copied(), Some(3));
        assert_eq!(f.get(2).copied(), Some(-3));
    }

    #[test]
    fn mpeg1_mismatch_control_skips_intra_dc() {
        let mut f = [0i32; 64];
        if let Some(v) = f.first_mut() {
            *v = 80; // even, but position 0 of an intra block.
        }
        mismatch_control_mpeg1(&mut f, true);
        assert_eq!(f.first().copied(), Some(80));
    }

    #[test]
    fn flat_step_dequantise_applies_the_callers_own_formula() {
        let mut qf = [0i32; 64];
        if let Some(v) = qf.first_mut() {
            *v = 3;
        }
        let f = flat_step_dequantise(&qf, 5, (-2048, 2047), |coeff, quant| {
            coeff * i32::from(quant)
        });
        assert_eq!(f.first().copied(), Some(15));
    }

    #[test]
    fn flat_step_dequantise_clamps_to_the_caller_range() {
        let mut qf = [0i32; 64];
        if let Some(v) = qf.first_mut() {
            *v = 1000;
        }
        let f = flat_step_dequantise(&qf, 1, (-10, 10), |coeff, _quant| coeff);
        assert_eq!(f.first().copied(), Some(10));
    }

    #[test]
    fn run_idct_on_dc_only_block_is_uniform() {
        // A DC-only block must produce a uniform output — the property test
        // this project's own lessons recommend over a second transcription
        // of the same transform (an oracle that can only agree cannot be
        // wrong differently).
        let Ok(mut idct) = vaco_codec_dsp_idct::mpeg2::idct8x8_f32() else {
            return;
        };
        let mut f = [0i32; 64];
        if let Some(v) = f.first_mut() {
            *v = 64;
        }
        let out = run_idct(&mut idct, &f, (-256, 255));
        let first = out.first().copied();
        assert!(out.iter().all(|&v| Some(v) == first));
    }
}
