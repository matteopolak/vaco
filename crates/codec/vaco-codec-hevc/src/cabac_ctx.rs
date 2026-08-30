//! HEVC's CABAC context initialisation tables and the context bank this
//! crate's I-slice-only decode needs.
//!
//! # Source and scope of this table
//!
//! Transcribed from the HM reference decoder's `TLibCommon/ContextTables.h`
//! (BSD-3-Clause; Tier A per `planning/research/07-legal-patents-licensing.md`
//! §1.6.1 — HM is a permissively licensed reference implementation, not
//! `FFmpeg`, and D7/D15's clean-room rule is about `FFmpeg` specifically). Each
//! table there carries three rows — B slice, P slice, I slice, in that literal
//! order — because ITU-T H.265 §9.3.2.2's `initType` derivation lets a P or B
//! slice select either of two rows via `cabac_init_flag`. This crate decodes
//! I-slices only (see the crate doc), and clause 9.3.2.2 fixes `initType = 0`
//! for every I slice unconditionally — `cabac_init_flag` is not even present
//! in an I-slice header — so **only the I-slice row of each table is
//! transcribed here**; the B/P rows this crate will never select are left out
//! rather than copied in and dead.
//!
//! Checked to tier 1 (every table's length matches the context count its own
//! `NUM_*_CTX` constant in the source declares) and read once directly
//! against the primary source rather than from recollection; not yet
//! independently re-verified entry-by-entry against a second transcription
//! (tier 3 in `AGENT-CONSTRAINTS.md`'s sense) beyond the differential
//! byte-exactness this crate's own oracle test provides, which would catch a
//! wrong value in exactly the same way it caught the sign-hiding and MPM
//! bugs during development.

use vaco_codec_cabac::{ContextModel, init_contexts_hevc};

// ITU-T H.265 §9.3.2.2 Table 9-5 / HM `ContextTables.h`'s "I slice" rows
// only — see the module doc for why the B/P rows are absent.

const INIT_SPLIT_CU_FLAG: [u8; 3] = [139, 141, 157];
const INIT_PART_SIZE: [u8; 4] = [184, 154, 154, 154];
const INIT_PREV_INTRA_LUMA_PRED: [u8; 1] = [184];
const INIT_INTRA_CHROMA_PRED_MODE: [u8; 2] = [63, 139];
const INIT_TRANS_SUBDIV_FLAG: [u8; 3] = [153, 138, 138];
// luma (5) then chroma (5): {111,141,CNU,CNU,CNU, 94,138,182,154,154}. `CNU`
// (154) is HM's "context not used" filler for entries this decode order
// never reads (H.265's transform-tree cbf context only ever uses `ctxInc`
// 0 or 1; indices 2..4 are unreachable dead weight in the source table
// too, kept here only so the table's shape matches HM's declared size).
const INIT_QT_CBF: [u8; 10] = [111, 141, 154, 154, 154, 94, 138, 182, 154, 154];
const INIT_TRANSFORM_SKIP: [u8; 2] = [139, 139];

// Significance map: DC + 4x4(8) + 8x8-first(6) + 8x8-other(6) + NxN-first(6)
// + NxN-other(6) + single(1) = 28 for luma; DC + 4x4(8) + 8x8-any(3) +
// NxN-any(3) + single(1) = 16 for chroma. Concatenated exactly as HM's
// `INIT_SIG_FLAG[ISLICE]` lays them out (`ISLICE_LUMA_SIGNIFICANCE_CONTEXT`
// followed by `ISLICE_CHROMA_SIGNIFICANCE_CONTEXT`).
const INIT_SIG_COEFF_FLAG: [u8; 44] = [
    111, 111, 125, 110, 110, 94, 124, 108, 124, 107, 125, 141, 179, 153, 125, 107, 125, 141, 179, 153, 125, 107, 125,
    141, 179, 153, 125, 141, 140, 139, 182, 182, 152, 136, 152, 136, 153, 136, 139, 111, 136, 139, 111, 111,
];

const INIT_SIG_COEFF_GROUP: [u8; 4] = [91, 171, 134, 141];

