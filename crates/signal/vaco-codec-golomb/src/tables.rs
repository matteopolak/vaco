//! ITU-T H.264 clause 9.1.2, Table 9-4 — the `me(v)` mapping.
//!
//! `me(v)` is the one Exp-Golomb variant that is not arithmetic: a code number
//! is read with `ue(v)` and then looked up in a table to give
//! `coded_block_pattern`. Four columns, selected by `ChromaArrayType` and by
//! whether the macroblock prediction mode is intra or inter.
//!
//! # Provenance
//!
//! Transcribed from Table 9-4 of ITU-T H.264 clause 9.1.2. A conforming decoder
//! must contain exactly these values in exactly this order — they are the
//! format, not an authorial choice, which is the merger case D9/D15 describe.
//! Nothing here comes from any implementation.
//!
//! # The check that makes transcription safe
//!
//! Each column of Table 9-4 is a **permutation**: every `coded_block_pattern`
//! value appears exactly once, because the mapping has to be invertible for an
//! encoder to exist at all. `tests/spec.rs` asserts that for all four columns
//! and asserts that the inverse tables round-trip, which catches a transposed
//! digit that spot-checking would miss.

/// `ChromaArrayType`, H.264 clause 7.4.2.1.1 — which of Table 9-4's two halves
/// applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChromaArrayType {
    /// Monochrome (`chroma_format_idc == 0`) or separate colour planes.
    /// Table 9-4 columns for `ChromaArrayType` equal to 0 or 3.
    Monochrome,
    /// 4:2:0 or 4:2:2. Table 9-4 columns for `ChromaArrayType` equal to 1 or 2.
    WithChroma,
    /// 4:4:4 with `separate_colour_plane_flag` equal to 0 — shares the
    /// monochrome columns.
    Yuv444,
}

impl ChromaArrayType {
    /// Map `chroma_format_idc` and `separate_colour_plane_flag` onto the variant
    /// that selects the right Table 9-4 column.
    ///
    /// Returns `None` for a `chroma_format_idc` above 3, which clause 7.4.2.1.1
    /// does not define.
    #[must_use]
    pub const fn from_idc(
        chroma_format_idc: u32,
        separate_colour_plane_flag: bool,
    ) -> Option<Self> {
        if separate_colour_plane_flag {
            return Some(Self::Monochrome);
        }
        Some(match chroma_format_idc {
            0 => Self::Monochrome,
            1 | 2 => Self::WithChroma,
            3 => Self::Yuv444,
            _ => return None,
        })
    }

    /// Whether the 48-entry columns apply. The 16-entry columns apply otherwise.
    #[must_use]
    const fn is_48(self) -> bool {
        matches!(self, Self::WithChroma)
    }
}

/// Which pair of Table 9-4 columns applies: intra or inter prediction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MbPartPredMode {
    /// `Intra_4x4` or `Intra_8x8`.
    Intra,
    /// Any inter mode.
    Inter,
}

/// Table 9-4, `ChromaArrayType` 1 or 2. Rows are `codeNum`, columns are
/// `[Intra_4x4 / Intra_8x8, Inter]`.
const CBP_48: [[u8; 2]; 48] = [
    [47, 0],
    [31, 16],
    [15, 1],
    [0, 2],
    [23, 4],
    [27, 8],
    [29, 32],
    [30, 3],
    [7, 5],
    [11, 10],
    [13, 12],
    [14, 15],
    [39, 47],
    [43, 7],
    [45, 11],
    [46, 13],
    [16, 14],
    [3, 6],
    [5, 9],
    [10, 31],
    [12, 35],
    [19, 37],
    [21, 42],
    [26, 44],
    [28, 33],
    [35, 34],
    [37, 36],
    [42, 40],
    [44, 39],
    [1, 43],
    [2, 45],
    [4, 46],
    [8, 17],
    [17, 18],
    [18, 20],
    [20, 24],
    [24, 19],
    [6, 21],
    [9, 26],
    [22, 28],
    [25, 23],
    [32, 27],
    [33, 29],
    [34, 30],
    [36, 22],
    [40, 25],
    [38, 38],
    [41, 41],
];

/// Table 9-4, `ChromaArrayType` 0 or 3. Rows are `codeNum`, columns are
/// `[Intra_4x4 / Intra_8x8, Inter]`.
const CBP_16: [[u8; 2]; 16] = [
    [15, 0],
    [0, 1],
    [7, 2],
    [11, 4],
    [13, 8],
    [14, 3],
    [3, 5],
    [5, 10],
    [10, 12],
    [12, 15],
    [1, 7],
    [2, 11],
    [4, 13],
    [8, 14],
    [6, 6],
    [9, 9],
];

