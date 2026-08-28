//! VP9 Bitstream & Decoding Process Specification v0.6 — constants, trees,
//! and the format-dictated data tables (transcribed from the primary
//! specification text, not from any existing decoder; see
//! `provenance/vaco-codec-vp9.toml`).
//!
//! Only the tables a **key-frame-only** decoder needs are here: every
//! inter-prediction-only table (`default_inter_mode_probs`,
//! `default_interp_filter_probs`, the MV probability tables,
//! `default_y_mode_probs`/`default_uv_mode_probs`/`default_partition_probs`
//! — the *adaptive* forms used by `inter_frame_mode_info`/
//! `intra_block_mode_info`, as opposed to the fixed `kf_*` forms
//! `intra_frame_mode_info` uses — see §9.3.2's `partition`/`default_intra_mode`
//! rules and §6.3's `compressed_header`, which gates every inter-only
//! forward-update call behind `if (FrameIsIntra == 0)`) is out of scope for
//! C-29/C-30 and is not transcribed here.

// -- Block sizes, RFC-numbered per §3's constants table and the ordering
// -- `mode2txfm_map`'s own comments establish (`// DC`, `// V`, ... in index
// -- order).
pub const BLOCK_4X4: i32 = 0;
pub const BLOCK_4X8: i32 = 1;
pub const BLOCK_8X4: i32 = 2;
pub const BLOCK_8X8: i32 = 3;
pub const BLOCK_8X16: i32 = 4;
pub const BLOCK_16X8: i32 = 5;
pub const BLOCK_16X16: i32 = 6;
pub const BLOCK_16X32: i32 = 7;
pub const BLOCK_32X16: i32 = 8;
pub const BLOCK_32X32: i32 = 9;
pub const BLOCK_32X64: i32 = 10;
pub const BLOCK_64X32: i32 = 11;
pub const BLOCK_64X64: i32 = 12;
pub const BLOCK_INVALID: i32 = 14;

pub const TX_4X4: i32 = 0;
pub const TX_8X8: i32 = 1;
pub const TX_16X16: i32 = 2;
pub const TX_32X32: i32 = 3;

pub const PARTITION_NONE: i32 = 0;
pub const PARTITION_HORZ: i32 = 1;
pub const PARTITION_VERT: i32 = 2;
pub const PARTITION_SPLIT: i32 = 3;

/// The 10 VP9 intra prediction modes, in the index order `mode2txfm_map`'s
/// own comments establish.
pub const DC_PRED: i32 = 0;
pub const V_PRED: i32 = 1;
pub const H_PRED: i32 = 2;
pub const D45_PRED: i32 = 3;
pub const D135_PRED: i32 = 4;
pub const D117_PRED: i32 = 5;
pub const D153_PRED: i32 = 6;
pub const D207_PRED: i32 = 7;
pub const D63_PRED: i32 = 8;
pub const TM_PRED: i32 = 9;

pub const ONLY_4X4: i32 = 0;
pub const ALLOW_32X32: i32 = 3;
pub const TX_MODE_SELECT: i32 = 4;

pub const SEG_LVL_ALT_Q: usize = 0;
pub const SEG_LVL_ALT_L: usize = 1;
pub const SEG_LVL_REF_FRAME: usize = 2;
pub const SEG_LVL_SKIP: usize = 3;
pub const SEG_LVL_MAX: usize = 4;
pub const MAX_SEGMENTS: usize = 8;

/// §6.2.11 — `segmentation_feature_bits[SEG_LVL_MAX]`.
pub const SEGMENTATION_FEATURE_BITS: [u32; SEG_LVL_MAX] = [8, 6, 2, 0];
/// §6.2.11 — `segmentation_feature_signed[SEG_LVL_MAX]`.
pub const SEGMENTATION_FEATURE_SIGNED: [bool; SEG_LVL_MAX] = [true, true, false, false];

