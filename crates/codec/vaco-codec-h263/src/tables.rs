//! VLC and constant tables for H.261 and baseline H.263, each transcribed
//! from the free base-text editions (H.261 03/93, H.263 03/96) with its
//! own clause citation in `provenance/vaco-codec-h263.toml`.

/// Parse a spec-style bit string ("0000 0001", spaces ignored) into
/// `(code, length)`.
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

/// The zig-zag scan shared by H.261 (Figure 12) and H.263 (Figure 13) —
/// mechanically identical to each other and to H.262's own default zigzag.
/// `ZIGZAG_SCAN[n]` is the `(row, col)` position the `n`-th transmitted
/// coefficient belongs at, 0-indexed (the spec figures are 1-indexed).
pub const ZIGZAG_SCAN: [(u8, u8); 64] = [
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
    (4, 0),
    (3, 1),
    (2, 2),
    (1, 3),
    (0, 4),
    (0, 5),
    (1, 4),
    (2, 3),
    (3, 2),
    (4, 1),
    (5, 0),
    (6, 0),
    (5, 1),
    (4, 2),
    (3, 3),
    (2, 4),
    (1, 5),
    (0, 6),
    (0, 7),
    (1, 6),
    (2, 5),
    (3, 4),
    (4, 3),
    (5, 2),
    (6, 1),
    (7, 0),
    (7, 1),
    (6, 2),
    (5, 3),
    (4, 4),
    (3, 5),
    (2, 6),
    (1, 7),
    (2, 7),
    (3, 6),
    (4, 5),
    (5, 4),
    (6, 3),
    (7, 2),
    (7, 3),
    (6, 4),
    (5, 5),
    (4, 6),
    (3, 7),
    (4, 7),
    (5, 6),
    (6, 5),
    (7, 4),
    (7, 5),
    (6, 6),
    (5, 7),
    (6, 7),
    (7, 6),
    (7, 7),
];

// ---------------------------------------------------------------- H.261

/// Table 1/H.261 — VLC for `MBA` (macroblock address, differential). Value
/// 0 is the escape ("add 33, decode again", identical in effect and bit
/// pattern to H.262's own `macroblock_address_increment` escape). The
/// spec's own "MBA stuffing" row (`0000 0001 111`) is not a decodable
/// value; it must be recognized and discarded before this table is tried,
/// which `h261::decode_gob`'s own loop does.
pub const H261_MBA: &[(&str, u8)] = &[
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
    ("00000001000", 0), // escape: add 33, decode again
];

/// MBA stuffing (§4.2.3.1): not a real address, discarded by decoders.
pub const H261_MBA_STUFFING: &str = "00000001111";

/// One row of Table 2/H.261 (`MTYPE`): the derived flags plus the VLC
/// code, bitstream field order. `fil` is H.261's optional loop filter,
/// applicable to motion-compensated macroblocks only.
#[derive(Debug, Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "these six fields are literally Table 2/H.261's own six named columns (Prediction/MC/FIL collapse to `intra`+`mc`+`fil`, plus MQUANT/MVD/CBP) — a state machine or enum would just re-encode the same table row shape with extra indirection, not remove any of the six independent presence bits"
)]
pub(crate) struct H261MbType {
    pub bits: &'static str,
    pub intra: bool,
    pub mc: bool,
    pub fil: bool,
    pub mquant: bool,
    pub mvd: bool,
    pub cbp: bool,
}

macro_rules! h261_mt {
    ($bits:literal, $intra:literal, $mc:literal, $fil:literal, $mquant:literal, $mvd:literal, $cbp:literal) => {
        H261MbType {
            bits: $bits,
            intra: $intra,
            mc: $mc,
            fil: $fil,
            mquant: $mquant,
            mvd: $mvd,
            cbp: $cbp,
        }
    };
}

