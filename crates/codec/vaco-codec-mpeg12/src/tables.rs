//! Constant tables from ITU-T H.262 / ISO/IEC 13818-2 (1995), Annex A and B.
//!
//! `Vaco-Spec-Ref: itu-t-h262` for every table below; see `provenance/
//! vaco-codec-mpeg12.toml` for the per-table clause citation this crate's
//! `provenance-check` gate requires for every table of 32+ elements.
//!
//! # Why VLC codes are strings, not pre-packed integers
//!
//! Every variable-length table here stores its code word as the literal bit
//! string the standard prints (e.g. `"0000 0001 1101"`, spaces kept for
//! readability and stripped by [`bits_of`]), rather than a hand-converted
//! `(code: u16, len: u8)` pair. Annex B's tables run to well over a hundred
//! rows across two DCT-coefficient tables; converting each row to binary by
//! hand is exactly the kind of transcription that is easy to get wrong once
//! and hard to notice, since a single-bit transposition still produces a
//! *valid*, differently-wrong prefix code. Keeping the source string next to
//! the printed table makes a transcription reviewable by eye against the
//! PDF; [`bits_of`] does the binary conversion once, mechanically, instead
//! of by hand per row.

/// Parse a spec-style bit string (`"0000 0001 1101"`, spaces ignored) into
/// `(code, length)`. Used to build every VLC table's `(code, len)` pair from
/// the literal string transcribed from the standard.
#[must_use]
pub fn bits_of(s: &str) -> (u32, u8) {
    let mut code: u32 = 0;
    let mut len: u8 = 0;
    for b in s.bytes() {
        match b {
            b'0' => {
                code <<= 1;
                len += 1;
            }
            b'1' => {
                code = (code << 1) | 1;
                len += 1;
            }
            _ => {}
        }
    }
    (code, len)
}

/// One row of a run/level VLC table (Table B.14/B.15). `run = -1` marks
/// "End of block"; `run = -2` marks "Escape". Otherwise `run >= 0` and
/// `level >= 1` (the sign is a separate bit read after the code, per the
/// tables' own Note 1).
#[derive(Debug, Clone, Copy)]
pub struct RunLevel {
    pub bits: &'static str,
    pub run: i16,
    pub level: i16,
    /// True only for [`TABLE_ZERO`]'s `"1"` row: valid **only** as the first
    /// (non-DC) coefficient of a non-intra block (Note 3). Every other row
    /// (including the `"11"` row for the same `run=0, level=1` pair, Note 4)
    /// is valid everywhere a table lookup happens. A decoder excludes this
    /// row from a normal lookup and instead peeks for it (a lone leading
    /// `1` bit is never a valid prefix of any other row, so this is
    /// unambiguous) before falling back to the ordinary decode — see
    /// `block::decode_coefficients`.
    pub first_coefficient_only: bool,
}

/// Marks "End of block" in a [`RunLevel`] row.
pub const EOB: i16 = -1;
/// Marks "Escape" in a [`RunLevel`] row.
pub const ESCAPE: i16 = -2;

macro_rules! rl {
    ($bits:literal, eob) => {
        RunLevel { bits: $bits, run: EOB, level: 0, first_coefficient_only: false }
    };
    ($bits:literal, escape) => {
        RunLevel { bits: $bits, run: ESCAPE, level: 0, first_coefficient_only: false }
    };
    ($bits:literal, $run:literal, $level:literal) => {
        RunLevel { bits: $bits, run: $run, level: $level, first_coefficient_only: false }
    };
    ($bits:literal, $run:literal, $level:literal, first) => {
        RunLevel { bits: $bits, run: $run, level: $level, first_coefficient_only: true }
    };
}

