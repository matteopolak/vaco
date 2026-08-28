//! Small helper tables and lookups outside the bit-allocation model itself:
//! channel counts and the mantissa quantizer shape per `bap`. ATSC A/52:2012
//! §7.3/§7.4 and Tables 7.16 through 7.23.
//!
//! Every value here is now checked against the specification text (see
//! `crate::tables_bitalloc`'s module docs for how it was obtained) — this
//! file previously carried a broader disclaimer covering values that turned
//! out, on inspection, to already be right (the bap 1/2/4 grouped-quantizer
//! spacing) and others that were not (bap 3/5's dequantisation, which is a
//! table lookup, not the two's-complement shift this file used to apply to
//! it uniformly across every non-grouped `bap`).

/// Full-bandwidth channel count for an `acmod` (excludes LFE). §5.3.2.4.
#[must_use]
pub fn acmod_channel_count(acmod: u8) -> usize {
    vaco_format_ac3::tables::ACMOD_CHANNELS
        .get(usize::from(acmod))
        .copied()
        .unwrap_or(2)
        .into()
}

/// Mantissa quantizer shape for a given `bap` (0..=15). §7.3.1, Table 7.18.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Quant {
    /// `bap == 0`: no mantissa transmitted; value is zero (or dither).
    Zero,
    /// `levels` values packed `per_group` to a `bits`-bit code (`bap` 1/2/4,
    /// Table 7.19/7.20/7.22 — verified evenly spaced, §7.3.5).
    Grouped {
        levels: u16,
        per_group: u8,
        bits: u8,
    },
    /// One ungrouped code per value, looked up in a small table rather than
    /// computed (`bap` 3/5, Table 7.21/7.23 — *not* evenly spaced the way
    /// the grouped quantizers are).
    SymmetricTable { bits: u8, values: &'static [f32] },
    /// True two's-complement fractional quantization, `bits` wide (`bap`
    /// 6..=15, §7.3.2: "the decimal point is considered to be to the left
    /// of the MSB").
    Asymmetric { bits: u8 },
}

use crate::tables_bitalloc::{BAP3_VALUES, BAP5_VALUES};

/// # Panics
/// Never for `bap <= 15`; out-of-range `bap` cannot occur from a 4-bit field.
#[must_use]
pub const fn quant_for_bap(bap: u8) -> Quant {
    match bap {
        0 => Quant::Zero,
        1 => Quant::Grouped {
            levels: 3,
            per_group: 3,
            bits: 5,
        },
        2 => Quant::Grouped {
            levels: 5,
            per_group: 3,
            bits: 7,
        },
        3 => Quant::SymmetricTable {
            bits: 3,
            values: &BAP3_VALUES,
        },
        4 => Quant::Grouped {
            levels: 11,
            per_group: 2,
            bits: 7,
        },
        5 => Quant::SymmetricTable {
            bits: 4,
            values: &BAP5_VALUES,
        },
        6 => Quant::Asymmetric { bits: 5 },
        7 => Quant::Asymmetric { bits: 6 },
        8 => Quant::Asymmetric { bits: 7 },
        9 => Quant::Asymmetric { bits: 8 },
        10 => Quant::Asymmetric { bits: 9 },
        11 => Quant::Asymmetric { bits: 10 },
        12 => Quant::Asymmetric { bits: 11 },
        13 => Quant::Asymmetric { bits: 12 },
        14 => Quant::Asymmetric { bits: 14 },
        _ => Quant::Asymmetric { bits: 16 },
    }
}
