//! T3-01j (#424)'s own scope, narrowed to what it actually is once the
//! transform math is subtracted out: `vaco-codec-dsp-idct::h264` already
//! has `idct4x4`, `luma_dc_hadamard4x4`, `chroma_dc_hadamard2x2` (and the
//! 8x8/4:2:2 variants this crate's Main-profile, 4:2:0-only scope never
//! calls) -- all of them already take *already-scaled* coefficients and
//! return final residual samples (`idct4x4`/`idct8x8` even fold in the
//! eq. (8-282) `(h+32)>>6` rounding themselves). What is missing, and
//! what this module is, is everything upstream of that: turning a raw
//! decoded coefficient level into the scaled value those functions
//! expect.
//!
//! # What this module is not
//!
//! Not wired into [`crate::mb`]'s macroblock loop, and not a reconstructed
//! pixel in sight. Wiring this in needs three things this dispatch was
//! told explicitly not to build yet: the inverse zig-zag scanning process
//! (clause 8.5.4) that turns a coefficient *list* into the 2-D array `c`
//! these functions take as input, the `dcY`-to-`luma4x4BlkIdx` assignment
//! (clause 8.5.2's Figure 8-6) for `Intra_16x16` macroblocks, and intra
//! prediction itself (`predL`/`predC`, clause 8.5.1's own `Clip1(pred +
//! r)` reconstruction step) -- all `#420`'s scope, not this one's. This
//! module is the seam between the entropy layer (already landed, #418/
//! #419) and reconstruction (#420 onward): pure functions from a decoded
//! coefficient array and a QP to a scaled array ready for the existing
//! transform primitives, each one a direct transcription of one clause's
//! equations, each independently testable without any of the three
//! missing pieces above.
//!
//! No scaling-list support: this crate's test corpora are encoded Main
//! profile (matching the same reason CAVLC/CABAC's own 8x8-transform
//! scope line is drawn where it is -- see `mb.rs`'s own module doc), and
//! `provenance/vaco-codec-h264.toml`'s `iso-iec-14496-10-2002-draft`
//! source predates `seq_scaling_matrix_present_flag`/custom scaling lists
//! entirely -- confirmed by reading clause 8.5.5's `LevelScale(m, i, j)`
//! definition below, which is a single fixed matrix with no per-SPS/PPS
//! parameter feeding it at all, not an omission on this crate's part.

#![allow(
    dead_code,
    reason = "not wired into mb.rs's macroblock loop yet -- #420's own scope needs the               inverse-scan, dcY-to-block assignment, and intra prediction this dispatch               was told not to build; exercised by this module's own tests in the meantime"
)]

use vaco_codec_dsp_idct::h264::{chroma_dc_hadamard2x2, luma_dc_hadamard4x4};

/// Clause 8.5.5, eq. (8-253) -- the fixed matrix `v` `LevelScale(m, i, j)`
/// indexes into. Row `m` (0..=5, `qP % 6`), column `k` (0..=2, the
/// position category eq. (8-252) selects). Transcribed once, directly,
/// from the primary text; `dequant::tests::level_scale_matches_table`
/// checks it row by row again independently and
/// `tests::no_two_rows_are_byte_identical` guards the failure mode
/// `cabac_mb_tables.rs`'s own `table_distinctness` module was written for
/// -- a future added table module accidentally duplicating one of these
/// six rows would fail loudly instead of silently.
#[rustfmt::skip]
pub(crate) const LEVEL_SCALE_V: [[i32; 3]; 6] = [
    [10, 16, 13],
    [11, 18, 14],
    [13, 20, 16],
    [14, 23, 18],
    [16, 25, 20],
    [18, 29, 23],
];

/// Clause 8.5.5, eq. (8-252): `LevelScale(m, i, j)`.
///
/// ```text
/// v[m][0]  for (i, j) in {(0,0), (0,2), (2,0), (2,2)}
/// v[m][1]  for (i, j) in {(1,1), (1,3), (3,1), (3,3)}
/// v[m][2]  otherwise
/// ```
#[must_use]
#[allow(
    clippy::indexing_slicing,
    reason = "col is one of the three literal constants 0/1/2 below, into a fixed 3-element row -- not a bitstream-derived index"
)]
pub(crate) fn level_scale(m: u32, i: u32, j: u32) -> i32 {
    let row = LEVEL_SCALE_V
        .get((m % 6) as usize)
        .copied()
        .unwrap_or([16, 16, 16]);
    let col = if (i % 2 == 0) && (j % 2 == 0) {
        0
    } else if (i % 2 == 1) && (j % 2 == 1) {
        1
    } else {
        2
    };
    row[col]
}

