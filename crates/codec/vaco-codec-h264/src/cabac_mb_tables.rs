#![allow(clippy::unreadable_literal, reason = "spec tables, not derived numbers")]
//! CABAC macroblock-layer context-initialisation tables, ITU-T H.264
//! clause 9.3.1.1, Tables 9-12 through 9-18 — the `(m, n)` pairs
//! [`crate::cabac_mb`] builds its per-syntax-element [`ContextInit`] arrays
//! from. Every value here is transcribed row-by-row from
//! `provenance/vaco-codec-h264.toml`'s `iso-iec-14496-10-2002-draft`
//! (the same source `cavlc_tables.rs` and `cabac_residual.rs` were checked
//! against), not from recollection — this file did not exist before the
//! CABAC macroblock-layer work landed, so there is no earlier,
//! less-verified pass to compare against the way those two files describe.
//!
//! # Layout convention
//!
//! Every table below is a plain array of `(m, n)` rows in ascending `ctxIdx`
//! order for the syntax element it names, one array per applicable
//! `cabac_init_idc` value (or one array total, for the handful of elements
//! Table 9-11 says do not vary by slice type or `cabac_init_idc` at all —
//! `mb_qp_delta`, `intra_chroma_pred_mode`, `prev_intra4x4_pred_mode_flag`,
//! `rem_intra4x4_pred_mode`). Where an I-slice-only table and up to three
//! `cabac_init_idc`-selected tables both exist for one element, they are
//! named `_I`, `_IDC0`, `_IDC1`, `_IDC2`.
//!
//! Only what [`crate::cabac_mb`] actually calls for is transcribed — not
//! every ctxIdx in the specification's tables (`mb_field_decoding_flag`'s
//! ctxIdx 70-72, for one, since MBAFF is out of scope; see `mb.rs`'s module
//! doc).
use vaco_codec_cabac::ContextInit;

/// One `(m, n)` row.
pub(crate) type Init = (i16, i16);

/// `mb_type`, I slices (also used, offset by 5/23, for the `Intra` suffix of
/// `mb_type` in P/SP and B slices — see [`super::cabac_mb`]).
/// ctxIdx 3..=10.
#[rustfmt::skip]
pub(crate) const MB_TYPE_I: [Init; 8] = [
    (20, -15), (2, 54), (3, 74), (-28, 127), (-23, 104), (-6, 53), (-1, 54), (7, 51),
];

/// `mb_skip_flag`, P/SP slices. ctxIdx 11..=13; each row is
/// `[cabac_init_idc=0, =1, =2]` for one ctxIdx (`cabac_mb`'s
/// [`inits_by_idc`] selects the column).
#[rustfmt::skip]
pub(crate) const SKIP_P: [[Init; 3]; 3] = [
    [(23, 33), (22, 25), (29, 16)],
    [(23, 2), (34, 0), (25, 0)],
    [(21, 0), (16, 0), (14, 0)],
];

/// `mb_type`, P/SP slices — prefix (ctxIdx 14..=17) and suffix (ctxIdx
/// 17..=20) combined into the *one* ctxIdx range they actually span, per
/// clause 9.3.3.1's note that the last prefix bin and first suffix bin "may
/// share the same ctxIdx": index 3 here (ctxIdx 17) is that shared context,
/// read as the prefix's bin2-when-`b1==1` case and again as the suffix's
/// bin0 — one array, not two independently-adapting copies of the same
/// state.
#[rustfmt::skip]
pub(crate) const MB_TYPE_P: [[Init; 3]; 7] = [
    [(1, 9), (-2, 9), (-10, 51)],
    [(0, 49), (4, 41), (-3, 62)],
    [(-37, 118), (-29, 118), (-27, 99)],
    [(5, 57), (2, 65), (26, 16)],
    [(-13, 78), (-6, 71), (-4, 85)],
    [(-11, 65), (-13, 79), (-24, 102)],
    [(1, 62), (5, 52), (5, 57)],
];