/// Table B.14 — DCT coefficients Table zero. Used for every non-intra block
/// unconditionally, and for intra blocks when `intra_vlc_format == 0`
/// (Table 7-3). Transcribed mechanically from the spec PDF's own table text
/// (see `provenance/vaco-codec-mpeg12.toml`) and verified prefix-free by a
/// scratch script before being trusted, which caught two real
/// transcription bugs in the `macroblock_type` tables below before they
/// were ever trusted.
pub const TABLE_ZERO: &[RunLevel] = &[
    rl!("10", eob),
    rl!("1", 0, 1, first),
    rl!("11", 0, 1),
    rl!("011", 1, 1),
    rl!("0100", 0, 2),
    rl!("0101", 2, 1),
    rl!("00101", 0, 3),
    rl!("00111", 3, 1),
    rl!("00110", 4, 1),
    rl!("000110", 1, 2),
    rl!("000111", 5, 1),
    rl!("000101", 6, 1),
    rl!("000100", 7, 1),
    rl!("0000110", 0, 4),
    rl!("0000100", 2, 2),
    rl!("0000111", 8, 1),
    rl!("0000101", 9, 1),
    rl!("000001", escape),
    rl!("00100110", 0, 5),
    rl!("00100001", 0, 6),
    rl!("00100101", 1, 3),
    rl!("00100100", 3, 2),
    rl!("00100111", 10, 1),
    rl!("00100011", 11, 1),
    rl!("00100010", 12, 1),
    rl!("00100000", 13, 1),
    rl!("0000001010", 0, 7),
    rl!("0000001100", 1, 4),
    rl!("0000001011", 2, 3),
    rl!("0000001111", 4, 2),
    rl!("0000001001", 5, 2),
    rl!("0000001110", 14, 1),
    rl!("0000001101", 15, 1),
    rl!("0000001000", 16, 1),
    rl!("000000011101", 0, 8),
    rl!("000000011000", 0, 9),
    rl!("000000010011", 0, 10),
    rl!("000000010000", 0, 11),
    rl!("000000011011", 1, 5),
    rl!("000000010100", 2, 4),
    rl!("000000011100", 3, 3),
    rl!("000000010010", 4, 3),
    rl!("000000011110", 6, 2),
    rl!("000000010101", 7, 2),
    rl!("000000010001", 8, 2),
    rl!("000000011111", 17, 1),
    rl!("000000011010", 18, 1),
    rl!("000000011001", 19, 1),
    rl!("000000010111", 20, 1),
    rl!("000000010110", 21, 1),
    rl!("0000000011010", 0, 12),
    rl!("0000000011001", 0, 13),
    rl!("0000000011000", 0, 14),
    rl!("0000000010111", 0, 15),
    rl!("0000000010110", 1, 6),
    rl!("0000000010101", 1, 7),
    rl!("0000000010100", 2, 5),
    rl!("0000000010011", 3, 4),
    rl!("0000000010010", 5, 3),
    rl!("0000000010001", 9, 2),
    rl!("0000000010000", 10, 2),
    rl!("0000000011111", 22, 1),
    rl!("0000000011110", 23, 1),
    rl!("0000000011101", 24, 1),
    rl!("0000000011100", 25, 1),
    rl!("0000000011011", 26, 1),
    rl!("00000000011111", 0, 16),
    rl!("00000000011110", 0, 17),
    rl!("00000000011101", 0, 18),
    rl!("00000000011100", 0, 19),
    rl!("00000000011011", 0, 20),
    rl!("00000000011010", 0, 21),
    rl!("00000000011001", 0, 22),
    rl!("00000000011000", 0, 23),
    rl!("00000000010111", 0, 24),
    rl!("00000000010110", 0, 25),
    rl!("00000000010101", 0, 26),
    rl!("00000000010100", 0, 27),
    rl!("00000000010011", 0, 28),
    rl!("00000000010010", 0, 29),
    rl!("00000000010001", 0, 30),
    rl!("00000000010000", 0, 31),
    rl!("000000000011000", 0, 32),
    rl!("000000000010111", 0, 33),
    rl!("000000000010110", 0, 34),
    rl!("000000000010101", 0, 35),
    rl!("000000000010100", 0, 36),
    rl!("000000000010011", 0, 37),
    rl!("000000000010010", 0, 38),
    rl!("000000000010001", 0, 39),
    rl!("000000000010000", 0, 40),
    rl!("000000000011111", 1, 8),
    rl!("000000000011110", 1, 9),
    rl!("000000000011101", 1, 10),
    rl!("000000000011100", 1, 11),
    rl!("000000000011011", 1, 12),
    rl!("000000000011010", 1, 13),
    rl!("000000000011001", 1, 14),
    rl!("0000000000010011", 1, 15),
    rl!("0000000000010010", 1, 16),
    rl!("0000000000010001", 1, 17),
    rl!("0000000000010000", 1, 18),
    rl!("0000000000010100", 6, 3),
    rl!("0000000000011010", 11, 2),
    rl!("0000000000011001", 12, 2),
    rl!("0000000000011000", 13, 2),
    rl!("0000000000010111", 14, 2),
    rl!("0000000000010110", 15, 2),
    rl!("0000000000010101", 16, 2),
    rl!("0000000000011111", 27, 1),
    rl!("0000000000011110", 28, 1),
    rl!("0000000000011101", 29, 1),
    rl!("0000000000011100", 30, 1),
    rl!("0000000000011011", 31, 1),
];


