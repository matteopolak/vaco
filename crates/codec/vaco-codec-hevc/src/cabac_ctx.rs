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
}