/// Clause 8.5.5, Table 8-13 -- `QPC` as a function of `qPI`. `qPI < 30`
/// maps to itself; `qPI` 30..=51 maps through this table, transcribed
/// value by value from the primary text (not a formula -- the table is
/// deliberately irregular, e.g. two consecutive `qPI` values sometimes
/// share a `QPC`).
#[rustfmt::skip]
const QPC_FOR_QPI_30_TO_51: [u8; 22] = [
    29, 30, 31, 32, 32, 33, 34, 34, 35, 35, 36,
    36, 37, 37, 37, 38, 38, 38, 39, 39, 39, 39,
];

/// Clause 8.5.5, eq. (8-251) + Table 8-13: `QPC` from `QPY` and the PPS's
/// `chroma_qp_index_offset`. `SI`/`SP` slices' separate `QSC` derivation
/// (same table, substituting `QSY`) is out of scope -- this crate refuses
/// `SI` slices outright (`check_scope`) and does not track `QSY`.
#[must_use]
pub(crate) fn chroma_qp(qpy: i32, chroma_qp_index_offset: i32) -> i32 {
    let qpi = (qpy + chroma_qp_index_offset).clamp(0, 51);
    if qpi < 30 {
        qpi
    } else {
        i32::from(
            QPC_FOR_QPI_30_TO_51
                .get((qpi - 30) as usize)
                .copied()
                .unwrap_or(39),
        )
    }
}

/// Clause 7.4.5, eq. (7-23): the running per-macroblock luma QP.
/// `qpy_prev` is `SliceQPY` for the first macroblock of a slice (clause
/// 7.4.3's eq. (7-16), already `PpsInfo::slice_qp` in `vaco-parse-h264`),
/// and the previous macroblock's own `QPY` for every macroblock after
/// that -- including a skipped one, since `mb_qp_delta` is inferred `0`
/// for those rather than leaving `QPY` undefined.
#[must_use]
pub(crate) fn next_qpy(qpy_prev: i32, mb_qp_delta: i32) -> i32 {
    (qpy_prev + mb_qp_delta + 52).rem_euclid(52)
}

/// Clause 8.5.8, eq. (8-260)..(8-265): scaling for one already
/// inverse-scanned 4x4 coefficient array. `qp` is whichever of `QPY`/
/// `QPC` clause 8.5.8's own `qP` derivation (eq. (8-260)..(8-263))
/// selects -- `SP`/`SI`'s `sMbFlag == 1` case (`QSY`/`QSC`) is out of
/// scope, so this always takes the plain `QPY`/`QPC` path a caller has
/// already resolved.
///
/// `dc_already_scaled` is eq. (8-264)'s own condition, factored out to a
/// bool since the caller (not this function) knows whether `c` is a luma
/// block coded `Intra_16x16` or a chroma block -- true in both those
/// cases (position `(0, 0)` already holds a value clause 8.5.6/8.5.7's
/// separate DC scaling produced, and must pass through unscaled), false
/// for every other 4x4 luma block (`Intra_4x4`/`Intra_8x8`/inter), where
/// `(0, 0)` is a normal coefficient like any other.
///
/// Returns the scaled array `d`, ready for
/// [`vaco_codec_dsp_idct::h264::idct4x4`] (which folds in eq. (8-266)
/// through (8-282) itself, rounding included).
#[must_use]
#[allow(
    clippy::indexing_slicing,
    reason = "idx = i*4+j with i,j in 0..4 is provably 0..16, into fixed 16-element arrays -- not a bitstream-derived index"
)]
pub(crate) fn dequant_4x4(c: &[i32; 16], qp: i32, dc_already_scaled: bool) -> [i32; 16] {
    let m = qp.rem_euclid(6) as u32;
    let shift = qp.div_euclid(6);
    let mut d = [0i32; 16];
    for i in 0..4u32 {
        for j in 0..4u32 {
            let idx = (i * 4 + j) as usize;
            d[idx] = if dc_already_scaled && i == 0 && j == 0 {
                c[idx]
            } else {
                c[idx] * level_scale(m, i, j) << shift
            };
        }
    }
    d
}