/// Table B.15 — DCT coefficients Table one. Used only for intra blocks when
/// `intra_vlc_format == 1` (Table 7-3); there is no "first coefficient"
/// special case for this table because it is never used for non-intra
/// blocks (7.2.2.2's modification applies to Table B.14 alone).
pub const TABLE_ONE: &[RunLevel] = &[
    rl!("0110", eob),
    rl!("10", 0, 1),
    rl!("010", 1, 1),
    rl!("110", 0, 2),
    rl!("00101", 2, 1),
    rl!("0111", 0, 3),
    rl!("00111", 3, 1),
    rl!("000110", 4, 1),
    rl!("00110", 1, 2),
    rl!("000111", 5, 1),
    rl!("0000110", 6, 1),
    rl!("0000100", 7, 1),
    rl!("11100", 0, 4),
    rl!("0000111", 2, 2),
    rl!("0000101", 8, 1),
    rl!("1111000", 9, 1),
    rl!("000001", escape),
    rl!("11101", 0, 5),
    rl!("000101", 0, 6),
    rl!("1111001", 1, 3),
    rl!("00100110", 3, 2),
    rl!("1111010", 10, 1),
    rl!("00100001", 11, 1),
    rl!("00100101", 12, 1),
    rl!("00100100", 13, 1),
    rl!("000100", 0, 7),
    rl!("00100111", 1, 4),
    rl!("11111100", 2, 3),
    rl!("11111101", 4, 2),
    rl!("000000100", 5, 2),
    rl!("000000101", 14, 1),
    rl!("000000111", 15, 1),
    rl!("0000001101", 16, 1),
    rl!("1111011", 0, 8),
    rl!("1111100", 0, 9),
    rl!("00100011", 0, 10),
    rl!("00100010", 0, 11),
    rl!("00100000", 1, 5),
    rl!("0000001100", 2, 4),
    rl!("000000011100", 3, 3),
    rl!("000000010010", 4, 3),
    rl!("000000011110", 6, 2),
    rl!("000000010101", 7, 2),
    rl!("000000010001", 8, 2),
    rl!("000000011111", 17, 1),
    rl!("000000011010", 18, 1),
    rl!("000000011001", 19, 1),
    rl!("000000010111", 20, 1),
    rl!("000000010110", 21, 1),
    rl!("11111010", 0, 12),
    rl!("11111011", 0, 13),
    rl!("11111110", 0, 14),
    rl!("11111111", 0, 15),
    rl!("0000000010110", 1, 6),
    rl!("0000000010101", 1, 7),
    rl!("0000000010100", 2, 5),
    rl!("0000000010011", 3, 4),
    rl!("0000000010010", 5, 3),
    rl!("0000000010001", 9, 2),
    rl!("0000000010000", 10, 2),
    rl!("0000000011111", 22, 1),
    rl!("0000000011110", 23, 1),
    rl!("0000000011101", 24, 1),
    rl!("0000000011100", 25, 1),
    rl!("0000000011011", 26, 1),
    rl!("00000000011111", 0, 16),
    rl!("00000000011110", 0, 17),
    rl!("00000000011101", 0, 18),
    rl!("00000000011100", 0, 19),
    rl!("00000000011011", 0, 20),
    rl!("00000000011010", 0, 21),
    rl!("00000000011001", 0, 22),
    rl!("00000000011000", 0, 23),
    rl!("00000000010111", 0, 24),
    rl!("00000000010110", 0, 25),
    rl!("00000000010101", 0, 26),
    rl!("00000000010100", 0, 27),
    rl!("00000000010011", 0, 28),
    rl!("00000000010010", 0, 29),
    rl!("00000000010001", 0, 30),
    rl!("00000000010000", 0, 31),
    rl!("000000000011000", 0, 32),
    rl!("000000000010111", 0, 33),
    rl!("000000000010110", 0, 34),
    rl!("000000000010101", 0, 35),
    rl!("000000000010100", 0, 36),
    rl!("000000000010011", 0, 37),
    rl!("000000000010010", 0, 38),
    rl!("000000000010001", 0, 39),
    rl!("000000000010000", 0, 40),
    rl!("000000000011111", 1, 8),
    rl!("000000000011110", 1, 9),
    rl!("000000000011101", 1, 10),
    rl!("000000000011100", 1, 11),
    rl!("000000000011011", 1, 12),
    rl!("000000000011010", 1, 13),
    rl!("000000000011001", 1, 14),
    rl!("0000000000010011", 1, 15),
    rl!("0000000000010010", 1, 16),
    rl!("0000000000010001", 1, 17),
    rl!("0000000000010000", 1, 18),
    rl!("0000000000010100", 6, 3),
    rl!("0000000000011010", 11, 2),
    rl!("0000000000011001", 12, 2),
    rl!("0000000000011000", 13, 2),
    rl!("0000000000010111", 14, 2),
    rl!("0000000000010110", 15, 2),
    rl!("0000000000010101", 16, 2),
    rl!("0000000000011111", 27, 1),
    rl!("0000000000011110", 28, 1),
    rl!("0000000000011101", 29, 1),
    rl!("0000000000011100", 30, 1),
    rl!("0000000000011011", 31, 1),
];

