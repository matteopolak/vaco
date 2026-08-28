//! Clause 8.5.4's inverse scanning process for transform coefficients --
//! turning a decoded coefficient list, still in per-block *forward scan
//! order* (exactly what [`crate::cabac_residual::residual_block_cabac`]
//! produces, and what [`crate::mb::MbResidual`] stores), into the raster
//! `c[i][j]` array [`crate::dequant`]'s functions expect.
//!
//! Frame macroblocks only (the inverse *zig-zag* scan, Table 8-12's first
//! row) -- this crate's own scope line already excludes MBAFF/field
//! pictures entirely (`mb.rs`'s `check_scope`), so the inverse *field*
//! scan (Table 8-12's second row) is never reachable and is not
//! implemented.

#![allow(
    dead_code,
    reason = "exercised by this module's own tests and by crate::reconstruct; not yet wired into a general multi-macroblock reconstruction loop"
)]

use crate::cabac_residual::CabacResidual;

/// Table 8-12's zig-zag row: `ZIGZAG_4X4[idx]` is `(i, j)` for `cij`, idx
/// 0..15. Transcribed directly from the primary text's own table (the
/// "zig-zag" row, not "field") -- `idx = 0` is always `(0, 0)`, which is
/// exactly why a DC coefficient (always at scan position 0 in whatever
/// list clause 8.5.2/8.5.3 build) always lands at raster position `(0,
/// 0)` regardless of how the rest of a block's own AC scan positions are
/// offset (see [`build_luma_ac_block`]'s own doc for why that matters).
const ZIGZAG_4X4: [(u8, u8); 16] = [
    (0, 0),
    (0, 1),
    (1, 0),
    (2, 0),
    (1, 1),
    (0, 2),
    (0, 3),
    (1, 2),
    (2, 1),
    (3, 0),
    (3, 1),
    (2, 2),
    (1, 3),
    (2, 3),
    (3, 2),
    (3, 3),
];

/// Clause 8.5.4 applied to a plain 16-entry scan list (used as-is for
/// `Intra16x16DCLevel`, clause 8.5.2 step 1a). `position_offset` shifts
/// every stored position before the zig-zag lookup -- eq. (8-244)'s
/// `lumaList[k] = Intra16x16ACLevel[luma4x4BlkIdx][k - 1]` means an AC
/// coefficient's own scan position `p` (0..14, `residual_block_cabac`'s
/// own `max_num_coeff = 15` for this case) occupies zig-zag index `p +
/// 1`, not `p` -- `offset = 1` for that caller, `offset = 0` for a plain
/// 16-position block (luma DC, or a non-`Intra_16x16` 4x4 block that
/// never has this DC/AC split at all).
///
/// A `None` residual (the block's own `coded_block_flag` was `0`) yields
/// an all-zero array, matching "implicitly zero, not undecoded".
#[must_use]
fn inverse_zigzag_scan_4x4(residual: Option<&CabacResidual>, position_offset: usize) -> [i32; 16] {
    let mut c = [0i32; 16];
    let Some(res) = residual else { return c };
    for (&level, &pos) in res.levels.iter().zip(res.positions.iter()) {
        let idx = usize::from(pos) + position_offset;
        if let Some(&(i, j)) = ZIGZAG_4X4.get(idx)
            && let Some(slot) = c.get_mut(usize::from(i) * 4 + usize::from(j))
        {
            *slot = level;
        }
    }
    c
}

/// Clause 8.5.2 step 1a: `Intra16x16DCLevel`, a plain 16-position scan
/// list with no DC/AC split of its own (unlike the AC blocks it feeds).
#[must_use]
pub(crate) fn inverse_scan_luma_dc(residual: Option<&CabacResidual>) -> [i32; 16] {
    inverse_zigzag_scan_4x4(residual, 0)
}

/// Clause 8.5.2 steps 2a/2b: one luma AC block's `lumaList`, already
/// carrying `dcY`'s own value (this block's own `luma4x4BlkIdx`'s share
/// of the macroblock's DC transform, clause 8.5.2 step 1) at position
/// `(0, 0)` -- `dc_val` is that value, `ac` is this block's own decoded
/// `Intra16x16ACLevel` residual (positions 0..14, `residual_block_cabac`'s
/// `max_num_coeff = 15`).
#[must_use]
pub(crate) fn build_luma_ac_block(dc_val: i32, ac: Option<&CabacResidual>) -> [i32; 16] {
    let mut c = inverse_zigzag_scan_4x4(ac, 1);
    if let Some(first) = c.first_mut() {
        *first = dc_val;
    }
    c
}