/// `sub_mb_type`, P/SP slices. ctxIdx 21..=23.
#[rustfmt::skip]
pub(crate) const SUB_MB_TYPE_P: [[Init; 3]; 3] = [
    [(12, 49), (9, 50), (6, 57)],
    [(-4, 73), (-3, 70), (-17, 73)],
    [(17, 50), (10, 54), (14, 57)],
];

/// `mb_skip_flag`, B slices. ctxIdx 24..=26.
#[rustfmt::skip]
pub(crate) const SKIP_B: [[Init; 3]; 3] = [
    [(18, 64), (26, 34), (20, 40)],
    [(9, 43), (19, 22), (20, 10)],
    [(29, 0), (40, 0), (29, 0)],
];

/// `mb_type`, B slices — prefix (ctxIdx 27..=32) and suffix (ctxIdx
/// 32..=35) combined the same way [`MB_TYPE_P`] combines its own: index 5
/// here (ctxIdx 32) is the shared context between the prefix's last bin and
/// the suffix's first.
#[rustfmt::skip]
pub(crate) const MB_TYPE_B: [[Init; 3]; 9] = [
    [(26, 67), (57, 2), (54, 0)],
    [(16, 90), (41, 36), (37, 42)],
    [(9, 104), (26, 69), (12, 97)],
    [(-46, 127), (-45, 127), (-32, 127)],
    [(-20, 104), (-15, 101), (-22, 117)],
    [(1, 67), (-4, 76), (-2, 74)],
    [(-13, 78), (-6, 71), (-4, 85)],
    [(-11, 65), (-13, 79), (-24, 102)],
    [(1, 62), (5, 52), (5, 57)],
];

/// `sub_mb_type`, B slices. ctxIdx 36..=39.
#[rustfmt::skip]
pub(crate) const SUB_MB_TYPE_B: [[Init; 3]; 4] = [
    [(-6, 86), (6, 69), (-6, 93)],
    [(-17, 95), (-13, 90), (-14, 88)],
    [(-6, 61), (0, 52), (-6, 44)],
    [(9, 45), (8, 43), (4, 55)],
];

/// `mvd_lX[][][0]` (horizontal component). ctxIdx 40..=46.
#[rustfmt::skip]
pub(crate) const MVD_COMP0: [[Init; 3]; 7] = [
    [(-3, 69), (-2, 69), (-11, 89)],
    [(-6, 81), (-5, 82), (-15, 103)],
    [(-11, 96), (-10, 96), (-21, 116)],
    [(6, 55), (2, 59), (19, 57)],
    [(7, 67), (2, 75), (20, 58)],
    [(-5, 86), (-3, 87), (4, 84)],
    [(2, 88), (-3, 100), (6, 96)],
];

/// `mvd_lX[][][1]` (vertical component). ctxIdx 47..=53.
#[rustfmt::skip]
pub(crate) const MVD_COMP1: [[Init; 3]; 7] = [
    [(0, 58), (1, 56), (1, 63)],
    [(-3, 76), (-3, 74), (-5, 85)],
    [(-10, 94), (-6, 85), (-13, 106)],
    [(5, 54), (0, 59), (5, 63)],
    [(4, 69), (-3, 81), (6, 75)],
    [(-3, 81), (-7, 86), (-3, 90)],
    [(0, 88), (-5, 95), (-1, 101)],
];

/// `ref_idx_l0`/`ref_idx_l1`. ctxIdx 54..=59.
#[rustfmt::skip]
pub(crate) const REF_IDX: [[Init; 3]; 6] = [
    [(-7, 67), (-1, 66), (3, 55)],
    [(-5, 74), (-1, 77), (-4, 79)],
    [(-4, 74), (1, 70), (-2, 75)],
    [(-5, 80), (-2, 86), (-12, 97)],
    [(-7, 72), (-5, 72), (-7, 50)],
    [(1, 58), (0, 61), (1, 60)],
];