pub(crate) const H261_MTYPE: &[H261MbType] = &[
    h261_mt!("0001", true, false, false, false, false, false),
    h261_mt!("0000001", true, false, false, true, false, false),
    h261_mt!("1", false, false, false, false, false, true),
    h261_mt!("00001", false, false, false, true, false, true),
    h261_mt!("000000001", false, true, false, false, true, false),
    h261_mt!("00000001", false, true, false, false, true, true),
    h261_mt!("0000000001", false, true, false, true, true, true),
    h261_mt!("001", false, true, true, false, true, false),
    h261_mt!("01", false, true, true, false, true, true),
    h261_mt!("000001", false, true, true, true, true, true),
];

/// Table 3/H.261 — VLC for `MVD` (one component). Each code represents a
/// pair of values 32 apart (`§4.2.3.4`'s own "advantage is taken of the
/// fact that the range... is constrained"); the value stored here is the
/// canonical one in `-16..=15`, and reconstruction (`motion::h261_vector`)
/// applies the same mod-32 range-clamp H.262's own motion vectors use.
pub const H261_MVD: &[(&str, i8)] = &[
    ("1", 0),
    ("010", 1),
    ("011", -1),
    ("0010", 2),
    ("0011", -2),
    ("00010", 3),
    ("00011", -3),
    ("0000110", 4),
    ("0000111", -4),
    ("00001010", 5),
    ("00001011", -5),
    ("00001000", 6),
    ("00001001", -6),
    ("00000110", 7),
    ("00000111", -7),
    ("0000010110", 8),
    ("0000010111", -8),
    ("0000010100", 9),
    ("0000010101", -9),
    ("0000010010", 10),
    ("0000010011", -10),
    ("00000100010", 11),
    ("00000100011", -11),
    ("00000100000", 12),
    ("00000100001", -12),
    ("00000011110", 13),
    ("00000011111", -13),
    ("00000011100", 14),
    ("00000011101", -14),
    ("00000011010", 15),
    ("00000011011", -15),
    ("00000011001", -16),
];

/// Table 4/H.261 — VLC for `CBP` (values 1-63; 0 is never coded, since
/// MTYPE's own `cbp` flag already says whether this element is present).
pub const H261_CBP: &[(&str, u8)] = &[
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
    ("000000011", 27),
    ("000000010", 39),
];

/// One row of H.261's Table 5 (`TCOEFF`): `run`, `level` (magnitude — the
/// sign is a trailing bit read separately), and whether this row is only
/// legal as the very first coefficient of a non-INTRA block (H.261's own
/// analogue of H.262's §7.2.2.2 first-coefficient special case — the note
/// under Table 5 states plainly that this is *why* EOB can be dropped from
/// that variant: "EOB cannot occur as the first coefficient").
#[derive(Debug, Clone, Copy)]
pub(crate) struct H261Coeff {
    pub bits: &'static str,
    pub run: i16,
    pub level: u8,
    pub first_only: bool,
}

/// Marks End-of-Block in an [`H261Coeff`] row.
pub(crate) const H261_EOB: i16 = -1;
/// Marks Escape in an [`H261Coeff`] row.
pub(crate) const H261_ESCAPE: i16 = -2;

macro_rules! h261_c {
    ($bits:literal, eob) => {
        H261Coeff {
            bits: $bits,
            run: H261_EOB,
            level: 0,
            first_only: false,
        }
    };
    ($bits:literal, escape) => {
        H261Coeff {
            bits: $bits,
            run: H261_ESCAPE,
            level: 0,
            first_only: false,
        }
    };
    ($bits:literal, $run:literal, $level:literal) => {
        H261Coeff {
            bits: $bits,
            run: $run,
            level: $level,
            first_only: false,
        }
    };
    ($bits:literal, $run:literal, $level:literal, first) => {
        H261Coeff {
            bits: $bits,
            run: $run,
            level: $level,
            first_only: true,
        }
    };
}