// last_sig_coeff_{x,y}_prefix: 15 luma + 15 chroma each.
const INIT_LAST_SIG_X: [u8; 30] = [
    110, 110, 124, 125, 140, 153, 125, 127, 140, 109, 111, 143, 127, 111, 79, 108, 123, 63, 154, 154, 154, 154, 154,
    154, 154, 154, 154, 154, 154, 154,
];
const INIT_LAST_SIG_Y: [u8; 30] = INIT_LAST_SIG_X;

// coeff_abs_level_greater1_flag: 4 sets luma (16) + 2 sets chroma (8).
const INIT_GREATER1: [u8; 24] = [
    140, 92, 137, 138, 140, 152, 138, 139, 153, 74, 149, 92, 139, 107, 122, 152, 140, 179, 166, 182, 140, 227, 122,
    197,
];
// coeff_abs_level_greater2_flag: 4 sets luma (4) + 2 sets chroma (2).
const INIT_GREATER2: [u8; 6] = [138, 153, 136, 167, 152, 152];

// `sao_merge_left_flag`/`sao_merge_up_flag` share this one context (HM's
// `m_cSaoMergeSCModel.get(0, 0, 0)` for both), and `sao_type_idx_luma`/
// `sao_type_idx_chroma` share this other one — both single-context tables,
// I-slice row only (see the module doc).
const INIT_SAO_MERGE_FLAG: [u8; 1] = [153];
const INIT_SAO_TYPE_IDX: [u8; 1] = [200];

// `cu_qp_delta_abs`'s truncated-unary prefix (§9.3.3.10): bin 0 uses ctx 0,
// every further bin shares ctx 1 (HM's `xReadUnaryMaxSymbol(..., iOffset=1)`
// against `m_cCUDeltaQpSCModel`). HM declares `NUM_DELTA_QP_CTX == 3` and all
// three I-slice-row entries are `154` regardless — the third is genuinely
// unaddressed by any binarisation, kept only so this table's length matches
// the source's declared size (the same convention `INIT_QT_CBF`'s unused
// entries already follow).
const INIT_CU_QP_DELTA: [u8; 3] = [154, 154, 154];

// ---------------------------------------------------------------------
// P-slice context-initialisation tables, ITU-T H.265 §9.3.2.2's `initType`
// 1 (P, the common case) and `initType` 0 (B — selected instead of `initType`
// 1 whenever `cabac_init_present_flag && cabac_init_flag`, §9.3.2.2's own
// swap rule; confirmed directly against HM's `TDecSbac::resetEntropy`, which
// swaps `sliceType` from `P_SLICE` to `B_SLICE` in exactly that condition,
// `B_SLICE == 0`/`P_SLICE == 1`/`I_SLICE == 2` being the literal row order
// HM's own `ContextTables.h` uses). This crate does not decode B slices, so
// only these two rows of each three-row HM table are transcribed — the
// I-slice row above already covers `initType` 2, which a P slice never
// selects (`cabac_init_flag` only ever swaps *between* the B and P rows,
// never to the I row: HM's own `switch` has no `I_SLICE` case).
//
// Every array below is `[normal, cabac_init_flag_set]` — index `0` is what
// §9.3.2.2 calls `initType = 1` (P, the value almost every real stream
// uses), index `1` is `initType = 0` (B, selected only when
// `cabac_init_present_flag && cabac_init_flag`).

