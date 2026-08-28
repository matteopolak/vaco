//! Every fixed table RFC 6386 defines: trees, default/update probabilities,
//! dequantisation lookups, and the sub-pixel interpolation filter taps.
//!
//! Format-dictated constants, not expression (D7/D15's merger-doctrine
//! carve-out) — transcribed from the primary specification text
//! (`rfc-6386`), never from an existing decoder. Every table below cites the
//! RFC section it came from; where the RFC states two contradictory things
//! about one field (`segment_feature_mode`'s polarity — see
//! [`crate::header`]), the resolution is recorded at the call site, not
//! here.

#![allow(clippy::unreadable_literal, reason = "spec tables, not derived numbers")]

/// Intra 16x16/chroma prediction modes plus `B_PRED`, RFC 6386 §8.2.
pub const DC_PRED: i32 = 0;
pub const V_PRED: i32 = 1;
pub const H_PRED: i32 = 2;
pub const TM_PRED: i32 = 3;
pub const B_PRED: i32 = 4;

/// Inter prediction modes, RFC 6386 §16.2 (offset by `num_ymodes = 5`).
pub const MV_NEARESTMV: i32 = 5;
pub const MV_NEARMV: i32 = 6;
pub const MV_ZEROMV: i32 = 7;
pub const MV_NEWMV: i32 = 8;
pub const MV_SPLITMV: i32 = 9;

/// The ten 4x4 luma subblock ("B_PRED") modes, RFC 6386 §11.2/§12.3.
pub const B_DC_PRED: i32 = 0;
pub const B_TM_PRED: i32 = 1;
pub const B_VE_PRED: i32 = 2;
pub const B_HE_PRED: i32 = 3;
pub const B_LD_PRED: i32 = 4;
pub const B_RD_PRED: i32 = 5;
pub const B_VR_PRED: i32 = 6;
pub const B_VL_PRED: i32 = 7;
pub const B_HD_PRED: i32 = 8;
pub const B_HU_PRED: i32 = 9;

/// RFC 6386 §10 — segment id tree (0..3), default probs 255/255/255.
pub const MB_SEGMENT_TREE: [i8; 6] = [2, 4, 0, -1, -2, -3];

/// RFC 6386 §11.2 — key-frame 16x16 luma mode tree (`B_PRED` is the "0" leaf).
pub const KF_YMODE_TREE: [i8; 8] = [-4, 2, 4, 6, 0, -1, -2, -3];
/// RFC 6386 §11.2 — fixed (never updated) probabilities for [`KF_YMODE_TREE`].
pub const KF_YMODE_PROB: [u8; 4] = [145, 156, 163, 128];

/// RFC 6386 §16.1 — non-key-frame 16x16 luma mode tree (`DC_PRED` is "0").
pub const YMODE_TREE: [i8; 8] = [0, 2, 4, 6, -1, -2, -3, -4];
/// RFC 6386 §16.1 — default probabilities for [`YMODE_TREE`]; frame-header
/// updatable (§9.10), reset to this default on every key frame.
pub const YMODE_PROB_DEFAULT: [u8; 4] = [112, 86, 140, 37];

/// RFC 6386 §11.4/§16.1 — chroma mode tree, shared by key frames and
/// interframes.
pub const UV_MODE_TREE: [i8; 6] = [0, 2, -1, 4, -2, -3];
/// RFC 6386 §11.4 — fixed key-frame chroma mode probabilities.
pub const KF_UV_MODE_PROB: [u8; 3] = [142, 114, 183];
/// RFC 6386 §16.1 — default interframe chroma mode probabilities; updatable,
/// reset on every key frame.
pub const UV_MODE_PROB_DEFAULT: [u8; 3] = [162, 101, 204];

/// RFC 6386 §11.2 — the ten-way B_PRED subblock mode tree, shared by key
/// frames (contextual probabilities, [`KF_BMODE_PROB`]) and interframes
/// (flat probabilities, [`BMODE_PROB`]).
pub const BMODE_TREE: [i8; 18] = [
    0, 2, //
    -1, 4, //
    -2, 6, //
    8, 12, //
    -3, 10, //
    -5, -6, //
    -4, 14, //
    -7, 16, //
    -8, -9,
];