/// `mb_qp_delta`. ctxIdx 60..=63 — one table for every slice type (Table
/// 9-11 lists no `cabac_init_idc` column for it).
#[rustfmt::skip]
pub(crate) const QP_DELTA: [Init; 4] = [(0, 41), (0, 63), (0, 63), (0, 63)];

/// `intra_chroma_pred_mode`. ctxIdx 64..=67 — one table for every slice
/// type.
#[rustfmt::skip]
pub(crate) const INTRA_CHROMA_PRED_MODE: [Init; 4] = [(-9, 83), (4, 86), (0, 97), (-7, 72)];

/// `prev_intra4x4_pred_mode_flag`. ctxIdx 68 — one context, every slice
/// type.
pub(crate) const PREV_INTRA4X4: Init = (13, 41);

/// `rem_intra4x4_pred_mode`. ctxIdx 69 — one context reused for all 3 bins
/// of its `FL(cMax=7)` binarisation (Table 9-29's row for ctxIdxOffset 69:
/// `0/0/0`), every slice type.
pub(crate) const REM_INTRA4X4: Init = (3, 62);

/// `transform_size_8x8_flag` (High profile). ctxIdx 399..=401, `ctxIdxInc`
/// = `condTermFlagA + condTermFlagB` (clause 9.3.3.1.1.10: each neighbour
/// contributes its own decoded `transform_size_8x8_flag`, 0 if that
/// neighbour is unavailable), column order `[I_or_SI, idc0, idc1, idc2]`.
///
/// This crate's on-hand `iso-iec-14496-10-2002-draft` source predates the
/// 8x8 transform entirely (`mb.rs`'s own module doc names the same gap),
/// so unlike every other table in this file, these three rows are not
/// transcribed from that primary text -- they are read from JM 19.1's
/// `lib/lcommon/ctx_tables.h::INIT_TRANSFORM_SIZE_I`/`_P` (BSD/Tier A per
/// `provenance/sources.toml`), the same source and confidence level
/// `cabac_residual.rs`'s own `ctxBlockCat` 5 tables use.
#[rustfmt::skip]
pub(crate) const TRANSFORM_SIZE_8X8: [[Init; 4]; 3] = [
    [(31, 21), (12, 40), (25, 32), (21, 33)],
    [(31, 31), (11, 51), (21, 49), (19, 50)],
    [(25, 50), (14, 59), (21, 54), (17, 61)],
];

/// `coded_block_pattern`'s luma prefix (`FL` of `CodedBlockPatternLuma`,
/// 4 bits, `binIdx` == the 8x8 luma block index directly). ctxIdx 73..=76,
/// column order `[I_or_SI, idc0, idc1, idc2]`.
#[rustfmt::skip]
pub(crate) const CBP_LUMA: [[Init; 4]; 4] = [
    [(-17, 127), (-27, 126), (-39, 127), (-36, 127)],
    [(-13, 102), (-28, 98), (-18, 91), (-17, 91)],
    [(0, 82), (-25, 101), (-17, 96), (-14, 95)],
    [(-7, 74), (-23, 67), (-26, 81), (-25, 84)],
];

/// `coded_block_pattern`'s chroma suffix (`TU` of `CodedBlockPatternChroma`,
/// `cMax=2`). ctxIdx 77..=84, column order `[I_or_SI, idc0, idc1, idc2]`.
#[rustfmt::skip]
pub(crate) const CBP_CHROMA: [[Init; 4]; 8] = [
    [(-21, 107), (-28, 82), (-35, 98), (-25, 86)],
    [(-27, 127), (-20, 94), (-24, 102), (-12, 89)],
    [(-31, 127), (-16, 83), (-23, 97), (-17, 91)],
    [(-24, 127), (-22, 110), (-27, 119), (-31, 127)],
    [(-18, 95), (-21, 91), (-24, 99), (-14, 76)],
    [(-27, 127), (-18, 102), (-21, 110), (-18, 103)],
    [(-21, 114), (-13, 93), (-18, 102), (-13, 90)],
    [(-30, 127), (-29, 127), (-36, 127), (-37, 127)],
];