const INIT_SPLIT_CU_FLAG_P: [[u8; 3]; 2] = [[107, 139, 126], [107, 139, 126]];
const INIT_PART_SIZE_P: [[u8; 4]; 2] = [[154, 139, 154, 154], [154, 139, 154, 154]];
const INIT_PREV_INTRA_LUMA_PRED_P: [[u8; 1]; 2] = [[154], [183]];
const INIT_INTRA_CHROMA_PRED_MODE_P: [[u8; 2]; 2] = [[152, 139], [152, 139]];
const INIT_TRANS_SUBDIV_FLAG_P: [[u8; 3]; 2] = [[124, 138, 94], [224, 167, 122]];
const INIT_QT_CBF_P: [[u8; 10]; 2] = [
    [153, 111, 154, 154, 154, 149, 107, 167, 154, 154],
    [153, 111, 154, 154, 154, 149, 92, 167, 154, 154],
];
const INIT_TRANSFORM_SKIP_P: [[u8; 2]; 2] = [[139, 139], [139, 139]];
#[rustfmt::skip]
const INIT_SIG_COEFF_FLAG_P: [[u8; 44]; 2] = [
    [
        155, 154, 139, 153, 139, 123, 123, 63, 153, 166, 183, 140, 136, 153, 154, 166, 183, 140, 136, 153, 154, 166,
        183, 140, 136, 153, 154, 140, 170, 153, 123, 123, 107, 121, 107, 121, 167, 151, 183, 140, 151, 183, 140, 140,
    ],
    [
        170, 154, 139, 153, 139, 123, 123, 63, 124, 166, 183, 140, 136, 153, 154, 166, 183, 140, 136, 153, 154, 166,
        183, 140, 136, 153, 154, 140, 170, 153, 138, 138, 122, 121, 122, 121, 167, 151, 183, 140, 151, 183, 140, 140,
    ],
];
const INIT_SIG_COEFF_GROUP_P: [[u8; 4]; 2] = [[121, 140, 61, 154], [121, 140, 61, 154]];
#[rustfmt::skip]
const INIT_LAST_SIG_P: [[u8; 30]; 2] = [
    [125, 110, 94, 110, 95, 79, 125, 111, 110, 78, 110, 111, 111, 95, 94, 108, 123, 108, 154, 154, 154, 154, 154, 154, 154, 154, 154, 154, 154, 154],
    [125, 110, 124, 110, 95, 94, 125, 111, 111, 79, 125, 126, 111, 111, 79, 108, 123, 93, 154, 154, 154, 154, 154, 154, 154, 154, 154, 154, 154, 154],
];
#[rustfmt::skip]
const INIT_GREATER1_P: [[u8; 24]; 2] = [
    [154, 196, 196, 167, 154, 152, 167, 182, 182, 134, 149, 136, 153, 121, 136, 137, 169, 194, 166, 167, 154, 167, 137, 182],
    [154, 196, 167, 167, 154, 152, 167, 182, 182, 134, 149, 136, 153, 121, 136, 122, 169, 208, 166, 167, 154, 152, 167, 182],
];
const INIT_GREATER2_P: [[u8; 6]; 2] = [[107, 167, 91, 122, 107, 167], [107, 167, 91, 107, 107, 167]];
const INIT_SAO_TYPE_IDX_P: [[u8; 1]; 2] = [[185], [160]];
const INIT_CU_QP_DELTA_P: [[u8; 3]; 2] = [[154, 154, 154], [154, 154, 154]];

// Inter-only tables: never read by an I slice at all (its own HM row is the
// dummy `CNU` filler), so only the P/B rows exist here — there is no I-row
// counterpart to omit-by-symmetry the way the module doc explains for the
// tables above.
const INIT_SKIP_FLAG_P: [[u8; 3]; 2] = [[197, 185, 201], [197, 185, 201]];
const INIT_MERGE_FLAG_P: [[u8; 1]; 2] = [[110], [154]];
const INIT_MERGE_IDX_P: [[u8; 1]; 2] = [[122], [137]];
const INIT_PRED_MODE_P: [[u8; 1]; 2] = [[149], [134]];
const INIT_MVD_P: [[u8; 2]; 2] = [[140, 198], [169, 198]];
const INIT_REF_PIC_P: [[u8; 2]; 2] = [[153, 153], [153, 153]];
const INIT_QT_ROOT_CBF_P: [[u8; 1]; 2] = [[79], [79]];
const INIT_MVP_IDX_P: [[u8; 1]; 2] = [[168], [168]];
// `inter_pred_idc` (§7.3.8.6/§9.3.4.2.1) — B-slice-only syntax (a P slice
// infers `PRED_L0` and never parses it), included here rather than in a
// separate table so `new_inter_slice`'s one `row_init!` macro covers it too.
// HM's own `INIT_INTER_DIR` table has identical B and P rows (confirmed
// directly against `ContextTables.h`), so index 0 (initType 1, "P") and
// index 1 (initType 0, "B") are the same five values.
const INIT_INTER_DIR_P: [[u8; 5]; 2] = [[95, 79, 63, 31, 31], [95, 79, 63, 31, 31]];