pub(crate) const H261_TCOEFF: &[H261Coeff] = &[
    h261_c!("10", eob),
    h261_c!("1", 0, 1, first),
    h261_c!("11", 0, 1),
    h261_c!("0100", 0, 2),
    h261_c!("00101", 0, 3),
    h261_c!("0000110", 0, 4),
    h261_c!("00100110", 0, 5),
    h261_c!("00100001", 0, 6),
    h261_c!("0000001010", 0, 7),
    h261_c!("000000011101", 0, 8),
    h261_c!("000000011000", 0, 9),
    h261_c!("000000010011", 0, 10),
    h261_c!("000000010000", 0, 11),
    h261_c!("0000000011010", 0, 12),
    h261_c!("0000000011001", 0, 13),
    h261_c!("0000000011000", 0, 14),
    h261_c!("0000000010111", 0, 15),
    h261_c!("011", 1, 1),
    h261_c!("000110", 1, 2),
    h261_c!("00100101", 1, 3),
    h261_c!("0000001100", 1, 4),
    h261_c!("000000011011", 1, 5),
    h261_c!("0000000010110", 1, 6),
    h261_c!("0000000010101", 1, 7),
    h261_c!("0101", 2, 1),
    h261_c!("0000100", 2, 2),
    h261_c!("0000001011", 2, 3),
    h261_c!("000000010100", 2, 4),
    h261_c!("0000000010100", 2, 5),
    h261_c!("00111", 3, 1),
    h261_c!("00100100", 3, 2),
    h261_c!("000000011100", 3, 3),
    h261_c!("0000000010011", 3, 4),
    h261_c!("00110", 4, 1),
    h261_c!("0000001111", 4, 2),
    h261_c!("000000010010", 4, 3),
    h261_c!("000111", 5, 1),
    h261_c!("0000001001", 5, 2),
    h261_c!("0000000010010", 5, 3),
    h261_c!("000101", 6, 1),
    h261_c!("000000011110", 6, 2),
    h261_c!("000100", 7, 1),
    h261_c!("000000010101", 7, 2),
    h261_c!("0000111", 8, 1),
    h261_c!("000000010001", 8, 2),
    h261_c!("0000101", 9, 1),
    h261_c!("0000000010001", 9, 2),
    h261_c!("00100111", 10, 1),
    h261_c!("0000000010000", 10, 2),
    h261_c!("00100011", 11, 1),
    h261_c!("00100010", 12, 1),
    h261_c!("00100000", 13, 1),
    h261_c!("0000001110", 14, 1),
    h261_c!("0000001101", 15, 1),
    h261_c!("0000001000", 16, 1),
    h261_c!("000000011111", 17, 1),
    h261_c!("000000011010", 18, 1),
    h261_c!("000000011001", 19, 1),
    h261_c!("000000010111", 20, 1),
    h261_c!("000000010110", 21, 1),
    h261_c!("0000000011111", 22, 1),
    h261_c!("0000000011110", 23, 1),
    h261_c!("0000000011101", 24, 1),
    h261_c!("0000000011100", 25, 1),
    h261_c!("0000000011011", 26, 1),
    h261_c!("000001", escape),
];

// ---------------------------------------------------------------- H.263

/// Table 4/H.263 — VLC for `MCBPC` in I-pictures. Each entry is
/// `(bits, mb_type, cbpc)`; `mb_type` 3 = INTRA, 4 = INTRA+Q (Table 6's
/// naming), `cbpc` is the two chroma coded-block-pattern bits
/// (`CBPC5<<1 | CBPC6`). `mb_type` 8 ("Stuffing") has no `cbpc` and
/// discards the rest of the macroblock layer.
pub const H263_MCBPC_INTRA: &[(&str, u8, u8)] = &[
    ("1", 3, 0b00),
    ("001", 3, 0b01),
    ("010", 3, 0b10),
    ("011", 3, 0b11),
    ("0001", 4, 0b00),
    ("000001", 4, 0b01),
    ("000010", 4, 0b10),
    ("000011", 4, 0b11),
    ("000000001", 8, 0), // Stuffing
];