pub mod token {
    pub const ZERO_TOKEN: i32 = 0;
    pub const ONE_TOKEN: i32 = 1;
    pub const TWO_TOKEN: i32 = 2;
    pub const THREE_TOKEN: i32 = 3;
    pub const FOUR_TOKEN: i32 = 4;
    pub const DCT_VAL_CATEGORY1: i32 = 5;
    pub const DCT_VAL_CATEGORY2: i32 = 6;
    pub const DCT_VAL_CATEGORY3: i32 = 7;
    pub const DCT_VAL_CATEGORY4: i32 = 8;
    pub const DCT_VAL_CATEGORY5: i32 = 9;
    pub const DCT_VAL_CATEGORY6: i32 = 10;
}

/// §9.3.1 — `token_tree[20]`.
pub const TOKEN_TREE: [i8; 20] = [
    -0, 2, //
    -1, 4, //
    6, 10, //
    -2, 8, //
    -3, -4, //
    12, 14, //
    -5, -6, //
    16, 18, //
    -7, -8, //
    -9, -10,
];

/// §9.3.1 — `partition_tree[6]`, `cols_partition_tree[2]`, `rows_partition_tree[2]`.
pub const PARTITION_TREE: [i8; 6] = [-0, 2, -1, 4, -2, -3];
pub const COLS_PARTITION_TREE: [i8; 2] = [-1, -3];
pub const ROWS_PARTITION_TREE: [i8; 2] = [-2, -3];

/// §9.3.1 — `intra_mode_tree[18]`.
pub const INTRA_MODE_TREE: [i8; 18] = [
    -0, 2, //
    -9, 4, //
    -1, 6, //
    8, 12, //
    -2, 10, //
    -4, -5, //
    -3, 14, //
    -8, 16, //
    -6, -7,
];

/// §9.3.1 — `segment_tree[14]`.
pub const SEGMENT_TREE: [i8; 14] = [2, 4, 6, 8, 10, 12, 0, -1, -2, -3, -4, -5, -6, -7];

/// §9.3.1 — `tx_size_32_tree[6]`, `tx_size_16_tree[4]`, `tx_size_8_tree[2]`.
pub const TX_SIZE_32_TREE: [i8; 6] = [0, 2, -1, 4, -2, -3];
pub const TX_SIZE_16_TREE: [i8; 4] = [0, 2, -1, -2];
pub const TX_SIZE_8_TREE: [i8; 2] = [0, -1];

/// §6.4.26 — `extra_bits[11][3]`: `{cat, num_extra_bits, base_coef}` per token.
pub const EXTRA_BITS: [(usize, u32, i32); 11] = [
    (0, 0, 0),
    (0, 0, 1),
    (0, 0, 2),
    (0, 0, 3),
    (0, 0, 4),
    (1, 1, 5),
    (2, 2, 7),
    (3, 3, 11),
    (4, 4, 19),
    (5, 5, 35),
    (6, 14, 67),
];

/// §6.4.26 — `cat_probs[7][14]` (only the first `numExtra` entries of each
/// row are ever read).
pub const CAT_PROBS: [&[u8]; 7] = [
    &[0],
    &[159],
    &[165, 145],
    &[173, 148, 140],
    &[176, 155, 140, 135],
    &[180, 157, 141, 134, 130],
    &[254, 254, 254, 252, 249, 243, 230, 196, 177, 153, 140, 133, 130, 129],
];

/// §10.2 — `energy_class[12]`, indexed by token.
pub const ENERGY_CLASS: [usize; 12] = [0, 1, 2, 3, 3, 4, 4, 5, 5, 5, 5, 5];

/// §6.4.24 — `coefband_4x4[16]`.
pub const COEFBAND_4X4: [usize; 16] = include!("tables/coefband_4x4.in");
/// §6.4.24 — `coefband_8x8plus[1024]`.
pub const COEFBAND_8X8PLUS: [usize; 1024] = include!("tables/coefband_8x8plus.in");