/// Clause 8.5.6, eq. (8-254)..(8-256): the luma `Intra_16x16` DC
/// transform. Unlike [`dequant_4x4`], the Hadamard transform runs
/// *before* scaling, not after -- `c` here is the raw 4x4 array of
/// `Intra16x16DCLevel` coefficients (already inverse-scanned, clause
/// 8.5.4), not something [`vaco_codec_dsp_idct::h264::idct4x4`] would
/// ever see; the scaled result (`dcY`) is what clause 8.5.2 inserts into
/// each of the 16 luma 4x4 AC blocks' own position `(0, 0)` before
/// [`dequant_4x4`] (with `dc_already_scaled = true`) runs on each of
/// those.
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "qpy/6 is eq. (8-255)/(8-256)'s own QPY/6 term verbatim, not a precision-loss bug"
)]
pub(crate) fn dequant_luma_dc_4x4(c: &[i32; 16], qpy: i32) -> [i32; 16] {
    let f = luma_dc_hadamard4x4(c);
    let m = qpy.rem_euclid(6) as u32;
    let scale = level_scale(m, 0, 0);
    let mut dc = [0i32; 16];
    if qpy >= 12 {
        let shift = qpy / 6 - 2;
        for (d, &v) in dc.iter_mut().zip(f.iter()) {
            *d = (v * scale) << shift;
        }
    } else {
        let shift = 2 - qpy / 6;
        let round = 1i32 << (1 - qpy / 6);
        for (d, &v) in dc.iter_mut().zip(f.iter()) {
            *d = (v * scale + round) >> shift;
        }
    }
    dc
}