/// Clause 8.5.3 step 1a: `ChromaDCLevel`'s own inverse *raster* scan (not
/// zig-zag at all -- eq. (8-246) assigns `c00, c01, c10, c11` straight
/// from scan positions `0, 1, 2, 3`), producing the row-major `c[2*i+j]`
/// array [`vaco_codec_dsp_idct::h264::chroma_dc_hadamard2x2`] expects.
#[must_use]
pub(crate) fn inverse_scan_chroma_dc(residual: Option<&CabacResidual>) -> [i32; 4] {
    let mut c = [0i32; 4];
    let Some(res) = residual else { return c };
    for (&level, &pos) in res.levels.iter().zip(res.positions.iter()) {
        if let Some(slot) = c.get_mut(usize::from(pos)) {
            *slot = level;
        }
    }
    c
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]
mod tests {
    use super::*;

    fn residual(levels: &[i32], positions: &[u8]) -> CabacResidual {
        CabacResidual {
            levels: levels.to_vec(),
            positions: positions.to_vec(),
        }
    }

    /// Table 8-12's zig-zag row, spot-checked at both ends and the middle
    /// against the primary text's own listing, independently of the
    /// `const` array's own transcription -- the "check the table twice"
    /// discipline this crate applies to every other transcribed table.
    #[test]
    fn zigzag_table_matches_table_8_12_spot_checks() {
        assert_eq!(ZIGZAG_4X4[0], (0, 0));
        assert_eq!(ZIGZAG_4X4[1], (0, 1));
        assert_eq!(ZIGZAG_4X4[2], (1, 0));
        assert_eq!(ZIGZAG_4X4[9], (3, 0));
        assert_eq!(ZIGZAG_4X4[15], (3, 3));
    }

    #[test]
    #[allow(
        clippy::needless_range_loop,
        reason = "both a and b are indices into the same array (a pairwise uniqueness check) and are also used directly in the assertion message -- an iterator/enumerate form would still need both back"
    )]
    fn no_two_zigzag_entries_are_the_same_position() {
        for a in 0..16 {
            for b in (a + 1)..16 {
                assert_ne!(
                    ZIGZAG_4X4[a], ZIGZAG_4X4[b],
                    "zig-zag scan must be a bijection: idx {a} and {b} both map to {:?}",
                    ZIGZAG_4X4[a]
                );
            }
        }
    }

    #[test]
    fn a_single_dc_only_coefficient_lands_at_raster_0_0() {
        let r = residual(&[7], &[0]);
        let c = inverse_scan_luma_dc(Some(&r));
        assert_eq!(c[0], 7);
        assert!(c[1..].iter().all(|&v| v == 0));
    }

    #[test]
    fn no_residual_is_all_zero() {
        assert_eq!(inverse_scan_luma_dc(None), [0i32; 16]);
        assert_eq!(inverse_scan_chroma_dc(None), [0i32; 4]);
        assert_eq!(build_luma_ac_block(5, None), {
            let mut c = [0i32; 16];
            c[0] = 5;
            c
        });
    }

    /// eq. (8-244)'s own `+1` shift: an AC coefficient at the AC block's
    /// own scan position 0 occupies zig-zag index 1, i.e. raster (0, 1) --
    /// not raster (0, 0), which is reserved for `dc_val`.
    #[test]
    fn ac_position_zero_lands_at_raster_0_1_not_0_0() {
        let ac = residual(&[9], &[0]);
        let c = build_luma_ac_block(3, Some(&ac));
        assert_eq!(c[0], 3, "dc_val must occupy (0,0)");
        assert_eq!(
            c[1], 9,
            "AC scan position 0 must occupy (0,1) via the +1 shift"
        );
    }

    /// Clause 8.5.3 eq. (8-246): chroma DC is inverse *raster* scan, not
    /// zig-zag -- scan position 2 must land at raster (1, 0), which is
    /// where zig-zag's own idx 2 (`(1, 0)`) would coincidentally also put
    /// it, so this checks position 1 instead (raster (0, 1) under raster
    /// scan; zig-zag's own idx 1 is also `(0, 1)` -- so check position 3,
    /// where the two scans genuinely disagree: raster puts idx 3 at (1,
    /// 1), zig-zag puts idx 3 at (2, 0), a position that does not even
    /// exist in a 2x2 block).
    #[test]
    fn chroma_dc_uses_raster_scan_not_zigzag() {
        let r = residual(&[1, 2, 3, 4], &[0, 1, 2, 3]);
        let c = inverse_scan_chroma_dc(Some(&r));
        assert_eq!(
            c,
            [1, 2, 3, 4],
            "ChromaDCLevel must map straight through, eq. (8-246)"
        );
    }
}