/// RFC 6386 §16.1 — flat, non-contextual B_PRED submode probabilities used
/// for intra-coded macroblocks *within an interframe*.
pub const BMODE_PROB: [u8; 9] = [120, 90, 79, 133, 87, 85, 80, 111, 151];

/// RFC 6386 §11.5 — `kf_bmode_prob[above_mode][left_mode][tree_node]`, used
/// only for key-frame B_PRED subblocks. Outer/middle indices follow the
/// `B_*_PRED` constants above (`B_DC_PRED = 0` .. `B_HU_PRED = 9`).
pub const KF_BMODE_PROB: [[[u8; 9]; 10]; 10] = include!("tables/kf_bmode_prob.in");

/// RFC 6386 §16.2 — `mv_ref_tree`, leaves relative to [`MV_NEARESTMV`].
pub const MV_REF_TREE: [i8; 8] = [-2, 2, -0, 4, -1, 6, -3, -4];

/// RFC 6386 §16.3 — `vp8_mode_contexts[6][4]`, indexed by neighbour weight
/// count (0..5) per column (zero/nearest/near/splitmv).
pub const VP8_MODE_CONTEXTS: [[u8; 4]; 6] = [
    [7, 1, 1, 143],
    [14, 18, 14, 107],
    [135, 64, 57, 68],
    [60, 56, 128, 65],
    [159, 134, 128, 34],
    [234, 188, 128, 28],
];

/// RFC 6386 §16.4 — `mvpartition_tree` (leaves: top/bottom, left/right,
/// quarters, 16-way, in that enum order 0..3).
pub const MVPARTITION_TREE: [i8; 6] = [-3, 2, -2, 4, -0, -1];
/// RFC 6386 §16.4 — fixed partition-layout probabilities.
pub const MVPARTITION_PROB: [u8; 3] = [110, 111, 150];
/// RFC 6386 §16.4 — subblock index (0..15, raster order) to partition index,
/// one row per layout (top/bottom, left/right, quarters, 16-way).
pub const MV_PARTITIONS: [[u8; 16]; 4] = [
    [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1],
    [0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1],
    [0, 0, 1, 1, 0, 0, 1, 1, 2, 2, 3, 3, 2, 2, 3, 3],
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
];
/// Number of distinct partitions in each [`MV_PARTITIONS`] row.
pub const MV_PARTITION_COUNTS: [usize; 4] = [2, 2, 4, 16];

/// RFC 6386 §16.4 — `sub_mv_ref_tree` (`LEFT4X4=0, ABOVE4X4=1, ZERO4X4=2, NEW4X4=3`).
pub const SUB_MV_REF_TREE: [i8; 6] = [-0, 2, -1, 4, -2, -3];
/// RFC 6386 §16.4 — `sub_mv_ref_prob[context][3]`, context from
/// `vp8_mv_cont` (0..4: normal, left-zero, above-zero, left==above, both zero).
pub const SUB_MV_REF_PROB: [[u8; 3]; 5] = [
    [147, 136, 18],
    [106, 145, 1],
    [179, 121, 1],
    [223, 1, 34],
    [208, 1, 1],
];

/// RFC 6386 §17.1 — `small_mvtree`, the short-form (0..7) MV magnitude tree.
pub const SMALL_MVTREE: [i8; 14] = [2, 8, 4, 6, -0, -1, -2, -3, 10, 12, -4, -5, -6, -7];

/// Layout of the 19-entry per-component MV probability array, RFC 6386 §17.1.
pub const MVP_IS_SHORT: usize = 0;
pub const MVP_SIGN: usize = 1;
pub const MVP_SHORT: usize = 2; // 7 entries: 2..9
pub const MVP_BITS: usize = 9; // 10 entries: 9..19