/// `ctxIndMap4x4`, HM `TComRom.cpp` — the fixed per-position context offset
/// for a 4x4 transform block's `sig_coeff_flag`, §9.3.4.2.5.
pub(crate) const CTX_IND_MAP_4X4: [u8; 16] = [0, 1, 4, 5, 2, 3, 4, 5, 6, 6, 8, 8, 7, 7, 8, 8];

/// `g_uiGroupIdx`, HM `TComRom.cpp` — maps a 0-based coefficient position to
/// its "group" for `last_sig_coeff_{x,y}_prefix`'s truncated-unary
/// binarization, §9.3.3.1.2 Table 9-31's inverse.
pub(crate) const GROUP_IDX: [u8; 32] = [
    0, 1, 2, 3, 4, 4, 5, 5, 6, 6, 6, 6, 7, 7, 7, 7, 8, 8, 8, 8, 8, 8, 8, 8, 9, 9, 9, 9, 9, 9, 9, 9,
];

/// `g_uiMinInGroup`, HM `TComRom.cpp` — the smallest coefficient position
/// belonging to each group index.
pub(crate) const MIN_IN_GROUP: [u32; 10] = [0, 1, 2, 3, 4, 6, 8, 12, 16, 24];

/// One I-slice's worth of CABAC contexts for every syntax element this
/// crate's transform-tree/residual-coding walk reads. Built once per slice
/// (or per WPP-restart point, not supported here — see the crate doc) from
/// [`ContextBank::new`] and threaded through by `&mut` for the rest of the
/// slice segment.
// `Clone`/`Copy`: WPP's own context-synchronisation rule (§9.3.2.3) needs a
// whole-bank snapshot taken after one CTU and restored verbatim before
// another, row-boundaries apart — `ContextModel` is `Copy` for exactly this
// reason (see its own doc comment), so every field here is too.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ContextBank {
    pub split_cu_flag: [ContextModel; 3],
    pub part_size: [ContextModel; 4],
    pub prev_intra_luma_pred: [ContextModel; 1],
    pub intra_chroma_pred_mode: [ContextModel; 2],
    pub trans_subdiv_flag: [ContextModel; 3],
    pub qt_cbf: [ContextModel; 10],
    pub transform_skip: [ContextModel; 2],
    pub sig_coeff_flag: [ContextModel; 44],
    pub sig_coeff_group: [ContextModel; 4],
    pub last_sig_x: [ContextModel; 30],
    pub last_sig_y: [ContextModel; 30],
    pub greater1: [ContextModel; 24],
    pub greater2: [ContextModel; 6],
    pub sao_merge_flag: [ContextModel; 1],
    pub sao_type_idx: [ContextModel; 1],
    pub cu_qp_delta: [ContextModel; 3],
    /// `cu_skip_flag` (§7.3.8.5) — `Default::default()`-valued (state 0,
    /// `pStateIdx`/`valMPS` both zero) on an I-slice `ContextBank`, which is
    /// harmless: `ctu::coding_unit` never reads it for an I slice (`slice
    /// isIntra()` skips `skip_flag` entirely, mirroring HM's own
    /// `parseSkipFlag`).
    pub skip_flag: [ContextModel; 3],
    pub merge_flag: [ContextModel; 1],
    pub merge_idx: [ContextModel; 1],
    pub pred_mode: [ContextModel; 1],
    pub mvd: [ContextModel; 2],
    pub ref_pic: [ContextModel; 2],
    pub qt_root_cbf: [ContextModel; 1],
    pub mvp_idx: [ContextModel; 1],
    /// `inter_pred_idc` (§7.3.8.6) — B-slice only, see [`INIT_INTER_DIR_P`]'s
    /// own doc.
    pub inter_dir: [ContextModel; 5],
}

fn init<const N: usize>(table: &[u8; N], qp: i8) -> [ContextModel; N] {
    let mut dst = [ContextModel::default(); N];
    init_contexts_hevc(&mut dst, table, qp);
    dst
}