/// Table 5/H.263 — VLC for `MCBPC` in P-pictures. `mb_type` 0-4 name
/// INTER/INTER+Q/INTER4V/INTRA/INTRA+Q per Table 6; `mb_type` 20
/// ("Stuffing") has no `cbpc`.
pub const H263_MCBPC_INTER: &[(&str, u8, u8)] = &[
    ("1", 0, 0b00),
    ("0011", 0, 0b01),
    ("0010", 0, 0b10),
    ("000101", 0, 0b11),
    ("011", 1, 0b00),
    ("0000111", 1, 0b01),
    ("0000110", 1, 0b10),
    ("000000101", 1, 0b11),
    ("010", 2, 0b00),
    ("0000101", 2, 0b01),
    ("0000100", 2, 0b10),
    ("00000101", 2, 0b11),
    ("00011", 3, 0b00),
    ("00000100", 3, 0b01),
    ("00000011", 3, 0b10),
    ("0000011", 3, 0b11),
    ("000100", 4, 0b00),
    ("000000100", 4, 0b01),
    ("000000011", 4, 0b10),
    ("000000010", 4, 0b11),
    ("000000001", 20, 0), // Stuffing
];

/// Table 9/H.263 — `DQUANT` (2 bits): index -> differential value added to
/// `QUANT`, clipped to `1..=31`.
pub const H263_DQUANT: [i8; 4] = [-1, -2, 1, 2];

/// Table 10/H.263, INTRA column: the four luma coded-block-pattern bits
/// (`CBPY1..4`, blocks 1-4). Table 10's own INTER column is each row's
/// bitwise complement (verified in [`H263_CBPY_INTER`]'s own values,
/// transcribed from the spec table's split "(12)"/"(34)" sub-columns
/// directly rather than by inverting — kept as two explicit tables so a
/// future reader can check either one against the spec without doing the
/// inversion by hand).
pub const H263_CBPY_INTRA: &[(&str, u8)] = &[
    ("0011", 0b0000),
    ("00101", 0b0001),
    ("00100", 0b0010),
    ("1001", 0b0011),
    ("00011", 0b0100),
    ("0111", 0b0101),
    ("000010", 0b0110),
    ("1011", 0b0111),
    ("00010", 0b1000),
    ("000011", 0b1001),
    ("0101", 0b1010),
    ("1010", 0b1011),
    ("0100", 0b1100),
    ("1000", 0b1101),
    ("0110", 0b1110),
    ("11", 0b1111),
];

/// Table 10/H.263, INTER column — same codes as [`H263_CBPY_INTRA`], each
/// mapped to the bitwise complement of the INTRA value at the same index
/// (Table 10 states the INTER pattern is the INTRA one inverted).
pub const H263_CBPY_INTER: &[(&str, u8)] = &[
    ("0011", 0b1111),
    ("00101", 0b1110),
    ("00100", 0b1101),
    ("1001", 0b1100),
    ("00011", 0b1011),
    ("0111", 0b1010),
    ("000010", 0b1001),
    ("1011", 0b1000),
    ("00010", 0b0111),
    ("000011", 0b0110),
    ("0101", 0b0101),
    ("1010", 0b0100),
    ("0100", 0b0011),
    ("1000", 0b0010),
    ("0110", 0b0001),
    ("11", 0b0000),
];

/// Table 11/H.263 — VLC for `MVD` (one component). The value stored is
/// the vector difference in half-pel units (table index `n` corresponds to
/// half-pel value `n - 32`); reconstruction wraps mod 64 the same way
/// H.261's own `MVD` table wraps mod 32 — see `motion::h263_vector`.
pub const H263_MVD: &[(&str, i8)] = &[
    ("0000000000101", -32),
    ("0000000000111", -31),
    ("000000000101", -30),
    ("000000000111", -29),
    ("000000001001", -28),
    ("000000001011", -27),
    ("000000001101", -26),
    ("000000001111", -25),
    ("00000001001", -24),
    ("00000001011", -23),
    ("00000001101", -22),
    ("00000001111", -21),
    ("00000010001", -20),
    ("00000010011", -19),
    ("00000010101", -18),
    ("00000010111", -17),
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
    ("00000010110", 17),
    ("00000010100", 18),
    ("00000010010", 19),
    ("00000010000", 20),
    ("00000001110", 21),
    ("00000001100", 22),
    ("00000001010", 23),
    ("00000001000", 24),
    ("000000001110", 25),
    ("000000001100", 26),
    ("000000001010", 27),
    ("000000001000", 28),
    ("000000000110", 29),
    ("000000000100", 30),
    ("0000000000110", 31),
];