/// RFC 6386 §17.2 — `default_mv_context[2]` (row, then column). Reset to
/// this on every key frame.
pub const DEFAULT_MV_CONTEXT: [[u8; 19]; 2] = [
    [
        162, 128, 225, 146, 172, 147, 214, 39, 156, 128, 129, 132, 75, 145, 178, 206, 239, 254,
        254,
    ],
    [
        164, 128, 204, 170, 119, 235, 140, 230, 228, 128, 130, 130, 74, 148, 180, 203, 236, 254,
        254,
    ],
];

/// RFC 6386 §17.2 — `vp8_mv_update_probs[2]`, fixed, never themselves updated.
pub const MV_UPDATE_PROBS: [[u8; 19]; 2] = [
    [
        237, 246, 253, 253, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 250, 250, 252, 254,
        254,
    ],
    [
        231, 243, 245, 253, 254, 254, 254, 254, 254, 254, 254, 254, 254, 254, 251, 251, 254, 254,
        254,
    ],
];

/// RFC 6386 §13.2 — the 12-value DCT token alphabet.
pub mod token {
    pub const DCT_0: i32 = 0;
    pub const DCT_1: i32 = 1;
    pub const DCT_2: i32 = 2;
    pub const DCT_3: i32 = 3;
    pub const DCT_4: i32 = 4;
    pub const DCT_CAT1: i32 = 5;
    pub const DCT_CAT2: i32 = 6;
    pub const DCT_CAT3: i32 = 7;
    pub const DCT_CAT4: i32 = 8;
    pub const DCT_CAT5: i32 = 9;
    pub const DCT_CAT6: i32 = 10;
    pub const DCT_EOB: i32 = 11;
}

/// RFC 6386 §13.2 — `coeff_tree`. Entered at index 2 (skipping the
/// EOB-vs-rest branch) whenever the previous token in the same block was
/// `DCT_0`, since `dct_eob` can never follow a `DCT_0` (see
/// [`vaco_codec_msac::read_tree_at`]).
pub const COEFF_TREE: [i8; 22] = [
    -11, 2, //
    0, 4, //
    -1, 6, //
    8, 12, //
    -2, 10, //
    -3, -4, //
    14, 16, //
    -5, -6, //
    18, 20, //
    -7, -8, //
    -9, -10,
];

/// RFC 6386 §13.2 — extra-bit probabilities and base magnitude per category
/// token (`dct_cat1`..`dct_cat6`).
pub const CATEGORY_BASE: [i32; 6] = [5, 7, 11, 19, 35, 67];
pub const PCAT1: [u8; 1] = [159];
pub const PCAT2: [u8; 2] = [165, 145];
pub const PCAT3: [u8; 3] = [173, 148, 140];
pub const PCAT4: [u8; 4] = [176, 155, 140, 135];
pub const PCAT5: [u8; 5] = [180, 157, 141, 134, 130];
pub const PCAT6: [u8; 11] = [254, 254, 243, 230, 196, 177, 153, 140, 133, 130, 129];

/// RFC 6386 §20.16's `zigzag[16]` (a pure numeric table; format-dictated,
/// not expression) — scan position to raster position within a 4x4 block.
pub const ZIGZAG: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];

/// RFC 6386 §13.3 — `coeff_bands[16]`, scan position to one of 8 bands.
pub const COEFF_BANDS: [usize; 16] = [0, 1, 2, 3, 6, 4, 5, 6, 6, 6, 6, 6, 6, 6, 6, 7];

/// RFC 6386 §13.3's block-type index into [`DEFAULT_COEFF_PROBS`] /
/// [`COEFF_UPDATE_PROBS`]'s outermost dimension.
pub const PLANE_Y_AFTER_Y2: usize = 0;
pub const PLANE_Y2: usize = 1;
pub const PLANE_UV: usize = 2;
pub const PLANE_Y_NO_Y2: usize = 3;

/// RFC 6386 §13.5 — `default_coeff_probs[4][8][3][11]`.
pub const DEFAULT_COEFF_PROBS: [[[[u8; 11]; 3]; 8]; 4] = include!("tables/default_coeff_probs.in");