/// Clause 8.5.7, eq. (8-257)..(8-259): the chroma DC transform for
/// `ChromaArrayType == 1` (4:2:0, this crate's only supported chroma
/// format). Same shape as [`dequant_luma_dc_4x4`] one size down: Hadamard
/// first (clause 8.5.7's own 2x2 `f = A c A` via
/// [`chroma_dc_hadamard2x2`]), then scale. The result (`dcC`) is what
/// clause 8.5.3 inserts into each of the 4 chroma 4x4 AC blocks' own
/// position `(0, 0)`.
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "qpc/6 is eq. (8-258)'s own QPC/6 term verbatim, not a precision-loss bug"
)]
pub(crate) fn dequant_chroma_dc_2x2(c: &[i32; 4], qpc: i32) -> [i32; 4] {
    let f = chroma_dc_hadamard2x2(c);
    let m = qpc.rem_euclid(6) as u32;
    let scale = level_scale(m, 0, 0);
    let mut dc = [0i32; 4];
    if qpc >= 6 {
        let shift = qpc / 6 - 1;
        for (d, &v) in dc.iter_mut().zip(f.iter()) {
            *d = (v * scale) << shift;
        }
    } else {
        for (d, &v) in dc.iter_mut().zip(f.iter()) {
            *d = (v * scale) >> 1;
        }
    }
    dc
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Independent re-check of [`LEVEL_SCALE_V`] against the primary
    /// text, row by row, the same discipline `cabac_mb_tables.rs` and
    /// `cabac_residual.rs`'s own table modules use.
    #[test]
    fn level_scale_matches_table() {
        assert_eq!(LEVEL_SCALE_V[0], [10, 16, 13]);
        assert_eq!(LEVEL_SCALE_V[1], [11, 18, 14]);
        assert_eq!(LEVEL_SCALE_V[2], [13, 20, 16]);
        assert_eq!(LEVEL_SCALE_V[3], [14, 23, 18]);
        assert_eq!(LEVEL_SCALE_V[4], [16, 25, 20]);
        assert_eq!(LEVEL_SCALE_V[5], [18, 29, 23]);
    }

    /// The failure mode `cabac_mb_tables.rs::table_distinctness` guards
    /// against, applied here: no two rows of this module's own table
    /// should ever be byte-identical (nothing in clause 8.5.5 gives a
    /// reason for two different `qP % 6` values to share a row).
    #[test]
    fn no_two_rows_are_byte_identical() {
        for (i, row_a) in LEVEL_SCALE_V.iter().enumerate() {
            for row_b in LEVEL_SCALE_V.iter().skip(i + 1) {
                assert_ne!(row_a, row_b, "two rows of LEVEL_SCALE_V are byte-identical");
            }
        }
    }

    #[test]
    fn level_scale_category_selection() {
        // (0,0): category 0 for every m.
        assert_eq!(level_scale(0, 0, 0), 10);
        // (2,2): also category 0.
        assert_eq!(level_scale(0, 2, 2), 10);
        // (1,1): category 1.
        assert_eq!(level_scale(0, 1, 1), 16);
        // (3,3): also category 1.
        assert_eq!(level_scale(0, 3, 3), 16);
        // (0,1): category 2 (mixed parity).
        assert_eq!(level_scale(0, 0, 1), 13);
        // m wraps mod 6.
        assert_eq!(level_scale(6, 0, 0), level_scale(0, 0, 0));
    }

    #[test]
    fn chroma_qp_below_30_is_identity() {
        assert_eq!(chroma_qp(10, 0), 10);
        assert_eq!(chroma_qp(29, 0), 29);
        // qPI clipped to [0, 51] before the table lookup.
        assert_eq!(chroma_qp(0, -5), 0);
    }

    #[test]
    fn chroma_qp_table_boundary_values() {
        // Table 8-13, transcribed spot checks (qPI: QPC).
        assert_eq!(chroma_qp(30, 0), 29);
        assert_eq!(chroma_qp(33, 0), 32);
        assert_eq!(chroma_qp(34, 0), 32); // 33 and 34 share QPC = 32.
        assert_eq!(chroma_qp(45, 0), 38);
        assert_eq!(chroma_qp(51, 0), 39);
        // chroma_qp_index_offset shifts qPI before the same table.
        assert_eq!(chroma_qp(45, 6), chroma_qp(51, 0));
    }

    #[test]
    fn next_qpy_wraps_per_eq_7_23() {
        assert_eq!(next_qpy(26, 0), 26);
        assert_eq!(next_qpy(51, 1), 0); // (51 + 1 + 52) % 52 == 0
        assert_eq!(next_qpy(0, -1), 51); // (0 - 1 + 52) % 52 == 51
        assert_eq!(next_qpy(26, -26), 0);
        assert_eq!(next_qpy(26, 25), 51);
    }

    /// Clause 8.5.8's own worked shape: at `qP == 0` (`m = 0`, `shift =
    /// 0`), an unscaled coefficient of exactly `1` at a category-2
    /// position scales to `LevelScale(0, 0, 1) == 13`, and the
    /// `dc_already_scaled` exception leaves position `(0, 0)` completely
    /// untouched regardless of what `LevelScale` would otherwise give it.
    #[test]
    fn dequant_4x4_dc_exception_and_regular_scaling() {
        let mut c = [0i32; 16];
        c[0] = 7; // (0, 0)
        c[1] = 1; // (0, 1), category 2
        let scaled = dequant_4x4(&c, 0, true);
        assert_eq!(
            scaled[0], 7,
            "dc_already_scaled must pass (0,0) through unscaled"
        );
        assert_eq!(scaled[1], 13);

        let scaled_no_dc = dequant_4x4(&c, 0, false);
        assert_eq!(scaled_no_dc[0], 7 * level_scale(0, 0, 0));
    }

    #[test]
    fn dequant_4x4_shift_scales_with_qp_over_6() {
        let mut c = [0i32; 16];
        c[5] = 2; // (1, 1), category 1
        let low = dequant_4x4(&c, 0, false);
        let high = dequant_4x4(&c, 6, false); // same m (qp%6==0), shift 1 higher
        assert_eq!(high[5], low[5] * 2);
    }

    #[test]
    fn dequant_luma_dc_matches_hand_computed_flat_case() {
        // All-zero DC coefficients: Hadamard of zero is zero, and both
        // branches of eq. (8-255)/(8-256) map zero to zero regardless of
        // qp, rounding term included.
        let c = [0i32; 16];
        assert_eq!(dequant_luma_dc_4x4(&c, 26), [0; 16]);
        assert_eq!(dequant_luma_dc_4x4(&c, 4), [0; 16]);
    }

    #[test]
    fn dequant_chroma_dc_matches_hand_computed_flat_case() {
        let c = [0i32; 4];
        assert_eq!(dequant_chroma_dc_2x2(&c, 26), [0; 4]);
        assert_eq!(dequant_chroma_dc_2x2(&c, 2), [0; 4]);
    }
}
