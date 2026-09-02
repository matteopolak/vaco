//! Block-size, transform-size, scan-order and quantizer tables the tile
//! decode loop, transform and intra prediction all read from.
//!
//! Most of this module is [`conversion`], [`scan`] and [`quant`] — verbatim,
//! mechanically extracted specification tables (see each submodule's own
//! doc). What is here directly is the small amount of *derived* data the
//! specification defines as a formula or as running text rather than a
//! printed array, plus lookup helpers that turn a raw index into the right
//! row of one of those tables.

pub mod conversion;
pub mod default_cdf;
pub mod quant;
pub mod scan;

pub use conversion::{
    ADJUSTED_TX_SIZE, COEFF_BASE_CTX_OFFSET, DR_INTRA_DERIVATIVE, INTRA_MODE_CONTEXT,
    MAG_REF_OFFSET_WITH_TX_CLASS, MAX_TX_DEPTH, MAX_TX_SIZE_RECT, MI_HEIGHT_LOG2, MI_WIDTH_LOG2,
    MODE_TO_ANGLE, NUM_4X4_BLOCKS_HIGH, NUM_4X4_BLOCKS_WIDE, PARTITION_SUBSIZE,
    SIG_REF_DIFF_OFFSET, SIZE_GROUP, SM_WEIGHTS_TX_4X4, SM_WEIGHTS_TX_8X8, SM_WEIGHTS_TX_16X16,
    SM_WEIGHTS_TX_32X32, SM_WEIGHTS_TX_64X64, SPLIT_TX_SIZE, TRANSFORM_ROW_SHIFT, TX_HEIGHT,
    TX_HEIGHT_LOG2, TX_SIZE_SQR, TX_SIZE_SQR_UP, TX_WIDTH, TX_WIDTH_LOG2,
};

/// `BLOCK_INVALID`, §3: one past the last real `BLOCK_SIZES` ordinal —
/// `Partition_Subsize`'s sentinel for "this partition/size combination does
/// not occur".
pub const BLOCK_INVALID: u8 = 22;

/// `TX_SIZES_ALL`, §3: how many transform-size ordinals exist.
pub const TX_SIZES_ALL: usize = 19;

/// `MI_SIZE`, §3: samples per mode-info unit.
pub const MI_SIZE: u32 = 4;

/// `Block_Width[x]`, §9.3: `4 * Num_4x4_Blocks_Wide[x]` — a formula in the
/// specification's own text, not a printed table, so computed here rather
/// than extracted.
#[must_use]
pub fn block_width(bsize: u8) -> u32 {
    MI_SIZE
        * u32::from(
            NUM_4X4_BLOCKS_WIDE
                .get(usize::from(bsize))
                .copied()
                .unwrap_or(1),
        )
}

/// `Block_Height[x]`, §9.3: `4 * Num_4x4_Blocks_High[x]`.
#[must_use]
pub fn block_height(bsize: u8) -> u32 {
    MI_SIZE
        * u32::from(
            NUM_4X4_BLOCKS_HIGH
                .get(usize::from(bsize))
                .copied()
                .unwrap_or(1),
        )
}

/// One 1D transform kind, as used per-axis by [`crate::transform`] — the row
/// and column transform types §7.13.3 dispatches on. `FlipAdst` is
/// deliberately absent: §7.12.3's reconstruct process applies the flip as an
/// index reversal on the finished 2D residual, so by the time an axis needs
/// a *transform* it is exactly `Adst`. See `crate::transform`'s module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tx1D {
    Dct,
    Adst,
    Identity,
}