/// One row of Table 13/H.263 (`TCOEF`): `last` (this is the final non-zero
/// coefficient in the block), `run`, `level` (magnitude; sign is the
/// trailing bit). The escape code (index 102, `0000011`, followed by a
/// separate 1-bit `LAST` + 6-bit `RUN` + 8-bit `LEVEL`) is
/// [`H263_TCOEF_ESCAPE`], not a row here.
#[derive(Debug, Clone, Copy)]
pub(crate) struct H263Coeff {
    pub bits: &'static str,
    pub last: bool,
    pub run: u8,
    pub level: u8,
}

macro_rules! h263_c {
    ($bits:literal, $last:literal, $run:literal, $level:literal) => {
        H263Coeff {
            bits: $bits,
            last: $last,
            run: $run,
            level: $level,
        }
    };
}

/// Table 13/H.263's escape code (index 102).
pub(crate) const H263_TCOEF_ESCAPE: &str = "0000011";

pub(crate) const H263_TCOEF: &[H263Coeff] = &[
    h263_c!("10", false, 0, 1),
    h263_c!("1111", false, 0, 2),
    h263_c!("010101", false, 0, 3),
    h263_c!("0010111", false, 0, 4),
    h263_c!("00011111", false, 0, 5),
    h263_c!("000100101", false, 0, 6),
    h263_c!("000100100", false, 0, 7),
    h263_c!("0000100001", false, 0, 8),
    h263_c!("0000100000", false, 0, 9),
    h263_c!("00000000111", false, 0, 10),
    h263_c!("00000000110", false, 0, 11),
    h263_c!("00000100000", false, 0, 12),
    h263_c!("110", false, 1, 1),
    h263_c!("010100", false, 1, 2),
    h263_c!("00011110", false, 1, 3),
    h263_c!("0000001111", false, 1, 4),
    h263_c!("00000100001", false, 1, 5),
    h263_c!("000001010000", false, 1, 6),
    h263_c!("1110", false, 2, 1),
    h263_c!("00011101", false, 2, 2),
    h263_c!("0000001110", false, 2, 3),
    h263_c!("000001010001", false, 2, 4),
    h263_c!("01101", false, 3, 1),
    h263_c!("000100011", false, 3, 2),
    h263_c!("0000001101", false, 3, 3),
    h263_c!("01100", false, 4, 1),
    h263_c!("000100010", false, 4, 2),
    h263_c!("000001010010", false, 4, 3),
    h263_c!("01011", false, 5, 1),
    h263_c!("0000001100", false, 5, 2),
    h263_c!("000001010011", false, 5, 3),
    h263_c!("010011", false, 6, 1),
    h263_c!("0000001011", false, 6, 2),
    h263_c!("000001010100", false, 6, 3),
    h263_c!("010010", false, 7, 1),
    h263_c!("0000001010", false, 7, 2),
    h263_c!("010001", false, 8, 1),
    h263_c!("0000001001", false, 8, 2),
    h263_c!("010000", false, 9, 1),
    h263_c!("0000001000", false, 9, 2),
    h263_c!("0010110", false, 10, 1),
    h263_c!("000001010101", false, 10, 2),
    h263_c!("0010101", false, 11, 1),
    h263_c!("0010100", false, 12, 1),
    h263_c!("00011100", false, 13, 1),
    h263_c!("00011011", false, 14, 1),
    h263_c!("000100001", false, 15, 1),
    h263_c!("000100000", false, 16, 1),
    h263_c!("000011111", false, 17, 1),
    h263_c!("000011110", false, 18, 1),
    h263_c!("000011101", false, 19, 1),
    h263_c!("000011100", false, 20, 1),
    h263_c!("000011011", false, 21, 1),
    h263_c!("000011010", false, 22, 1),
    h263_c!("00000100010", false, 23, 1),
    h263_c!("00000100011", false, 24, 1),
    h263_c!("000001010110", false, 25, 1),
    h263_c!("000001010111", false, 26, 1),
    h263_c!("0111", true, 0, 1),
    h263_c!("000011001", true, 0, 2),
    h263_c!("00000000101", true, 0, 3),
    h263_c!("001111", true, 1, 1),
    h263_c!("00000000100", true, 1, 2),
    h263_c!("001110", true, 2, 1),
    h263_c!("001101", true, 3, 1),
    h263_c!("001100", true, 4, 1),
    h263_c!("0010011", true, 5, 1),
    h263_c!("0010010", true, 6, 1),
    h263_c!("0010001", true, 7, 1),
    h263_c!("0010000", true, 8, 1),
    h263_c!("00011010", true, 9, 1),
    h263_c!("00011001", true, 10, 1),
    h263_c!("00011000", true, 11, 1),
    h263_c!("00010111", true, 12, 1),
    h263_c!("00010110", true, 13, 1),
    h263_c!("00010101", true, 14, 1),
    h263_c!("00010100", true, 15, 1),
    h263_c!("00010011", true, 16, 1),
    h263_c!("000011000", true, 17, 1),
    h263_c!("000010111", true, 18, 1),
    h263_c!("000010110", true, 19, 1),
    h263_c!("000010101", true, 20, 1),
    h263_c!("000010100", true, 21, 1),
    h263_c!("000010011", true, 22, 1),
    h263_c!("000010010", true, 23, 1),
    h263_c!("000010001", true, 24, 1),
    h263_c!("0000000111", true, 25, 1),
    h263_c!("0000000110", true, 26, 1),
    h263_c!("0000000101", true, 27, 1),
    h263_c!("0000000100", true, 28, 1),
    h263_c!("00000100100", true, 29, 1),
    h263_c!("00000100101", true, 30, 1),
    h263_c!("00000100110", true, 31, 1),
    h263_c!("00000100111", true, 32, 1),
    h263_c!("000001011000", true, 33, 1),
    h263_c!("000001011001", true, 34, 1),
    h263_c!("000001011010", true, 35, 1),
    h263_c!("000001011011", true, 36, 1),
    h263_c!("000001011100", true, 37, 1),
    h263_c!("000001011101", true, 38, 1),
    h263_c!("000001011110", true, 39, 1),
    h263_c!("000001011111", true, 40, 1),
];

