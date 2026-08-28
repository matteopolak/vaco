//! Constant tables for exponent grouping, the bit-allocation model and
//! mantissa dequantisation. ATSC A/52:2018 Annex A ("Bit Allocation") gives
//! the bit-allocation tables; §7.3/§7.4 give the exponent and mantissa ones.
//!
//! # Confidence, stated plainly
//!
//! The exponent-group differential coding (`DEXP`) and the mantissa
//! quantizer bit-widths are widely reproduced in independent AC-3 writeups
//! and this crate's own decode of real fixtures cross-checks them (a wrong
//! entry desyncs every mantissa read after it, which the frame-length oracle
//! in `crate::decode` catches). The bit-allocation model's masking-curve
//! constants (`HTH`, `BNDSZ`, the decay/gain/floor tables) are the least
//! independently verifiable part of this crate: they were not transcribed
//! from the standard's own text (unavailable in this environment) but
//! reconstructed from the algorithm's well-documented *structure*. Where the
//! bit-exact values matter and could not be cross-checked, `docs/` and the
//! final report say so rather than implying a confidence this crate cannot
//! back up.

/// `DEXP` grouped differential exponent table: one 7-bit code carries three
/// consecutive delta values, each in `{-2,-1,0,1,2}`, packed as a base-5
/// number (`5^3 = 125 <= 128`). §7.3.
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "base-5 digit extraction from a packed 7-bit code, not a precision loss"
)]
pub const fn dexp(code: u8) -> [i8; 3] {
    let code = code as u16;
    let d0 = (code % 5) as i8 - 2;
    let d1 = ((code / 5) % 5) as i8 - 2;
    let d2 = ((code / 25) % 5) as i8 - 2;
    [d0, d1, d2]
}

/// Coefficients per exponent group, by strategy code (`0` reuse handled by
/// the caller, `1..=3` are D15/D25/D45). §7.3.
#[must_use]
pub const fn group_size(expstr: u8) -> u32 {
    match expstr {
        1 => 1,
        2 => 2,
        _ => 4,
    }
}

/// Full-bandwidth channel count for an `acmod` (excludes LFE). §5.3.2.4.
#[must_use]
pub fn acmod_channel_count(acmod: u8) -> usize {
    vaco_format_ac3::tables::ACMOD_CHANNELS
        .get(usize::from(acmod))
        .copied()
        .unwrap_or(2)
        .into()
}

/// Mantissa quantizer shape for a given `bap` (0..=15). §7.4.4, Table 7.19.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quant {
    /// No mantissa transmitted; value is zero (or dither).
    Zero,
    /// `levels` values packed `per_group` to a `bits`-bit code (bap 1/2/4).
    Grouped {
        levels: u16,
        per_group: u8,
        bits: u8,
    },
    /// One `bits`-bit two's-complement-ish symmetric code per value.
    Uniform { bits: u8 },
}

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
        3 => Quant::Uniform { bits: 3 },
        4 => Quant::Grouped {
            levels: 11,
            per_group: 2,
            bits: 7,
        },
        5 => Quant::Uniform { bits: 4 },
        6 => Quant::Uniform { bits: 5 },
        7 => Quant::Uniform { bits: 6 },
        8 => Quant::Uniform { bits: 7 },
        9 => Quant::Uniform { bits: 8 },
        10 => Quant::Uniform { bits: 9 },
        11 => Quant::Uniform { bits: 10 },
        12 => Quant::Uniform { bits: 11 },
        13 => Quant::Uniform { bits: 12 },
        14 => Quant::Uniform { bits: 14 },
        _ => Quant::Uniform { bits: 16 },
    }
}