/// Table B.1 — `macroblock_address_increment`. The last entry (`0`) is the
/// `macroblock_escape` code: on a match the caller adds 33 to the running
/// address and decodes another VLC rather than stopping (6.2.5's `while`
/// loop over `macroblock_escape`).
pub const MACROBLOCK_ADDRESS_INCREMENT: &[(&str, u8)] = &[
    ("1", 1),
    ("011", 2),
    ("010", 3),
    ("0011", 4),
    ("0010", 5),
    ("00011", 6),
    ("00010", 7),
    ("0000111", 8),
    ("0000110", 9),
    ("00001011", 10),
    ("00001010", 11),
    ("00001001", 12),
    ("00001000", 13),
    ("00000111", 14),
    ("00000110", 15),
    ("0000010111", 16),
    ("0000010110", 17),
    ("0000010101", 18),
    ("0000010100", 19),
    ("0000010011", 20),
    ("0000010010", 21),
    ("00000100011", 22),
    ("00000100010", 23),
    ("00000100001", 24),
    ("00000100000", 25),
    ("00000011111", 26),
    ("00000011110", 27),
    ("00000011101", 28),
    ("00000011100", 29),
    ("00000011011", 30),
    ("00000011010", 31),
    ("00000011001", 32),
    ("00000011000", 33),
    ("00000001000", 0), // macroblock_escape: add 33, decode again.
];