/// `Mode_To_Txfm[ mode ]`, §9.3 — the `(column, row)` 1D transform pair an
/// intra mode's residual is expected to look like, used when narrowing the
/// transform-type search space. Printed in the specification as
/// `DCT_DCT`/`ADST_DCT`/... enumerator names, whose own naming convention
/// (cross-checked against Table 6.10.19's per-name row/column description,
/// e.g. "`FLIPADST_DCT`: rows with DCT and columns with FLIPADST") is
/// `{column}_{row}` — transcribed directly (14 entries) rather than
/// mechanically extracted, since the printed values are names, not numbers.
pub const MODE_TO_TXFM: [(Tx1D, Tx1D); 14] = [
    (Tx1D::Dct, Tx1D::Dct),   // DC_PRED:        DCT_DCT
    (Tx1D::Adst, Tx1D::Dct),  // V_PRED:         ADST_DCT
    (Tx1D::Dct, Tx1D::Adst),  // H_PRED:         DCT_ADST
    (Tx1D::Dct, Tx1D::Dct),   // D45_PRED:       DCT_DCT
    (Tx1D::Adst, Tx1D::Adst), // D135_PRED:      ADST_ADST
    (Tx1D::Adst, Tx1D::Dct),  // D113_PRED:      ADST_DCT
    (Tx1D::Dct, Tx1D::Adst),  // D157_PRED:      DCT_ADST
    (Tx1D::Dct, Tx1D::Adst),  // D203_PRED:      DCT_ADST
    (Tx1D::Adst, Tx1D::Dct),  // D67_PRED:       ADST_DCT
    (Tx1D::Adst, Tx1D::Adst), // SMOOTH_PRED:    ADST_ADST
    (Tx1D::Adst, Tx1D::Dct),  // SMOOTH_V_PRED:  ADST_DCT
    (Tx1D::Dct, Tx1D::Adst),  // SMOOTH_H_PRED:  DCT_ADST
    (Tx1D::Adst, Tx1D::Adst), // PAETH_PRED:     ADST_ADST
    (Tx1D::Dct, Tx1D::Dct),   // UV_CFL_PRED:    DCT_DCT
];

/// `Coeff_Base_Pos_Ctx_Offset[3]`, §8.3.2 — `{ SIG_COEF_CONTEXTS_2D,
/// SIG_COEF_CONTEXTS_2D + 5, SIG_COEF_CONTEXTS_2D + 10 }` with
/// `SIG_COEF_CONTEXTS_2D = 26` (§3). Printed as a symbolic expression rather
/// than literal numbers, so computed here rather than extracted.
pub const COEFF_BASE_POS_CTX_OFFSET: [u16; 3] = [26, 31, 36];

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    reason = "test code over fixed-shape spec tables"
)]
mod tests {
    use super::*;

    fn is_permutation(scan: &[u16]) -> bool {
        let mut seen = vec![false; scan.len()];
        for &s in scan {
            let Some(slot) = seen.get_mut(usize::from(s)) else {
                return false;
            };
            if *slot {
                return false;
            }
            *slot = true;
        }
        seen.into_iter().all(|b| b)
    }

    #[test]
    fn every_scan_order_is_a_permutation_of_its_own_length() {
        assert!(is_permutation(&scan::DEFAULT_SCAN_4X4));
        assert!(is_permutation(&scan::MCOL_SCAN_4X4));
        assert!(is_permutation(&scan::MROW_SCAN_4X4));
        assert!(is_permutation(&scan::DEFAULT_SCAN_8X8));
        assert!(is_permutation(&scan::DEFAULT_SCAN_16X16));
        assert!(is_permutation(&scan::DEFAULT_SCAN_32X32));
        assert!(is_permutation(&scan::DEFAULT_SCAN_4X8));
        assert!(is_permutation(&scan::DEFAULT_SCAN_8X4));
        assert!(is_permutation(&scan::DEFAULT_SCAN_16X32));
        assert!(is_permutation(&scan::DEFAULT_SCAN_32X16));
    }

    #[test]
    fn quantizer_tables_are_non_decreasing_in_qindex() {
        for depth_row in &quant::DC_QLOOKUP {
            for w in depth_row.windows(2) {
                assert!(w[0] <= w[1], "Dc_Qlookup must be non-decreasing: {w:?}");
            }
        }
        for depth_row in &quant::AC_QLOOKUP {
            for w in depth_row.windows(2) {
                assert!(w[0] <= w[1], "Ac_Qlookup must be non-decreasing: {w:?}");
            }
        }
    }

    #[test]
    fn block_width_height_match_num_4x4_times_mi_size() {
        assert_eq!(block_width(0), 4); // BLOCK_4X4
        assert_eq!(block_height(0), 4);
        assert_eq!(block_width(3), 8); // BLOCK_8X8
        assert_eq!(block_width(15), 128); // BLOCK_128X128
        assert_eq!(block_height(15), 128);
    }

    #[test]
    fn tx_width_and_height_agree_with_their_log2_tables() {
        for i in 0..TX_SIZES_ALL {
            let w = TX_WIDTH[i];
            let h = TX_HEIGHT[i];
            assert_eq!(u32::from(w), 1u32 << TX_WIDTH_LOG2[i], "index {i}");
            assert_eq!(u32::from(h), 1u32 << TX_HEIGHT_LOG2[i], "index {i}");
        }
    }
}