pub const DEFAULT_SCAN_4X4: [usize; 16] = include!("tables/default_scan_4x4.in");
pub const COL_SCAN_4X4: [usize; 16] = include!("tables/col_scan_4x4.in");
pub const ROW_SCAN_4X4: [usize; 16] = include!("tables/row_scan_4x4.in");
pub const DEFAULT_SCAN_8X8: [usize; 64] = include!("tables/default_scan_8x8.in");
pub const COL_SCAN_8X8: [usize; 64] = include!("tables/col_scan_8x8.in");
pub const ROW_SCAN_8X8: [usize; 64] = include!("tables/row_scan_8x8.in");
pub const DEFAULT_SCAN_16X16: [usize; 256] = include!("tables/default_scan_16x16.in");
pub const COL_SCAN_16X16: [usize; 256] = include!("tables/col_scan_16x16.in");
pub const ROW_SCAN_16X16: [usize; 256] = include!("tables/row_scan_16x16.in");
pub const DEFAULT_SCAN_32X32: [usize; 1024] = include!("tables/default_scan_32x32.in");

/// §8.6.1 — `dc_qlookup[3][256]`, indexed by `[(BitDepth-8)>>1][qindex]`.
pub const DC_QLOOKUP: [[i32; 256]; 3] = include!("tables/dc_qlookup.in");
/// §8.6.1 — `ac_qlookup[3][256]`.
pub const AC_QLOOKUP: [[i32; 256]; 3] = include!("tables/ac_qlookup.in");

/// §10.4 — `kf_partition_probs[16][3]`.
pub const KF_PARTITION_PROBS: [[u8; 3]; 16] = include!("tables/kf_partition_probs.in");
/// §10.4 — `kf_y_mode_probs[10][10][9]`.
pub const KF_Y_MODE_PROBS: [[[u8; 9]; 10]; 10] = include!("tables/kf_y_mode_probs.in");
/// §10.4 — `kf_uv_mode_probs[10][9]`.
pub const KF_UV_MODE_PROBS: [[u8; 9]; 10] = include!("tables/kf_uv_mode_probs.in");

/// §10.5 — `default_skip_prob[3]`.
pub const DEFAULT_SKIP_PROB: [u8; 3] = include!("tables/default_skip_prob.in");
/// §10.5 — `default_tx_probs[4][2][3]` (the three separate `tx_probs_8x8`
/// `/16x16/32x32` tables from §6.3.2, unified into one padded shape indexed
/// by `maxTxSize`; see the module doc on why the padding lines up).
pub const DEFAULT_TX_PROBS: [[[u8; 3]; 2]; 4] = include!("tables/default_tx_probs.in");
/// §10.5 — `default_coef_probs[4][2][2][6][6][3]`.
pub const DEFAULT_COEF_PROBS: [[[[[[u8; 3]; 6]; 6]; 2]; 2]; 4] = include!("tables/default_coef_probs.in");

/// §10.3 — `pareto_table[128][8]`.
pub const PARETO_TABLE: [[u8; 8]; 128] = include!("tables/pareto_table.in");

/// §6.3.5 — `inv_map_table[255]`.
pub const INV_MAP_TABLE: [u8; 255] = include!("tables/inv_map_table.in");

/// §6.4.25's `get_scan` table dispatch (the `TxType` derivation itself is
/// [`crate::decode::get_scan_tx_type`] — this just picks the scan order
/// once that type is known).
#[must_use]
pub fn get_scan(tx_sz: i32, tx_type: vaco_codec_dsp_idct::vp9::TxType) -> &'static [usize] {
    use vaco_codec_dsp_idct::vp9::TxType::{AdstDct, DctAdst};
    if tx_sz == TX_4X4 {
        if tx_type == AdstDct {
            &ROW_SCAN_4X4
        } else if tx_type == DctAdst {
            &COL_SCAN_4X4
        } else {
            &DEFAULT_SCAN_4X4
        }
    } else if tx_sz == TX_8X8 {
        if tx_type == AdstDct {
            &ROW_SCAN_8X8
        } else if tx_type == DctAdst {
            &COL_SCAN_8X8
        } else {
            &DEFAULT_SCAN_8X8
        }
    } else if tx_sz == TX_16X16 {
        if tx_type == AdstDct {
            &ROW_SCAN_16X16
        } else if tx_type == DctAdst {
            &COL_SCAN_16X16
        } else {
            &DEFAULT_SCAN_16X16
        }
    } else {
        &DEFAULT_SCAN_32X32
    }
}