/// One row of a `macroblock_type` table (B.2-B.4): the derived flags plus
/// the VLC code, in bitstream field order (`macroblock_quant`,
/// `macroblock_motion_forward`, `macroblock_motion_backward`,
/// `macroblock_pattern`, `macroblock_intra`). Spatial/SNR-scalability
/// columns (Tables B.5-B.8) are out of this crate's scope.
#[derive(Debug, Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is an independently-meaningful derived flag from Tables B.2-B.4 (H.262 §6.3.17.1); a state machine would not reduce the count, only hide it"
)]
pub struct MacroblockType {
    pub bits: &'static str,
    pub quant: bool,
    pub motion_forward: bool,
    pub motion_backward: bool,
    pub pattern: bool,
    pub intra: bool,
}

macro_rules! mbt {
    ($bits:literal, $q:literal, $mf:literal, $mb:literal, $p:literal, $i:literal) => {
        MacroblockType {
            bits: $bits,
            quant: $q,
            motion_forward: $mf,
            motion_backward: $mb,
            pattern: $p,
            intra: $i,
        }
    };
}

/// Table B.2 — `macroblock_type` in I-pictures.
pub const MB_TYPE_I: &[MacroblockType] = &[
    mbt!("1", false, false, false, false, true),
    mbt!("01", true, false, false, false, true),
];

/// Table B.3 — `macroblock_type` in P-pictures.
pub const MB_TYPE_P: &[MacroblockType] = &[
    mbt!("1", false, true, false, true, false),
    mbt!("01", false, false, false, true, false),
    mbt!("001", false, true, false, false, false),
    mbt!("00011", false, false, false, false, true),
    mbt!("00010", true, true, false, true, false),
    mbt!("00001", true, false, false, true, false),
    mbt!("000001", true, false, false, false, true),
];

/// Table B.4 — `macroblock_type` in B-pictures.
pub const MB_TYPE_B: &[MacroblockType] = &[
    mbt!("10", false, true, true, false, false),
    mbt!("11", false, true, true, true, false),
    mbt!("010", false, false, true, false, false),
    mbt!("011", false, false, true, true, false),
    mbt!("0010", false, true, false, false, false),
    mbt!("0011", false, true, false, true, false),
    mbt!("00011", false, false, false, false, true),
    mbt!("00010", true, true, true, true, false),
    mbt!("000011", true, true, false, true, false),
    mbt!("000010", true, false, true, true, false),
    mbt!("000001", true, false, false, false, true),
];

/// Table B.9 — `coded_block_pattern_420`. The VLC decodes directly to the
/// 6-bit `cbp` value (`0` is a note-flagged code "not used with 4:2:0",
/// included anyway since decoding it as literally specified is harmless and
/// simpler than special-casing it away).
pub const CODED_BLOCK_PATTERN: &[(&str, u8)] = &[
    ("111", 60),
    ("1101", 4),
    ("1100", 8),
    ("1011", 16),
    ("1010", 32),
    ("10011", 12),
    ("10010", 48),
    ("10001", 20),
    ("10000", 40),
    ("01111", 28),
    ("01110", 44),
    ("01101", 52),
    ("01100", 56),
    ("01011", 1),
    ("01010", 61),
    ("01001", 2),
    ("01000", 62),
    ("001111", 24),
    ("001110", 36),
    ("001101", 3),
    ("001100", 63),
    ("0010111", 5),
    ("0010110", 9),
    ("0010101", 17),
    ("0010100", 33),
    ("0010011", 6),
    ("0010010", 10),
    ("0010001", 18),
    ("0010000", 34),
    ("00011111", 7),
    ("00011110", 11),
    ("00011101", 19),
    ("00011100", 35),
    ("00011011", 13),
    ("00011010", 49),
    ("00011001", 21),
    ("00011000", 41),
    ("00010111", 14),
    ("00010110", 50),
    ("00010101", 22),
    ("00010100", 42),
    ("00010011", 15),
    ("00010010", 51),
    ("00010001", 23),
    ("00010000", 43),
    ("00001111", 25),
    ("00001110", 37),
    ("00001101", 26),
    ("00001100", 38),
    ("00001011", 29),
    ("00001010", 45),
    ("00001001", 53),
    ("00001000", 57),
    ("00000111", 30),
    ("00000110", 46),
    ("00000101", 54),
    ("00000100", 58),
    ("000000111", 31),
    ("000000110", 47),
    ("000000101", 55),
    ("000000100", 59),
    // Table B.9's last three rows are 9 bits ("0000 0001 1", "0000 0001 0",
    // "0000 0000 1"), one bit shorter than the previous four despite the
    // visual column alignment in the spec's own printed table — a
    // hand-transcription trap this row's own length briefly fell into
    // (10 bits, one zero too many, silently valid VLC-wise since it just
    // shifted three codes one bit later without colliding with anything,
    // so the crate's own prefix-free/value-coverage tests never caught
    // it). Confirmed against a real encoder's bitstream: the 9-bit form
    // is what a real bitstream actually contains at this code.
    ("000000011", 27),
    ("000000010", 39),
    ("000000001", 0),
];

