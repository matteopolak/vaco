//! The three normative CABAC tables, and the two derived ones.
//!
//! # Provenance
//!
//! Transcribed from ITU-T H.264 (ISO/IEC 14496-10) clause 9.3.3.2.1.1:
//!
//! - [`RANGE_TAB_LPS`] is **Table 9-44**, `rangeTabLPS[pStateIdx][qRangeIdx]`.
//! - [`TRANS_IDX_LPS`] and [`TRANS_IDX_MPS`] are **Table 9-45**.
//!
//! ITU-T H.265 clause 9.3.4.3.2 carries the identical tables (its Tables 9-46
//! and 9-47), which is why one engine serves both codecs.
//!
//! A conforming decoder must contain exactly these numbers in exactly this
//! order — they *are* the arithmetic coder, not a description of it. This is the
//! merger case D9/D15 describe, and nothing here comes from any implementation.
//!
//! # The derived tables, and why they exist
//!
//! The spec keeps `pStateIdx` and `valMPS` as two variables and writes
//!
//! ```text
//! if (pStateIdx == 0) valMPS = 1 - valMPS
//! pStateIdx = transIdxLPS[pStateIdx]
//! ```
//!
//! which is a branch inside the hottest loop in video decoding, on a condition
//! that is almost never true and therefore never usefully predicted. Packing the
//! pair into one byte as `(pStateIdx << 1) | valMPS` — the same encoding clause
//! 9.3.1.1's `preCtxState` derivation naturally produces — folds that branch
//! into the table: [`TRANS`] holds the successor state for both outcomes, so a
//! decision is one indexed load and no branch at all.
//!
//! [`TRANS`] and [`LPS_RANGE`] are computed at compile time from the three
//! normative tables above, so there is exactly one transcription of each number
//! and `tests/spec.rs` re-derives both to check the folding.

/// Table 9-44 — `rangeTabLPS[pStateIdx][qRangeIdx]`, ITU-T H.264 clause
/// 9.3.3.2.1.1.
///
/// Row is `pStateIdx` (0–63), column is
/// `qRangeIdx = (ivlCurrRange >> 6) & 3`.
pub const RANGE_TAB_LPS: [[u8; 4]; 64] = [
    [128, 176, 208, 240],
    [128, 167, 197, 227],
    [128, 158, 187, 216],
    [123, 150, 178, 205],
    [116, 142, 169, 195],
    [111, 135, 160, 185],
    [105, 128, 152, 175],
    [100, 122, 144, 166],
    [95, 116, 137, 158],
    [90, 110, 130, 150],
    [85, 104, 123, 142],
    [81, 99, 117, 135],
    [77, 94, 111, 128],
    [73, 89, 105, 122],
    [69, 85, 100, 116],
    [66, 80, 95, 110],
    [62, 76, 90, 104],
    [59, 72, 86, 99],
    [56, 69, 81, 94],
    [53, 65, 77, 89],
    [51, 62, 73, 85],
    [48, 59, 69, 80],
    [46, 56, 66, 76],
    [43, 53, 63, 72],
    [41, 50, 59, 69],
    [39, 48, 56, 65],
    [37, 45, 54, 62],
    [35, 43, 51, 59],
    [33, 41, 48, 56],
    [32, 39, 46, 53],
    [30, 37, 43, 50],
    [29, 35, 41, 48],
    [27, 33, 39, 45],
    [26, 31, 37, 43],
    [24, 30, 35, 41],
    [23, 28, 33, 39],
    [22, 27, 32, 37],
    [21, 26, 30, 35],
    [20, 24, 29, 33],
    [19, 23, 27, 31],
    [18, 22, 26, 30],
    [17, 21, 25, 28],
    [16, 20, 23, 27],
    [15, 19, 22, 25],
    [14, 18, 21, 24],
    [14, 17, 20, 23],
    [13, 16, 19, 22],
    [12, 15, 18, 21],
    [12, 14, 17, 20],
    [11, 14, 16, 19],
    [11, 13, 15, 18],
    [10, 12, 15, 17],
    [10, 12, 14, 16],
    [9, 11, 13, 15],
    [9, 11, 12, 14],
    [8, 10, 12, 14],
    [8, 9, 11, 13],
    [7, 9, 11, 12],
    [7, 9, 10, 12],
    [7, 8, 10, 11],
    [6, 8, 9, 11],
    [6, 7, 9, 10],
    [6, 7, 8, 9],
    [2, 2, 2, 2],
];