// ------------------------------------------------------------ Annex I

/// Table I.1/H.263 — VLC for `INTRA_MODE` (Annex I, Advanced INTRA
/// Coding): which spatial prediction a macroblock's INTRA blocks use.
/// `0` = DC only, `1` = vertical (predict from the block above), `2` =
/// horizontal (predict from the block to the left).
pub const H263_INTRA_MODE: &[(&str, u8)] = &[("0", 0), ("10", 1), ("11", 2)];

/// Table I.2/H.263 — VLC for INTRA `TCOEF` under Advanced INTRA Coding.
/// `Vaco-Spec-Ref: itu-t-h263` Annex I, Table I.2: "the VLC codeword
/// entries used in Table I.2 are the same as those used in the normal
/// TCOEF table (Table 16)... but with a different interpretation of
/// LEVEL and RUN (without altering LAST)" — confirmed directly, not
/// assumed: this table's own 102 codewords are exactly [`H263_TCOEF`]'s
/// 102 codewords, set-for-set (checked by this module's own tests), so
/// a transcription slip producing an extra/missing/altered codeword
/// would break that invariant, not just look slightly off. The escape
/// code (index 102) is the same `"0000011"` as [`H263_TCOEF_ESCAPE`] —
/// Annex I redefines what a decoded (RUN, LEVEL) pair *means*, not how
/// an out-of-table one is escaped.
#[allow(
    dead_code,
    reason = "landed ahead of its consumer: Annex I's reconstruction (mode 0/1/2 prediction, oddification) and macroblock-layer INTRA_MODE dispatch are not wired yet -- this table and H263_INTRA_MODE are the two verified transcription pieces, confirmed against the primary text and, via the set-equality test above, self-consistent with the already-shipped H263_TCOEF"
)]
pub(crate) const H263_INTRA_TCOEF: &[H263Coeff] = &[
    h263_c!("10", false, 0, 1),
    h263_c!("1111", false, 1, 1),
    h263_c!("010101", false, 3, 1),
    h263_c!("0010111", false, 5, 1),
    h263_c!("00011111", false, 7, 1),
    h263_c!("000100101", false, 8, 1),
    h263_c!("000100100", false, 9, 1),
    h263_c!("0000100001", false, 10, 1),
    h263_c!("0000100000", false, 11, 1),
    h263_c!("00000000111", false, 4, 3),
    h263_c!("00000000110", false, 9, 2),
    h263_c!("00000100000", false, 13, 1),
    h263_c!("110", false, 0, 2),
    h263_c!("010100", false, 1, 2),
    h263_c!("00011110", false, 1, 4),
    h263_c!("0000001111", false, 1, 5),
    h263_c!("00000100001", false, 1, 6),
    h263_c!("000001010000", false, 1, 7),
    h263_c!("1110", false, 0, 3),
    h263_c!("00011101", false, 3, 2),
    h263_c!("0000001110", false, 2, 3),
    h263_c!("000001010001", false, 3, 4),
    h263_c!("01101", false, 0, 5),
    h263_c!("000100011", false, 4, 2),
    h263_c!("0000001101", false, 3, 3),
    h263_c!("01100", false, 0, 4),
    h263_c!("000100010", false, 5, 2),
    h263_c!("000001010010", false, 5, 3),
    h263_c!("01011", false, 2, 1),
    h263_c!("0000001100", false, 6, 2),
    h263_c!("000001010011", false, 0, 25),
    h263_c!("010011", false, 4, 1),
    h263_c!("0000001011", false, 7, 2),
    h263_c!("000001010100", false, 0, 24),
    h263_c!("010010", false, 0, 8),
    h263_c!("0000001010", false, 8, 2),
    h263_c!("010001", false, 0, 7),
    h263_c!("0000001001", false, 2, 4),
    h263_c!("010000", false, 0, 6),
    h263_c!("0000001000", false, 12, 1),
    h263_c!("0010110", false, 0, 9),
    h263_c!("000001010101", false, 0, 23),
    h263_c!("0010101", false, 2, 2),
    h263_c!("0010100", false, 1, 3),
    h263_c!("00011100", false, 6, 1),
    h263_c!("00011011", false, 0, 10),
    h263_c!("000100001", false, 0, 12),
    h263_c!("000100000", false, 0, 11),
    h263_c!("000011111", false, 0, 18),
    h263_c!("000011110", false, 0, 17),
    h263_c!("000011101", false, 0, 16),
    h263_c!("000011100", false, 0, 15),
    h263_c!("000011011", false, 0, 14),
    h263_c!("000011010", false, 0, 13),
    h263_c!("00000100010", false, 0, 20),
    h263_c!("00000100011", false, 0, 19),
    h263_c!("000001010110", false, 0, 22),
    h263_c!("000001010111", false, 0, 21),
    h263_c!("0111", true, 0, 1),
    h263_c!("000011001", true, 14, 1),
    h263_c!("00000000101", true, 20, 1),
    h263_c!("001111", true, 1, 1),
    h263_c!("00000000100", true, 19, 1),
    h263_c!("001110", true, 2, 1),
    h263_c!("001101", true, 3, 1),
    h263_c!("001100", true, 0, 2),
    h263_c!("0010011", true, 5, 1),
    h263_c!("0010010", true, 6, 1),
    h263_c!("0010001", true, 4, 1),
    h263_c!("0010000", true, 0, 3),
    h263_c!("00011010", true, 9, 1),
    h263_c!("00011001", true, 10, 1),
    h263_c!("00011000", true, 11, 1),
    h263_c!("00010111", true, 12, 1),
    h263_c!("00010110", true, 13, 1),
    h263_c!("00010101", true, 8, 1),
    h263_c!("00010100", true, 7, 1),
    h263_c!("00010011", true, 0, 4),
    h263_c!("000011000", true, 17, 1),
    h263_c!("000010111", true, 18, 1),
    h263_c!("000010110", true, 16, 1),
    h263_c!("000010101", true, 15, 1),
    h263_c!("000010100", true, 2, 2),
    h263_c!("000010011", true, 1, 2),
    h263_c!("000010010", true, 0, 6),
    h263_c!("000010001", true, 0, 5),
    h263_c!("0000000111", true, 4, 2),
    h263_c!("0000000110", true, 3, 2),
    h263_c!("0000000101", true, 1, 3),
    h263_c!("0000000100", true, 0, 7),
    h263_c!("00000100100", true, 2, 3),
    h263_c!("00000100101", true, 1, 4),
    h263_c!("00000100110", true, 0, 9),
    h263_c!("00000100111", true, 0, 8),
    h263_c!("000001011000", true, 21, 1),
    h263_c!("000001011001", true, 22, 1),
    h263_c!("000001011010", true, 23, 1),
    h263_c!("000001011011", true, 7, 2),
    h263_c!("000001011100", true, 6, 2),
    h263_c!("000001011101", true, 5, 2),
    h263_c!("000001011110", true, 3, 3),
    h263_c!("000001011111", true, 0, 10),
];
#[cfg(test)]
mod annex_i_tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn intra_tcoef_has_102_rows_matching_the_spec_table() {
        assert_eq!(H263_INTRA_TCOEF.len(), 102);
    }

    #[test]
    fn intra_tcoef_uses_exactly_h263_tcoefs_own_codewords() {
        // `Vaco-Spec-Ref: itu-t-h263` Annex I, Table I.2's own text:
        // "the VLC codeword entries used in Table I.2 are the same as
        // those used in the normal TCOEF table (Table 16)... but with a
        // different interpretation of LEVEL and RUN". Checked directly:
        // this is a set-equality invariant a transcription slip (an
        // extra/missing/mistyped codeword) would almost certainly break,
        // not a shape or plausibility check.
        let baseline: BTreeSet<&str> = H263_TCOEF.iter().map(|c| c.bits).collect();
        let intra: BTreeSet<&str> = H263_INTRA_TCOEF.iter().map(|c| c.bits).collect();
        assert_eq!(baseline.len(), 102);
        assert_eq!(intra.len(), 102);
        assert_eq!(baseline, intra);
    }

    #[test]
    fn intra_tcoef_is_prefix_free() {
        let codes: Vec<&str> = H263_INTRA_TCOEF.iter().map(|c| c.bits).collect();
        for (i, a) in codes.iter().enumerate() {
            for b in codes.iter().skip(i + 1) {
                assert!(
                    !a.starts_with(*b) && !b.starts_with(*a),
                    "{a} and {b} are not prefix-free"
                );
            }
        }
    }

    #[test]
    fn intra_tcoef_spot_checks_against_the_primary_text() {
        // A handful of rows read directly off Table I.2, independent of
        // the set-equality check above (which would not catch two
        // codewords' RUN/LEVEL payloads being swapped with each other).
        let get = |bits: &str| H263_INTRA_TCOEF.iter().find(|c| c.bits == bits).copied();
        assert!(matches!(get("10"), Some(c) if !c.last && c.run == 0 && c.level == 1));
        assert!(matches!(get("1111"), Some(c) if !c.last && c.run == 1 && c.level == 1));
        assert!(matches!(get("0111"), Some(c) if c.last && c.run == 0 && c.level == 1));
        // Index 101: LAST=1, RUN=0, LEVEL=10, the highest-index non-escape row.
        assert!(matches!(get("000001011111"), Some(c) if c.last && c.run == 0 && c.level == 10));
    }

    #[test]
    fn intra_mode_vlc_matches_table_i1() {
        assert_eq!(H263_INTRA_MODE, &[("0", 0), ("10", 1), ("11", 2)]);
    }
}