/// Table B.10 — `motion_code`.
pub const MOTION_CODE: &[(&str, i16)] = &[
    ("00000011001", -16),
    ("00000011011", -15),
    ("00000011101", -14),
    ("00000011111", -13),
    ("00000100001", -12),
    ("00000100011", -11),
    ("0000010011", -10),
    ("0000010101", -9),
    ("0000010111", -8),
    ("00000111", -7),
    ("00001001", -6),
    ("00001011", -5),
    ("0000111", -4),
    ("00011", -3),
    ("0011", -2),
    ("011", -1),
    ("1", 0),
    ("010", 1),
    ("0010", 2),
    ("00010", 3),
    ("0000110", 4),
    ("00001010", 5),
    ("00001000", 6),
    ("00000110", 7),
    ("0000010110", 8),
    ("0000010100", 9),
    ("0000010010", 10),
    ("00000100010", 11),
    ("00000100000", 12),
    ("00000011110", 13),
    ("00000011100", 14),
    ("00000011010", 15),
    ("00000011000", 16),
];

/// Table B.11 — `dmvector[t]`. Dual-prime is out of this crate's scope,
/// but the table is kept here since it costs nothing and documents exactly
/// what was skipped.
pub const DMVECTOR: &[(&str, i8)] = &[("11", -1), ("0", 0), ("10", 1)];

/// Table B.12 — `dct_dc_size_luminance`.
pub const DCT_DC_SIZE_LUMA: &[(&str, u8)] = &[
    ("100", 0),
    ("00", 1),
    ("01", 2),
    ("101", 3),
    ("110", 4),
    ("1110", 5),
    ("11110", 6),
    ("111110", 7),
    ("1111110", 8),
    ("11111110", 9),
    ("111111110", 10),
    ("111111111", 11),
];

/// Table B.13 — `dct_dc_size_chrominance`.
pub const DCT_DC_SIZE_CHROMA: &[(&str, u8)] = &[
    ("00", 0),
    ("01", 1),
    ("10", 2),
    ("110", 3),
    ("1110", 4),
    ("11110", 5),
    ("111110", 6),
    ("1111110", 7),
    ("11111110", 8),
    ("111111110", 9),
    ("1111111110", 10),
    ("1111111111", 11),
];

/// Figure 7-2 — `scan[0][v][u]`, the default ("zigzag") inverse scan. Index
/// by `v * 8 + u`; the value is the coefficient's position `n` in the
/// entropy-decoded 1-D array `QFS[n]` (7.3: `QF[v][u] = QFS[scan[v][u]]`).
#[rustfmt::skip]
pub const ZIGZAG_SCAN: [u8; 64] = [
     0,  1,  5,  6, 14, 15, 27, 28,
     2,  4,  7, 13, 16, 26, 29, 42,
     3,  8, 12, 17, 25, 30, 41, 43,
     9, 11, 18, 24, 31, 40, 44, 53,
    10, 19, 23, 32, 39, 45, 52, 54,
    20, 22, 33, 38, 46, 51, 55, 60,
    21, 34, 37, 47, 50, 56, 59, 61,
    35, 36, 48, 49, 57, 58, 62, 63,
];