/// Table 9-45 — `transIdxLPS[pStateIdx]`, ITU-T H.264 clause 9.3.3.2.1.1.
pub const TRANS_IDX_LPS: [u8; 64] = [
    0, 0, 1, 2, 2, 4, 4, 5, 6, 7, 8, 9, 9, 11, 11, 12, 13, 13, 15, 15, 16, 16, 18, 18, 19, 19, 21,
    21, 22, 22, 23, 24, 24, 25, 26, 26, 27, 27, 28, 29, 29, 30, 30, 30, 31, 32, 32, 33, 33, 33, 34,
    34, 35, 35, 35, 36, 36, 36, 37, 37, 37, 38, 38, 63,
];

/// Table 9-45 — `transIdxMPS[pStateIdx]`, ITU-T H.264 clause 9.3.3.2.1.1.
///
/// `pStateIdx + 1` everywhere except the two absorbing states: 62 stays at 62
/// (the most-skewed adapting state) and 63 stays at 63 (the non-adapting state
/// used for `end_of_slice_flag`).
pub const TRANS_IDX_MPS: [u8; 64] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26,
    27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50,
    51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 62, 63,
];

/// The number of distinct packed context states: `pStateIdx` 0–63 × `valMPS`.
pub const STATE_COUNT: usize = 128;

// The two derived tables are built with plain indexing inside a `const fn`,
// which means an out-of-range index is a *compile* error rather than a runtime
// panic — const evaluation cannot produce a panicking binary. That is the one
// place where `indexing_slicing` is buying nothing, so it is switched off for
// exactly these two builders and nowhere else.
#[allow(
    clippy::indexing_slicing,
    reason = "const-evaluated table construction: an out-of-range index fails the build, \
              not the process"
)]
mod derive {
    use super::{RANGE_TAB_LPS, STATE_COUNT, TRANS_IDX_LPS, TRANS_IDX_MPS};

    /// `TRANS[lps << 8 | state]` — the successor packed state.
    ///
    /// The low half (`lps == 0`) is the MPS transition, the high half the LPS
    /// transition. Both halves are 256 entries wide although only 128 states
    /// exist: the upper 128 mirror the lower, so indexing by a `u8` is provably
    /// in bounds and LLVM removes the check entirely.
    pub(super) const fn trans() -> [u8; 512] {
        let mut t = [0u8; 512];
        let mut i = 0;
        while i < 256 {
            let s = i & (STATE_COUNT - 1);
            let p = s >> 1;
            let mps = (s & 1) as u8;

            // MPS path: pStateIdx advances, valMPS is unchanged.
            t[i] = (TRANS_IDX_MPS[p] << 1) | mps;

            // LPS path: pStateIdx follows transIdxLPS, and valMPS flips exactly
            // when pStateIdx was 0 — clause 9.3.3.2.1.1. Folding the flip into
            // the table is what removes the branch.
            let new_mps = if p == 0 { 1 - mps } else { mps };
            t[256 + i] = (TRANS_IDX_LPS[p] << 1) | new_mps;

            i += 1;
        }
        t
    }

    /// `LPS_RANGE[(state >> 1) * 4 + q]` — `rangeTabLPS`, flattened and indexed
    /// by the packed state so no shift-and-two-index sequence is needed.
    ///
    /// 512 entries for the same provable-in-bounds reason as [`trans`].
    pub(super) const fn lps_range() -> [u8; 512] {
        let mut t = [0u8; 512];
        let mut i = 0;
        while i < 128 {
            let p = i & 63;
            let mut q = 0;
            while q < 4 {
                t[i * 4 + q] = RANGE_TAB_LPS[p][q];
                q += 1;
            }
            i += 1;
        }
        t
    }
}

/// Packed state transition: `TRANS[(lps << 8) | state]`.
///
/// See the module documentation for the packing and why the branch is folded in.
pub const TRANS: [u8; 512] = derive::trans();

/// `rangeTabLPS` flattened for the packed state: `LPS_RANGE[(state >> 1) * 4 + q]`.
pub const LPS_RANGE: [u8; 512] = derive::lps_range();