/// `coded_block_flag`, one 4-context block per `ctxBlockCat` (0, 1, 2, 4 —
/// `ctxBlockCat` 3, chroma DC, is at ctxIdx 97..=100 ([`CBF_CHROMA_DC`]).
/// ctxIdx 85..=88 (cat0), 89..=92 (cat1), 93..=96 (cat2), 101..=104 (cat4),
/// column order `[I_or_SI, idc0, idc1, idc2]`.
#[rustfmt::skip]
pub(crate) const CBF_LUMA_DC: [[Init; 4]; 4] = [
    [(-17, 123), (-7, 92), (0, 80), (11, 80)],
    [(-12, 115), (-5, 89), (-5, 89), (5, 76)],
    [(-16, 122), (-7, 96), (-7, 94), (2, 84)],
    [(-11, 115), (-13, 108), (-4, 92), (5, 78)],
];
/// `coded_block_flag`, `ctxBlockCat` 3 (chroma DC). ctxIdx 97..=100 — this
/// one *is* needed even though `cabac_residual`'s chroma-DC residual
/// decode was the harder puzzle to work out: `coded_block_flag` is still a
/// real syntax element read once per chroma DC block whenever
/// `coded_block_pattern` says chroma has *any* residual (`cbp_chroma != 0`),
/// exactly like every luma 4x4 block within an enabled 8x8 quadrant still
/// gets its own flag. An earlier draft of this module treated CBP's own
/// presence as inferring `coded_block_flag == 1` outright and skipped
/// reading it — wrong, and caught by the same real-corpus bit-exactness
/// measurement this table now makes possible: a P-slice `Intra16x16`
/// macroblock with `cbp_chroma == 1` decoded fine on its own but silently
/// desynced the very next macroblock, since the encoder had written a real
/// bit this crate never read.
#[rustfmt::skip]
pub(crate) const CBF_CHROMA_DC: [[Init; 4]; 4] = [
    [(-1, 74), (5, 54), (3, 55), (0, 65)],
    [(-6, 97), (6, 60), (7, 56), (-2, 79)],
    [(-7, 91), (6, 59), (7, 55), (0, 72)],
    [(-20, 127), (6, 69), (8, 61), (-4, 92)],
];
#[rustfmt::skip]
pub(crate) const CBF_LUMA_AC: [[Init; 4]; 4] = [
    [(-12, 63), (-3, 46), (0, 39), (-6, 55)],
    [(-2, 68), (-1, 65), (0, 65), (4, 61)],
    [(-15, 84), (-1, 57), (-15, 84), (-14, 83)],
    [(-13, 104), (-9, 93), (-35, 127), (-37, 127)],
];
#[rustfmt::skip]
pub(crate) const CBF_LUMA4X4: [[Init; 4]; 4] = [
    [(-3, 70), (-3, 74), (-2, 73), (-5, 79)],
    [(-8, 93), (-9, 92), (-12, 104), (-11, 104)],
    [(-10, 90), (-8, 87), (-9, 91), (-11, 91)],
    [(-30, 127), (-23, 126), (-31, 127), (-30, 127)],
];
// `CBF_CHROMA_AC` (ctxIdx 101..=104) was previously an exact duplicate
// of `CBF_CHROMA_DC`'s own values (ctxIdx 97..=100) -- a copy-paste
// transcription error, not caught by the residual-layer table audit
// two rounds ago (which checked `significant_coeff_flag`/
// `last_significant_coeff_flag`/`coeff_abs_level_minus1`'s tables row by
// row but not `coded_block_flag`'s own five `cabac_mb_tables.rs` arrays).
// Found by transcribing these specific four rows fresh from Table 9-18
// while independently bin-tracing a real macroblock's residual decode,
// not by re-auditing on suspicion alone -- the duplicate was noticed
// because it was suspiciously identical, then confirmed wrong against
// primary text. `CBF_LUMA_DC`/`CBF_LUMA_AC`/`CBF_LUMA4X4`/
// `CBF_CHROMA_DC` (ctxIdx 85..=100) were re-checked against the same
// table at the same time and all match.
#[rustfmt::skip]
pub(crate) const CBF_CHROMA_AC: [[Init; 4]; 4] = [
    [(-4, 56), (-1, 48), (-3, 53), (-6, 56)],
    [(-5, 82), (0, 68), (0, 68), (3, 68)],
    [(-7, 76), (-4, 69), (-7, 74), (-8, 71)],
    [(-22, 125), (-8, 88), (-9, 88), (-13, 98)],
];