// -- Block-size lookup tables, §6.4/§9.3/§10.2.
pub const B_WIDTH_LOG2_LOOKUP: [u32; 13] = [0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 4, 4];
pub const B_HEIGHT_LOG2_LOOKUP: [u32; 13] = [0, 1, 0, 1, 2, 1, 2, 3, 2, 3, 4, 3, 4];
pub const NUM_4X4_BLOCKS_WIDE_LOOKUP: [usize; 13] = [1, 1, 2, 2, 2, 4, 4, 4, 8, 8, 8, 16, 16];
pub const NUM_4X4_BLOCKS_HIGH_LOOKUP: [usize; 13] = [1, 2, 1, 2, 4, 2, 4, 8, 4, 8, 16, 8, 16];
pub const MI_WIDTH_LOG2_LOOKUP: [u32; 13] = [0, 0, 0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3];
pub const NUM_8X8_BLOCKS_WIDE_LOOKUP: [usize; 13] = [1, 1, 1, 1, 1, 2, 2, 2, 4, 4, 4, 8, 8];
pub const MI_HEIGHT_LOG2_LOOKUP: [u32; 13] = [0, 0, 0, 0, 1, 0, 1, 2, 1, 2, 3, 2, 3];
pub const NUM_8X8_BLOCKS_HIGH_LOOKUP: [usize; 13] = [1, 1, 1, 1, 2, 1, 2, 4, 2, 4, 8, 4, 8];
pub const SIZE_GROUP_LOOKUP: [usize; 13] = [0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3, 3];
pub const MAX_TXSIZE_LOOKUP: [i32; 13] =
    [TX_4X4, TX_4X4, TX_4X4, TX_8X8, TX_8X8, TX_8X8, TX_16X16, TX_16X16, TX_16X16, TX_32X32, TX_32X32, TX_32X32, TX_32X32];
/// §6.2's `tx_mode_to_biggest_tx_size[TX_MODES]`.
pub const TX_MODE_TO_BIGGEST_TX_SIZE: [i32; 5] = [TX_4X4, TX_8X8, TX_16X16, TX_32X32, TX_32X32];

/// §6.4.3/§6.4.23 — `subsize_lookup[PARTITION_TYPES][BLOCK_SIZES]`, with
/// `BLOCK_INVALID` (14) marking a partition/size combination that is
/// illegal (never actually read by a conforming `decode_partition` call).
pub const SUBSIZE_LOOKUP: [[i32; 13]; 4] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
    [14, 14, 14, 2, 14, 14, 5, 14, 14, 8, 14, 14, 11],
    [14, 14, 14, 1, 14, 14, 4, 14, 14, 7, 14, 14, 10],
    [14, 14, 14, 0, 14, 14, 3, 14, 14, 6, 14, 14, 9],
];

/// §6.4.23 — `ss_size_lookup[BLOCK_SIZES][2][2]`, indexed
/// `[bsize][subsampling_x][subsampling_y]`.
pub const SS_SIZE_LOOKUP: [[[i32; 2]; 2]; 13] = [
    [[0, 14], [14, 14]],
    [[1, 0], [14, 14]],
    [[2, 14], [0, 14]],
    [[3, 2], [1, 0]],
    [[4, 3], [14, 1]],
    [[5, 14], [3, 2]],
    [[6, 5], [4, 3]],
    [[7, 6], [14, 4]],
    [[8, 14], [6, 5]],
    [[9, 8], [7, 6]],
    [[10, 9], [14, 7]],
    [[11, 14], [9, 8]],
    [[12, 11], [10, 9]],
];

/// §8.5.1/§6.4.25 — `mode2txfm_map[MB_MODE_COUNT]`, restricted to the 10
/// intra modes (the 4 inter-mode entries are never reached from a key
/// frame's `is_inter == 0` path).
pub const MODE2TXFM_MAP: [vaco_codec_dsp_idct::vp9::TxType; 10] = {
    use vaco_codec_dsp_idct::vp9::TxType::{AdstAdst, AdstDct, DctAdst, DctDct};
    [
        DctDct,  // DC
        AdstDct, // V
        DctAdst, // H
        DctDct,  // D45
        AdstAdst, // D135
        AdstDct, // D117
        DctAdst, // D153
        DctAdst, // D207
        AdstDct, // D63
        AdstAdst, // TM
    ]
};