impl ContextBank {
    /// Build a fresh context bank for `SliceQPY` (clause 7.4.7.1's derived
    /// slice QP, clamped by the caller to what §7.4.9.10 allows — this
    /// function trusts its input rather than re-deriving the clamp).
    #[must_use]
    pub(crate) fn new(slice_qp: i8) -> Self {
        Self {
            split_cu_flag: init(&INIT_SPLIT_CU_FLAG, slice_qp),
            part_size: init(&INIT_PART_SIZE, slice_qp),
            prev_intra_luma_pred: init(&INIT_PREV_INTRA_LUMA_PRED, slice_qp),
            intra_chroma_pred_mode: init(&INIT_INTRA_CHROMA_PRED_MODE, slice_qp),
            trans_subdiv_flag: init(&INIT_TRANS_SUBDIV_FLAG, slice_qp),
            qt_cbf: init(&INIT_QT_CBF, slice_qp),
            transform_skip: init(&INIT_TRANSFORM_SKIP, slice_qp),
            sig_coeff_flag: init(&INIT_SIG_COEFF_FLAG, slice_qp),
            sig_coeff_group: init(&INIT_SIG_COEFF_GROUP, slice_qp),
            last_sig_x: init(&INIT_LAST_SIG_X, slice_qp),
            last_sig_y: init(&INIT_LAST_SIG_Y, slice_qp),
            greater1: init(&INIT_GREATER1, slice_qp),
            greater2: init(&INIT_GREATER2, slice_qp),
            sao_merge_flag: init(&INIT_SAO_MERGE_FLAG, slice_qp),
            sao_type_idx: init(&INIT_SAO_TYPE_IDX, slice_qp),
            cu_qp_delta: init(&INIT_CU_QP_DELTA, slice_qp),
            // Never read on an I-slice decode path (see each field's own
            // doc); `ContextModel::default()` is as good a value as any.
            skip_flag: [ContextModel::default(); 3],
            merge_flag: [ContextModel::default(); 1],
            merge_idx: [ContextModel::default(); 1],
            pred_mode: [ContextModel::default(); 1],
            mvd: [ContextModel::default(); 2],
            ref_pic: [ContextModel::default(); 2],
            qt_root_cbf: [ContextModel::default(); 1],
            mvp_idx: [ContextModel::default(); 1],
            inter_dir: [ContextModel::default(); 5],
        }
    }

    /// Build a fresh context bank for a P slice — §9.3.2.2's `initType = 1`
    /// by default, swapped to `initType = 0` when `cabac_init_flag` is set
    /// (the *same* direction `initType = 1` selects for a B slice — see
    /// [`ContextBank::new_b_slice`]'s own doc for why the two are mirror
    /// images of each other, not the same function called with a flipped
    /// argument).
    #[must_use]
    pub(crate) fn new_p_slice(slice_qp: i8, cabac_init_flag: bool) -> Self {
        Self::build(slice_qp, usize::from(cabac_init_flag))
    }

    /// Build a fresh context bank for a B slice — §9.3.2.2's `initType = 0`
    /// by default, swapped to `initType = 1` when `cabac_init_flag` is set.
    /// This is the *opposite* row-selection direction from
    /// [`ContextBank::new_p_slice`]: both this crate's `[[u8; N]; 2]` tables
    /// are laid out as `[initType == 1 ("P"), initType == 0 ("B")]` (the
    /// module doc's own convention, kept from before B slices existed), so a
    /// clear `cabac_init_flag` means "use my own slice type's row" for
    /// either kind, which is row index 1 for a default-initType-0 B slice
    /// and row index 0 for a default-initType-1 P slice — not the same `row
    /// = usize::from(cabac_init_flag)` formula both ways.
    #[must_use]
    pub(crate) fn new_b_slice(slice_qp: i8, cabac_init_flag: bool) -> Self {
        Self::build(slice_qp, usize::from(!cabac_init_flag))
    }