/// Build a [`ContextInit`] array from a fixed row set (no `cabac_init_idc`
/// variation — `QP_DELTA`, `INTRA_CHROMA_PRED_MODE`).
pub(crate) fn inits_fixed<const N: usize>(rows: &[Init; N]) -> [ContextInit; N] {
    let mut out = [ContextInit::new(0, 0); N];
    for (dst, &(m, n)) in out.iter_mut().zip(rows.iter()) {
        *dst = ContextInit::new(m, n);
    }
    out
}

/// Build a [`ContextInit`] array selecting one `cabac_init_idc` column
/// (0, 1 or 2) from a `[[Init; N]; 3]` table.
pub(crate) fn inits_by_idc<const N: usize>(rows: &[[Init; 3]; N], idc: u8) -> [ContextInit; N] {
    let col = usize::from(idc.min(2));
    let mut out = [ContextInit::new(0, 0); N];
    for (dst, row) in out.iter_mut().zip(rows.iter()) {
        let (m, n) = row.get(col).copied().unwrap_or((0, 0));
        *dst = ContextInit::new(m, n);
    }
    out
}

/// Build a [`ContextInit`] array selecting one column from a
/// `[I_or_SI, idc0, idc1, idc2]`-shaped table — `col == 0` for I/SI slices,
/// `1 + idc` for P/SP/B.
pub(crate) fn inits_by_col<const N: usize>(rows: &[[Init; 4]; N], col: usize) -> [ContextInit; N] {
    let col = col.min(3);
    let mut out = [ContextInit::new(0, 0); N];
    for (dst, row) in out.iter_mut().zip(rows.iter()) {
        let (m, n) = row.get(col).copied().unwrap_or((0, 0));
        *dst = ContextInit::new(m, n);
    }
    out
}

#[cfg(test)]
mod table_distinctness {
    //! **Structural invariant, not a value check**: no two of this file's
    //! context-initialisation tables may be byte-identical to each other.
    //! Every table here is transcribed from a distinct row range of Table
    //! 9-11 (`ctxIdxOffset` is unique per syntax element/category — checked
    //! against the primary text while writing this test, and no two rows of
    //! that table share an offset), so no legitimate reason exists for any
    //! two of these arrays to hold the same values. `CBF_CHROMA_AC` being an
    //! exact duplicate of `CBF_CHROMA_DC` is exactly the failure mode this
    //! guards: per-table verification ("does this table match what I believe
    //! its own row is") can pass against the *wrong* row entirely, since it
    //! never compares a table to its neighbours. This does.
    //!
    //! If a future table legitimately *is* identical to another (nothing in
    //! Table 9-11's current scope calls for that, but nothing rules it out
    //! forever either), add a named pair to `ALLOWED_DUPLICATES` with a
    //! comment saying why — that entry becomes a checkable claim instead of
    //! silence.
    use super::*;

