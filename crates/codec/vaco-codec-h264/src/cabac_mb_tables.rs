#![allow(clippy::unreadable_literal, reason = "spec tables, not derived numbers")]
//! CABAC macroblock-layer context-initialisation tables, ITU-T H.264
//! clause 9.3.1.1, Tables 9-12 through 9-18 — the `(m, n)` pairs
//! [`crate::cabac_mb`] builds its per-syntax-element [`ContextInit`] arrays
//! from. Every value here is transcribed row-by-row from
//! `provenance/vaco-codec-h264.toml`'s `iso-iec-14496-10-2002-draft`
//! (the same source `cavlc_tables.rs` and `cabac_residual.rs` were checked
//! against), not from recollection — this file did not exist before #419's
//! CABAC macroblock-layer work, so there is no earlier, less-verified pass
//! to compare against the way those two files describe.
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
#[allow(dead_code, reason = "kept for a follow-up dispatch landing B-slice CABAC; see mb.rs's own module doc")]
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
#[allow(dead_code, reason = "kept for a follow-up dispatch landing B-slice CABAC; see mb.rs's own module doc")]
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
#[allow(dead_code, reason = "kept for a follow-up dispatch landing B-slice CABAC; see mb.rs's own module doc")]
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
#[rustfmt::skip]
pub(crate) const CBF_CHROMA_AC: [[Init; 4]; 4] = [
    [(-1, 74), (5, 54), (3, 55), (0, 65)],
    [(-6, 97), (6, 60), (7, 56), (-2, 79)],
    [(-7, 91), (6, 59), (7, 55), (0, 72)],
    [(-20, 127), (6, 69), (8, 61), (-4, 92)],
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
