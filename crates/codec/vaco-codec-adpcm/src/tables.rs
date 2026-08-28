//! Shared step-size and adaptation tables.
//!
//! `IMA_STEP_TABLE`/`IMA_INDEX_TABLE` are the classic IMA/DVI ADPCM reference
//! tables — the same 89-entry step table and 16-entry index-delta table
//! published in the IMA *Recommended Practices for Enhancing Digital Audio
//! Compatibility* (1992) and reproduced identically across decades of
//! independent, mutually-unrelated implementations (this is the textbook
//! table, not any one codebase's expression of it). `vaco-codec-adpcm`'s
//! IMA-WAV and IMA-QT decoders share it; SWF ADPCM's 2/3/5-bit code widths
//! reuse the same step table with their own narrower index-delta tables
//! (`SWF_INDEX_TABLE_*`), per the *SWF File Format Specification*.
//!
//! `MS_ADAPT_TABLE`/`MS_ADAPT_COEFF1`/`MS_ADAPT_COEFF2` are Microsoft's own
//! published ADPCM coefficient set (the seven "preset" predictor pairs every
//! MS-ADPCM WAV file's `fmt ` chunk enumerates, `WAVEFORMATEX` extension),
//! reproduced the same way — no `provenance/sources.toml` entry names any of
//! these documents today, so no `Vaco-Spec-Ref` is attached.

/// The 89-entry IMA/DVI ADPCM step-size table.
pub(crate) const IMA_STEP_TABLE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408,
    449, 494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066,
    2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630,
    9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794,
    32767,
];

/// The 4-bit-code index-delta table: how far `index` moves after decoding
/// each of the 16 possible nibble values.
pub(crate) const IMA_INDEX_TABLE: [i32; 16] = [
    -1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8,
];

/// SWF ADPCM's own index-delta tables, one per supported code width. The 4-bit
/// row is identical to [`IMA_INDEX_TABLE`]'s positive half by construction —
/// both are the same "double the step every other code" rule at 4 bits.
pub(crate) const SWF_INDEX_TABLE_2BIT: [i32; 4] = [-1, 2, -1, 2];
pub(crate) const SWF_INDEX_TABLE_3BIT: [i32; 8] = [-1, -1, 2, 4, -1, -1, 2, 4];
pub(crate) const SWF_INDEX_TABLE_4BIT: [i32; 16] = IMA_INDEX_TABLE;
pub(crate) const SWF_INDEX_TABLE_5BIT: [i32; 32] = [
    -1, -1, -1, -1, -1, -1, -1, -1, 1, 2, 4, 6, 8, 13, 20, 32, -1, -1, -1, -1, -1, -1, -1, -1, 1,
    2, 4, 6, 8, 13, 20, 32,
];

/// Microsoft ADPCM's per-nibble step-size adaptation multiplier (fixed-point,
/// scaled by 256).
pub(crate) const MS_ADAPT_TABLE: [i32; 16] = [
    230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230,
];

/// The seven built-in predictor coefficient pairs every MS-ADPCM stream's
/// `fmt ` extension enumerates (`coef1`).
pub(crate) const MS_ADAPT_COEFF1: [i32; 7] = [256, 512, 0, 192, 240, 460, 392];
/// `coef2`, paired with [`MS_ADAPT_COEFF1`] by index.
pub(crate) const MS_ADAPT_COEFF2: [i32; 7] = [0, -256, 0, 64, 0, -208, -232];