    /// Every table in this file, flattened to a plain `Vec<Init>` so
    /// differently-shaped tables (which can never collide) and
    /// identically-shaped ones (where a copy-paste bug is actually possible)
    /// are compared the same way.
    fn named_tables() -> Vec<(&'static str, Vec<Init>)> {
        vec![
            ("MB_TYPE_I", MB_TYPE_I.to_vec()),
            ("SKIP_P", SKIP_P.iter().flatten().copied().collect()),
            ("MB_TYPE_P", MB_TYPE_P.iter().flatten().copied().collect()),
            (
                "SUB_MB_TYPE_P",
                SUB_MB_TYPE_P.iter().flatten().copied().collect(),
            ),
            ("SKIP_B", SKIP_B.iter().flatten().copied().collect()),
            ("MB_TYPE_B", MB_TYPE_B.iter().flatten().copied().collect()),
            (
                "SUB_MB_TYPE_B",
                SUB_MB_TYPE_B.iter().flatten().copied().collect(),
            ),
            ("MVD_COMP0", MVD_COMP0.iter().flatten().copied().collect()),
            ("MVD_COMP1", MVD_COMP1.iter().flatten().copied().collect()),
            ("REF_IDX", REF_IDX.iter().flatten().copied().collect()),
            ("QP_DELTA", QP_DELTA.to_vec()),
            ("INTRA_CHROMA_PRED_MODE", INTRA_CHROMA_PRED_MODE.to_vec()),
            ("PREV_INTRA4X4", vec![PREV_INTRA4X4]),
            ("REM_INTRA4X4", vec![REM_INTRA4X4]),
            ("CBP_LUMA", CBP_LUMA.iter().flatten().copied().collect()),
            ("CBP_CHROMA", CBP_CHROMA.iter().flatten().copied().collect()),
            (
                "CBF_LUMA_DC",
                CBF_LUMA_DC.iter().flatten().copied().collect(),
            ),
            (
                "CBF_CHROMA_DC",
                CBF_CHROMA_DC.iter().flatten().copied().collect(),
            ),
            (
                "CBF_LUMA_AC",
                CBF_LUMA_AC.iter().flatten().copied().collect(),
            ),
            (
                "CBF_LUMA4X4",
                CBF_LUMA4X4.iter().flatten().copied().collect(),
            ),
            (
                "CBF_CHROMA_AC",
                CBF_CHROMA_AC.iter().flatten().copied().collect(),
            ),
            (
                "TRANSFORM_SIZE_8X8",
                TRANSFORM_SIZE_8X8.iter().flatten().copied().collect(),
            ),
        ]
    }

    /// Pairs that are allowed to be byte-identical, with the reason why —
    /// empty today. Any real hit that isn't listed here fails the test.
    const ALLOWED_DUPLICATES: &[(&str, &str)] = &[];

    #[test]
    fn no_two_tables_are_byte_identical() {
        let tables = named_tables();
        let mut hits = Vec::new();
        for (i, (name_a, vals_a)) in tables.iter().enumerate() {
            for (name_b, vals_b) in tables.iter().skip(i + 1) {
                if vals_a == vals_b {
                    let allowed = ALLOWED_DUPLICATES.iter().any(|&(a, b)| {
                        (a == *name_a && b == *name_b) || (a == *name_b && b == *name_a)
                    });
                    if !allowed {
                        hits.push(format!("{name_a} == {name_b} ({} entries)", vals_a.len()));
                    }
                }
            }
        }
        assert!(
            hits.is_empty(),
            "found tables that are byte-for-byte identical to each other, which Table \
             9-11's per-syntax-element ctxIdxOffset assignment gives no legitimate reason \
             for -- this is exactly the shape of the CBF_CHROMA_AC/CBF_CHROMA_DC bug: \
             {hits:?}"
        );
    }

    /// Sanity check on the test itself: every table must appear at least
    /// once, and the two single-`Init` entries (`PREV_INTRA4X4`/
    /// `REM_INTRA4X4`) are deliberately different values (13,41) vs (3,62),
    /// so a trivially-passing empty-list bug in `named_tables` would still
    /// be caught by `no_two_tables_are_byte_identical` finding *nothing* --
    /// this confirms the harness actually inspects real data.
    #[test]
    fn named_tables_is_not_accidentally_empty() {
        let tables = named_tables();
        assert_eq!(
            tables.len(),
            22,
            "expected exactly 22 named tables in this file"
        );
        for (name, vals) in &tables {
            assert!(!vals.is_empty(), "table {name} flattened to zero entries");
        }
    }
}
