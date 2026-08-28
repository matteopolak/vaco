//! Table 8-16 (`α`/`β` thresholds) and Table 8-17 (`tC0`), clause 8.7.2.2.
//!
//! **Transcription tier, stated plainly rather than implied**: this session
//! looked for a primary copy of the source this crate's own provenance row
//! (`iso-iec-14496-10-2002-draft`, `provenance/sources.toml`) claims was
//! acquired, and found none reachable -- the one local file matching that
//! name, in this session's own scratchpad, is a 245-byte HTTP rejection
//! page from a failed fetch, not a spec. So this is **not** a tier-3
//! (checked line-by-line against primary text) transcription in the sense
//! `planning/AGENT-CONSTRAINTS.md`'s three-tier rule uses that phrase for
//! `vaco-codec-h264`'s CAVLC tables, and it would be dishonest to label it
//! that way.
//!
//! What it is instead: transcribed from working, extensively-documented
//! knowledge of these exact 52-entry tables, which are unusually
//! well-attested for a spec table -- they appear byte-for-byte identical
//! across numerous independent, mutually-uninvolved open-source H.264
//! implementations (their own convergent citation of the same clause, not
//! independent derivation, so this is corroborating evidence rather than a
//! second primary source). Backed by the structural checks this module's
//! own tests run (non-decreasing in `indexA`/`indexB`, `TC0` non-decreasing
//! in both `indexA` and `bS`, in-range) and, more importantly, by the
//! actual tier this crate's caller relies on for real confidence: an
//! end-to-end, black-box, byte-exact comparison against real `ffmpeg`
//! output on real content (see `vaco-codec-h264::deblock`'s own tests) --
//! the same method that has caught every wrong-table bug this whole
//! investigation has found, and the one method that does not depend on
//! this module's own transcription being right by construction. A future
//! agent who does obtain a working primary copy should re-check this file
//! against it line by line and upgrade this comment, not just believe it
//! because the pixel comparison currently passes.

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
    [5, 6, 9],
    [6, 7, 10],
    [6, 8, 11],
    [7, 9, 12],
    [8, 10, 13],
    [9, 12, 15],
    [10, 13, 17],
    [11, 16, 20],
    [13, 18, 23],
    [14, 20, 25],
    [16, 23, 27],
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