/// RFC 6386 §13.4 — `coeff_update_probs[4][8][3][11]`.
pub const COEFF_UPDATE_PROBS: [[[[u8; 11]; 3]; 8]; 4] = include!("tables/coeff_update_probs.in");

/// RFC 6386 §14.1 / §20.3 `dequant_data.h` — `dc_q_lookup[128]`.
pub const DC_QLOOKUP: [i16; 128] = [
    4, 5, 6, 7, 8, 9, 10, 10, 11, 12, 13, 14, 15, 16, 17, 17, 18, 19, 20, 20, 21, 21, 22, 22, 23,
    23, 24, 25, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 37, 38, 39, 40, 41, 42, 43,
    44, 45, 46, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65,
    66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87,
    88, 89, 91, 93, 95, 96, 98, 100, 101, 102, 104, 106, 108, 110, 112, 114, 116, 118, 122, 124,
    126, 128, 130, 132, 134, 136, 138, 140, 143, 145, 148, 151, 154, 157,
];

/// RFC 6386 §14.1 / §20.3 `dequant_data.h` — `ac_q_lookup[128]`.
pub const AC_QLOOKUP: [i16; 128] = [
    4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28,
    29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51,
    52, 53, 54, 55, 56, 57, 58, 60, 62, 64, 66, 68, 70, 72, 74, 76, 78, 80, 82, 84, 86, 88, 90,
    92, 94, 96, 98, 100, 102, 104, 106, 108, 110, 112, 114, 116, 119, 122, 125, 128, 131, 134,
    137, 140, 143, 146, 149, 152, 155, 158, 161, 164, 167, 170, 173, 177, 181, 185, 189, 193, 197,
    201, 205, 209, 213, 217, 221, 225, 229, 234, 239, 245, 249, 254, 259, 264, 269, 274, 279, 284,
];

/// RFC 6386 §14.4 — the two fixed-point constants `short_idct4x4llm` uses.
pub const COSPI8_SQRT2_MINUS1: i32 = 20091;
pub const SINPI8_SQRT2: i32 = 35468;

/// RFC 6386 §18.3 — `filters` (6-tap bicubic), indexed by 1/8-pel phase 0..7.
pub const SIXTAP_FILTERS: [[i32; 6]; 8] = [
    [0, 0, 128, 0, 0, 0],
    [0, -6, 123, 12, -1, 0],
    [2, -11, 108, 36, -8, 1],
    [0, -9, 93, 50, -6, 0],
    [3, -16, 77, 77, -16, 3],
    [0, -6, 50, 93, -9, 0],
    [1, -8, 36, 108, -11, 2],
    [0, -1, 12, 123, -6, 0],
];

/// RFC 6386 §18.3 — `BilinearFilters`, indexed by 1/8-pel phase 0..7,
/// expressed as a 6-tap array (only taps 2/3 nonzero) for interface
/// uniformity with [`SIXTAP_FILTERS`].
pub const BILINEAR_FILTERS: [[i32; 6]; 8] = [
    [0, 0, 128, 0, 0, 0],
    [0, 0, 112, 16, 0, 0],
    [0, 0, 96, 32, 0, 0],
    [0, 0, 80, 48, 0, 0],
    [0, 0, 64, 64, 0, 0],
    [0, 0, 48, 80, 0, 0],
    [0, 0, 32, 96, 0, 0],
    [0, 0, 16, 112, 0, 0],
];

/// RFC 6386 §16.3/§18.1/§20.11 — the one-macroblock (16px = 128 in 1/8-pel
/// units) motion-vector clamp margin. The RFC states the margin only
/// symbolically (`LEFT_TOP_MARGIN`/`RIGHT_BOTTOM_MARGIN`); this numeric
/// value is derived from the reference decoder's `modemv.c` bound
/// computation and cross-checked against `predict.c`'s independent
/// `BORDER_PIXELS = 16` constant (both are pure numeric constants, not
/// algorithmic expression).
pub const MV_BORDER_EIGHTH_PEL: i32 = 128;
/// The reconstruction border width in full pixels these margins imply.
pub const BORDER_PIXELS: usize = 16;