/// `codeNum` → `coded_block_pattern`, Table 9-4.
///
/// Returns `None` when `code_num` is past the end of the applicable column,
/// which is the only way a conforming stream can be told from a malformed one
/// here: `ue(v)` will happily return 1000, and Table 9-4 has no row for it.
#[must_use]
#[inline]
pub fn cbp_from_code_num(
    code_num: u32,
    chroma: ChromaArrayType,
    pred: MbPartPredMode,
) -> Option<u32> {
    let col = usize::from(matches!(pred, MbPartPredMode::Inter));
    let row = usize::try_from(code_num).ok()?;
    let pair = if chroma.is_48() {
        CBP_48.get(row)?
    } else {
        CBP_16.get(row)?
    };
    pair.get(col).map(|&v| u32::from(v))
}

/// `coded_block_pattern` → `codeNum`, the inverse of [`cbp_from_code_num`].
///
/// A linear scan rather than a second table. It runs once per macroblock in an
/// *encoder*, over at most 48 entries that fit in one cache line pair, and a
/// second hand-transcribed table would be a second thing to get wrong. If an
/// encoder profile ever shows this, build the inverse at compile time from the
/// forward table rather than typing it out.
#[must_use]
pub fn code_num_from_cbp(cbp: u32, chroma: ChromaArrayType, pred: MbPartPredMode) -> Option<u32> {
    let col = usize::from(matches!(pred, MbPartPredMode::Inter));
    let cbp = u8::try_from(cbp).ok()?;
    let table: &[[u8; 2]] = if chroma.is_48() { &CBP_48 } else { &CBP_16 };
    table
        .iter()
        .position(|pair| pair.get(col) == Some(&cbp))
        .and_then(|i| u32::try_from(i).ok())
}

/// The number of rows in the applicable Table 9-4 column — 48 or 16.
///
/// The natural `max` argument for a bounded `me(v)` read.
#[must_use]
pub const fn cbp_code_num_count(chroma: ChromaArrayType) -> u32 {
    if chroma.is_48() { 48 } else { 16 }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        reason = "test code over fixed-size tables: an out-of-range index is the failure being looked for"
    )]

    use super::*;

    /// Every column of Table 9-4 must be a permutation, or no encoder could
    /// exist. This is the transcription check.
    fn assert_permutation(table: &[[u8; 2]], col: usize) {
        let n = table.len();
        let mut seen = vec![false; n];
        for pair in table {
            let v = usize::from(pair[col]);
            assert!(v < n, "value {v} out of range for a {n}-entry column");
            assert!(!seen[v], "value {v} appears twice in column {col}");
            seen[v] = true;
        }
        assert!(seen.iter().all(|&b| b), "column {col} is not onto");
    }

    #[test]
    fn table_9_4_columns_are_permutations() {
        assert_permutation(&CBP_48, 0);
        assert_permutation(&CBP_48, 1);
        assert_permutation(&CBP_16, 0);
        assert_permutation(&CBP_16, 1);
    }

    #[test]
    fn forward_and_inverse_agree() {
        for chroma in [
            ChromaArrayType::WithChroma,
            ChromaArrayType::Monochrome,
            ChromaArrayType::Yuv444,
        ] {
            for pred in [MbPartPredMode::Intra, MbPartPredMode::Inter] {
                for code_num in 0..cbp_code_num_count(chroma) {
                    let cbp = cbp_from_code_num(code_num, chroma, pred).unwrap();
                    assert_eq!(code_num_from_cbp(cbp, chroma, pred), Some(code_num));
                }
            }
        }
    }

    #[test]
    fn out_of_range_code_num_is_none() {
        assert_eq!(
            cbp_from_code_num(48, ChromaArrayType::WithChroma, MbPartPredMode::Intra),
            None
        );
        assert_eq!(
            cbp_from_code_num(16, ChromaArrayType::Monochrome, MbPartPredMode::Inter),
            None
        );
        assert_eq!(
            cbp_from_code_num(u32::MAX, ChromaArrayType::WithChroma, MbPartPredMode::Inter),
            None
        );
    }

    #[test]
    fn yuv444_shares_the_monochrome_columns() {
        for code_num in 0..16 {
            assert_eq!(
                cbp_from_code_num(code_num, ChromaArrayType::Yuv444, MbPartPredMode::Intra),
                cbp_from_code_num(code_num, ChromaArrayType::Monochrome, MbPartPredMode::Intra),
            );
        }
    }
}
