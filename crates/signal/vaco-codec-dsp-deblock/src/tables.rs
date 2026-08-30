//! Table 8-16 (`α`/`β` thresholds) and Table 8-17 (`tC0`), clause 8.7.2.2.
//!
//! **Transcription tier**: all three tables are now checked entry by entry
//! against a locally cloned JM 19.1 reference decoder
//! (`provenance/sources.toml`'s `jm-reference-software`, Tier A --
//! `source/app/ldecod/loop_filter.h`'s own `ALPHA_TABLE`, `BETA_TABLE` and
//! `CLIP_TAB`), extracted mechanically rather than read by eye. `ALPHA_TABLE`
//! and `BETA_TABLE` matched the previous transcription exactly. `TC0_TABLE`
//! did **not**: 23 of its 52 rows were wrong (see that table's own doc).
//!
//! The previous version of this comment said these tables were transcribed
//! from recollection, that this was "not a tier-3 transcription", and that
//! the real confidence came from an end-to-end pixel comparison. That was
//! an honest description of a genuinely weak basis, and it was right to
//! distrust: the pixel comparison was passing at 99.78% with a table that
//! was off by one row across half its range. A near-miss whole-picture
//! percentage is not evidence a table is right; it is evidence the wrong
//! entries are rarely reached.

/// `α` (alpha), indexed by `indexA` (clause 8.7.2.2, eq. 8-458).
pub const ALPHA_TABLE: [u8; 52] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 4, 5, 6, 7, 8, 9, 10, 12, 13, 15, 17, 20,
    22, 25, 28, 32, 36, 40, 45, 50, 56, 63, 71, 80, 90, 101, 113, 127, 144, 162, 182, 203, 226,
    255, 255,
];

/// `β` (beta), indexed by `indexB` (clause 8.7.2.2, eq. 8-459).
pub const BETA_TABLE: [u8; 52] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 6, 6, 7, 7, 8, 8,
    9, 9, 10, 10, 11, 11, 12, 12, 13, 13, 14, 14, 15, 15, 16, 16, 17, 17, 18, 18,
];

/// `tC0`, indexed by `[indexA][bS - 1]` for `bS` in `1..=3` -- `bS == 4`
/// (the strong intra filter, clause 8.7.2.4) has no `tC0` at all, a
/// different equation entirely, not a fourth column here.
///
/// **Corrected against JM 19.1's own `CLIP_TAB`** (`loop_filter.h`, columns
/// `1..=3` of its `[52][5]` -- its column `0` is the unused `bS == 0` slot
/// and column `4` mirrors column `3`; `ldecod`'s `edge_loop_luma_ver`
/// indexes it as `ClipTab[Strength]`, so column `n` is `bS == n`). The
/// previous transcription was **off by one row for every `indexA >= 16`**
/// -- it held `CLIP_TAB[indexA + 1]`'s values at `indexA`, plus further
/// divergence in the top rows (`indexA >= 41`, where the correct values are
/// not a pure shift). 23 of 52 rows differed.
///
/// That shift is also why the previous "oracle-guided" hand-correction of
/// `indexA == 30`'s `bS == 3` entry (`3 -> 2`) made this table *less*
/// correct while making one fixture's byte-match number go up: `indexA ==
/// 30`'s correct row is `[1, 1, 2]`, and the pre-existing `[1, 2, 3]` was
/// wrong in its *first two* columns, not its third. Fitting a single table
/// entry to a whole-picture difference percentage can move that percentage
/// in the right direction for the wrong reason; only an entry-by-entry
/// check against a reference decides a table.
pub const TC0_TABLE: [[u8; 3]; 52] = [
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 1],
    [0, 0, 1],
    [0, 0, 1],
    [0, 0, 1],
    [0, 1, 1],
    [0, 1, 1],
    [1, 1, 1],
    [1, 1, 1],
    [1, 1, 1],
    [1, 1, 1],
    [1, 1, 2],
    [1, 1, 2],
    [1, 1, 2],
    [1, 1, 2],
    [1, 2, 3],
    [1, 2, 3],
    [2, 2, 3],
    [2, 2, 4],
    [2, 3, 4],
    [2, 3, 4],
    [3, 3, 5],
    [3, 4, 6],
    [3, 4, 6],
    [4, 5, 7],
    [4, 5, 8],
    [4, 6, 9],
    [5, 7, 10],
    [6, 8, 11],
    [6, 8, 13],
    [7, 10, 14],
    [8, 11, 16],
    [9, 12, 18],
    [10, 13, 20],
    [11, 15, 23],
    [13, 17, 25],
];

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    reason = "test code, fixed-size windows/arrays"
)]
mod tests {
    use super::*;

    #[test]
    fn alpha_and_beta_are_the_documented_length_and_shape() {
        assert_eq!(ALPHA_TABLE.len(), 52);
        assert_eq!(BETA_TABLE.len(), 52);
        assert_eq!(TC0_TABLE.len(), 52);
    }

    #[test]
    fn alpha_is_non_decreasing_in_index_a() {
        for w in ALPHA_TABLE.windows(2) {
            assert!(w[0] <= w[1], "ALPHA_TABLE not non-decreasing at {w:?}");
        }
    }

    #[test]
    fn beta_is_non_decreasing_in_index_b() {
        for w in BETA_TABLE.windows(2) {
            assert!(w[0] <= w[1], "BETA_TABLE not non-decreasing at {w:?}");
        }
    }

    #[test]
    fn tc0_is_non_decreasing_in_index_a_for_every_bs_column() {
        for col in 0..3 {
            for w in TC0_TABLE.windows(2) {
                assert!(
                    w[0][col] <= w[1][col],
                    "TC0_TABLE column {col} not non-decreasing at {w:?}"
                );
            }
        }
    }

    #[test]
    fn tc0_is_non_decreasing_in_bs_for_every_index_a() {
        for row in TC0_TABLE {
            assert!(
                row[0] <= row[1] && row[1] <= row[2],
                "TC0_TABLE row {row:?} not non-decreasing in bS"
            );
        }
    }

    #[test]
    fn alpha_and_beta_are_not_byte_identical() {
        // Cheap structural guard against the CBF_CHROMA_AC-shaped mistake
        // (a whole table copy-pasted from a neighbouring one): these two
        // tables are transcribed from distinct rows of Table 8-16 and have
        // no legitimate reason to coincide.
        assert_ne!(ALPHA_TABLE.to_vec(), BETA_TABLE.to_vec());
    }
}