    /// Shared row-indexed builder both [`ContextBank::new_p_slice`] and
    /// [`ContextBank::new_b_slice`] delegate to, once each has resolved
    /// §9.3.2.2's `initType` down to this table convention's own `row`
    /// (`0` = the "P" column, `1` = the "B" column of every `[[u8; N]; 2]`
    /// table above).
    fn build(slice_qp: i8, row: usize) -> Self {
        macro_rules! row_init {
            ($table:expr, $n:expr) => {{
                // Array-pattern destructuring rather than `$table[row]`:
                // this crate denies `clippy::indexing_slicing` crate-wide
                // with no per-site silencing, and a `[[u8; N]; 2]` table has
                // exactly two rows to destructure regardless of `$n`.
                let [row0, row1]: [[u8; $n]; 2] = $table;
                let t: [u8; $n] = if row == 1 { row1 } else { row0 };
                init(&t, slice_qp)
            }};
        }
        Self {
            split_cu_flag: row_init!(INIT_SPLIT_CU_FLAG_P, 3),
            part_size: row_init!(INIT_PART_SIZE_P, 4),
            prev_intra_luma_pred: row_init!(INIT_PREV_INTRA_LUMA_PRED_P, 1),
            intra_chroma_pred_mode: row_init!(INIT_INTRA_CHROMA_PRED_MODE_P, 2),
            trans_subdiv_flag: row_init!(INIT_TRANS_SUBDIV_FLAG_P, 3),
            qt_cbf: row_init!(INIT_QT_CBF_P, 10),
            transform_skip: row_init!(INIT_TRANSFORM_SKIP_P, 2),
            sig_coeff_flag: row_init!(INIT_SIG_COEFF_FLAG_P, 44),
            sig_coeff_group: row_init!(INIT_SIG_COEFF_GROUP_P, 4),
            last_sig_x: row_init!(INIT_LAST_SIG_P, 30),
            last_sig_y: row_init!(INIT_LAST_SIG_P, 30),
            greater1: row_init!(INIT_GREATER1_P, 24),
            greater2: row_init!(INIT_GREATER2_P, 6),
            sao_merge_flag: init(&INIT_SAO_MERGE_FLAG, slice_qp),
            sao_type_idx: row_init!(INIT_SAO_TYPE_IDX_P, 1),
            cu_qp_delta: row_init!(INIT_CU_QP_DELTA_P, 3),
            skip_flag: row_init!(INIT_SKIP_FLAG_P, 3),
            merge_flag: row_init!(INIT_MERGE_FLAG_P, 1),
            merge_idx: row_init!(INIT_MERGE_IDX_P, 1),
            pred_mode: row_init!(INIT_PRED_MODE_P, 1),
            mvd: row_init!(INIT_MVD_P, 2),
            ref_pic: row_init!(INIT_REF_PIC_P, 2),
            qt_root_cbf: row_init!(INIT_QT_ROOT_CBF_P, 1),
            mvp_idx: row_init!(INIT_MVP_IDX_P, 1),
            inter_dir: row_init!(INIT_INTER_DIR_P, 5),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_table_builds_without_panicking_across_the_qp_range() {
        for qp in -12i8..=51 {
            let _ = ContextBank::new(qp);
        }
    }

    #[test]
    fn every_p_slice_table_builds_without_panicking_across_the_qp_range() {
        for qp in -12i8..=51 {
            let _ = ContextBank::new_p_slice(qp, false);
            let _ = ContextBank::new_p_slice(qp, true);
        }
    }

    #[test]
    fn every_b_slice_table_builds_without_panicking_across_the_qp_range() {
        for qp in -12i8..=51 {
            let _ = ContextBank::new_b_slice(qp, false);
            let _ = ContextBank::new_b_slice(qp, true);
        }
    }

    #[test]
    fn a_b_slice_s_default_cabac_init_flag_selects_the_opposite_row_from_a_p_slice() {
        // §9.3.2.2: a P slice defaults to initType=1, a B slice to
        // initType=0 — `cabac_init_flag` swaps *away* from the slice's own
        // default toward the other kind's row. Since `INIT_MERGE_FLAG_P`'s
        // two rows differ (`[110]` vs `[154]`), the two constructors' default
        // (`cabac_init_flag == false`) banks must select opposite rows of
        // the same table rather than the same `row = usize::from(flag)`
        // formula.
        let p_default = ContextBank::new_p_slice(26, false);
        let b_default = ContextBank::new_b_slice(26, false);
        assert_ne!(p_default.merge_flag[0], b_default.merge_flag[0]);
        // Swapping `cabac_init_flag` on either slice type lands on the same
        // context state as the other type's own default.
        let p_swapped = ContextBank::new_p_slice(26, true);
        let b_swapped = ContextBank::new_b_slice(26, true);
        assert_eq!(p_default.merge_flag[0], b_swapped.merge_flag[0]);
        assert_eq!(b_default.merge_flag[0], p_swapped.merge_flag[0]);
    }
}