/// Figure 7-3 — `scan[1][v][u]`, the alternate inverse scan (selected by
/// `alternate_scan` in `picture_coding_extension()`). Same indexing as
/// [`ZIGZAG_SCAN`].
#[rustfmt::skip]
pub const ALTERNATE_SCAN: [u8; 64] = [
     0,  4,  6, 20, 22, 36, 38, 52,
     1,  5,  7, 21, 23, 37, 39, 53,
     2,  8, 19, 24, 34, 40, 50, 54,
     3,  9, 18, 25, 35, 41, 51, 55,
    10, 17, 26, 30, 42, 46, 56, 60,
    11, 16, 27, 31, 43, 47, 57, 61,
    12, 15, 28, 32, 44, 48, 58, 62,
    13, 14, 29, 33, 45, 49, 59, 63,
];

/// §6.2.3.2 default intra weighting matrix, row-major `[v][u]` (already in
/// natural order, not zigzag — the standard prints it as the 2-D matrix
/// directly).
#[rustfmt::skip]
pub const DEFAULT_INTRA_MATRIX: [u8; 64] = [
     8, 16, 19, 22, 26, 27, 29, 34,
    16, 16, 22, 24, 27, 29, 34, 37,
    19, 22, 26, 27, 29, 34, 34, 38,
    22, 22, 26, 27, 29, 34, 37, 40,
    22, 26, 27, 29, 32, 35, 40, 48,
    26, 27, 29, 32, 35, 40, 48, 58,
    26, 27, 29, 34, 38, 46, 56, 69,
    27, 29, 35, 38, 46, 56, 69, 83,
];

/// §6.2.3.2 default non-intra weighting matrix: flat 16 everywhere.
pub const DEFAULT_NON_INTRA_MATRIX: [u8; 64] = [16; 64];

/// Table 7-6 — `quantiser_scale[q_scale_type][quantiser_scale_code]`.
/// Index 0 (`quantiser_scale_code == 0`) is "Forbidden" in the standard and
/// stored as `0` here; a bitstream that reaches it is non-conforming and the
/// caller should treat the resulting zero scale as a decode error rather
/// than a valid quantiser.
pub const QUANTISER_SCALE: [[u8; 32]; 2] = [
    [
        0, 2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 22, 24, 26, 28, 30, 32, 34, 36, 38, 40, 42, 44, 46,
        48, 50, 52, 54, 56, 58, 60, 62,
    ],
    [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 18, 20, 22, 24, 28, 32, 36, 40, 44, 48, 52, 56,
        64, 72, 80, 88, 96, 104, 112,
    ],
];

/// Table 7-4 — `intra_dc_precision` -> `intra_dc_mult`.
#[must_use]
pub const fn intra_dc_mult(precision: u8) -> u16 {
    match precision {
        1 => 4,
        2 => 2,
        3 => 1,
        _ => 8,
    }
}

/// Table 7-2 — `intra_dc_precision` -> the DC predictor reset value.
#[must_use]
pub const fn intra_dc_reset(precision: u8) -> i32 {
    match precision {
        1 => 256,
        2 => 512,
        3 => 1024,
        _ => 128,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every active VLC table must be a prefix-free code: no code word is a
    /// proper prefix of another, or a decoder reading bit by bit could stop
    /// too early. [`RunLevel::first_coefficient_only`] rows are excluded
    /// since they are never active in the same lookup as the ordinary rows
    /// they overlap with (see that field's docs).
    fn assert_prefix_free(mut codes: Vec<String>) {
        codes.sort();
        for w in codes.windows(2) {
            let (Some(a), Some(b)) = (w.first(), w.get(1)) else {
                continue;
            };
            assert!(a != b, "duplicate code {a}");
            assert!(!b.starts_with(a.as_str()), "{a} is a prefix of {b}");
        }
    }

    #[test]
    fn table_zero_is_prefix_free_excluding_first_coefficient_row() {
        let codes = TABLE_ZERO
            .iter()
            .filter(|r| !r.first_coefficient_only)
            .map(|r| r.bits.to_owned())
            .collect();
        assert_prefix_free(codes);
    }

    #[test]
    fn table_one_is_prefix_free() {
        let codes = TABLE_ONE.iter().map(|r| r.bits.to_owned()).collect();
        assert_prefix_free(codes);
    }

    #[test]
    fn macroblock_address_increment_is_prefix_free() {
        let codes = MACROBLOCK_ADDRESS_INCREMENT
            .iter()
            .map(|(b, _)| (*b).to_owned())
            .collect();
        assert_prefix_free(codes);
    }

    #[test]
    fn macroblock_type_tables_are_prefix_free() {
        for table in [MB_TYPE_I, MB_TYPE_P, MB_TYPE_B] {
            let codes = table.iter().map(|r| r.bits.to_owned()).collect();
            assert_prefix_free(codes);
        }
    }

    #[test]
    fn coded_block_pattern_is_prefix_free_and_covers_every_value() {
        let codes = CODED_BLOCK_PATTERN
            .iter()
            .map(|(b, _)| (*b).to_owned())
            .collect();
        assert_prefix_free(codes);
        let mut vals: Vec<u8> = CODED_BLOCK_PATTERN.iter().map(|(_, v)| *v).collect();
        vals.sort_unstable();
        assert_eq!(vals, (0..64).collect::<Vec<u8>>());
    }

    #[test]
    fn coded_block_pattern_shortest_codes_are_exactly_9_bits() {
        // Regression for a real transcription bug: Table B.9's last three
        // rows (cbp 27, 39, 0) are 9 bits, one bit shorter than the four
        // rows just above them in the spec's own printed layout — easy to
        // miscount by one zero since prefix-freedom and 64-value coverage
        // both still hold with an extra leading zero (it just shifts
        // three codes one bit later without colliding with anything), so
        // neither check above would have caught it. A real MPEG-2
        // bitstream using cbp 39 at this exact 9-bit code was what
        // actually exposed the bug: the reader landed one bit early for
        // every macroblock after it.
        for (bits, cbp) in [("000000011", 27u8), ("000000010", 39), ("000000001", 0)] {
            let found = CODED_BLOCK_PATTERN.iter().find(|(_, v)| *v == cbp);
            assert_eq!(found.map(|(b, _)| *b), Some(bits));
        }
    }

    #[test]
    fn motion_code_is_prefix_free_and_symmetric() {
        let codes = MOTION_CODE.iter().map(|(b, _)| (*b).to_owned()).collect();
        assert_prefix_free(codes);
        for v in -16i16..=16 {
            assert!(
                MOTION_CODE.iter().any(|(_, val)| *val == v),
                "missing motion_code value {v}"
            );
        }
    }

    #[test]
    fn dc_size_tables_are_prefix_free() {
        for table in [DCT_DC_SIZE_LUMA, DCT_DC_SIZE_CHROMA] {
            let codes = table.iter().map(|(b, _)| (*b).to_owned()).collect();
            assert_prefix_free(codes);
        }
    }

    #[test]
    fn scans_are_permutations_of_0_to_63() {
        for scan in [ZIGZAG_SCAN, ALTERNATE_SCAN] {
            let mut v = scan.to_vec();
            v.sort_unstable();
            assert_eq!(v, (0..64).collect::<Vec<u8>>());
        }
    }

    #[test]
    fn bits_of_parses_spec_style_strings() {
        assert_eq!(bits_of("101"), (0b101, 3));
        assert_eq!(bits_of("0000 0001 1101"), (0b0000_0001_1101, 12));
    }
}
