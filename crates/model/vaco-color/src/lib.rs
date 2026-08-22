#![forbid(unsafe_code)]
//! Colour signalling per ITU-T H.273, and the conversion maths it implies.
//!
//! These four properties travel together everywhere — a pixel format alone does
//! not say how to interpret the values — so they are one struct rather than four
//! fields repeated on every frame, stream and filter link.
//!
//! # What is here
//!
//! Two things that are usually kept apart and should not be:
//!
//! 1. **The vocabulary.** [`ColorPrimaries`], [`TransferCharacteristic`],
//!    [`MatrixCoefficients`], [`ColorRange`] and [`ChromaLocation`], with the
//!    specification's own code points as discriminants and both name tables the
//!    reference tool uses (see "Two name tables" below).
//! 2. **The numbers those names stand for.** Primary chromaticities, the
//!    RGB↔XYZ derivation, the R'G'B'↔Y'`CbCr` matrices, the transfer functions
//!    and their inverses, and the quantisation levels for narrow and full range.
//!    This is what `vaco-scale` needs and it belongs next to the enum that names
//!    it, not copied into every consumer.
//!
//! Nothing here allocates and nothing here fails on well-formed input. Every
//! query is either a table index or a handful of floating-point operations.
//!
//! # Coefficients are derived, not tabulated
//!
//! The Y'`CbCr` matrices are computed from the specification's `Kr`/`Kb` constants
//! at the point of use rather than stored pre-multiplied, and the primaries'
//! `Kr`/`Kb` are computed from the chromaticity coordinates when H.273 says to
//! (matrix coefficients 12 and 13). See [`Chromaticity::luma_coefficients`] for
//! the derivation. Values are `f64` throughout and are never pre-rounded — a
//! fixed-point kernel rounds once, at the end, where it knows its own precision.
//!
//! # Two name tables (D17)
//!
//! The reference tool spells several of these values one way on the command line
//! and a different way in `ffprobe` output. `-color_trc gamma22` is accepted and
//! prints back as `bt470m`; `-colorspace rgb` prints back as `gbr`. So
//! [`TransferCharacteristic::name`] (output) and
//! [`TransferCharacteristic::from_name`] (input) are deliberately *not* inverses
//! for every value, and the same holds for [`MatrixCoefficients`]. Each divergence
//! carries a `// D17:` comment where it is defined. Do not "fix" them into one
//! table: D6 makes byte-identical output the contract, and these strings are
//! observable in `-show_streams`.
//!
//! # Unassigned code points
//!
//! H.273 reserves values for future use, and a bitstream may legally carry one.
//! [`ColorPrimaries::from_u8`] and friends return `None` for those rather than
//! inventing a variant. A demuxer that must round-trip the raw byte should keep
//! the byte; this crate models the values the specification has assigned.

mod chroma;
mod matrix;
mod primaries;
mod range;
mod transfer;

#[cfg(test)]
mod tests;

pub use primaries::Chromaticity;
pub use range::{Levels, SUPPORTED_DEPTHS};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ColorInfo {
    pub primaries: ColorPrimaries,
    pub transfer: TransferCharacteristic,
    pub matrix: MatrixCoefficients,
    pub range: ColorRange,
    pub chroma_location: ChromaLocation,
}

/// H.273 Table 2. Discriminants are the specification's own code points, so a
/// bitstream value casts directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
#[repr(u8)]
pub enum ColorPrimaries {
    Bt709 = 1,
    #[default]
    Unspecified = 2,
    Bt470m = 4,
    Bt470bg = 5,
    Smpte170m = 6,
    Smpte240m = 7,
    Film = 8,
    Bt2020 = 9,
    Smpte428 = 10,
    Smpte431 = 11,
    Smpte432 = 12,
    /// EBU Tech 3213-E / JEDEC P22 phosphors.
    ///
    /// Added after the Phase 0 freeze: the reference tool prints `ebu3213` for
    /// this code point in `-show_streams`, so a crate that cannot represent it
    /// cannot reproduce that output (D5/D6). The enum was already
    /// `#[non_exhaustive]`, so this is additive.
    Ebu3213 = 22,
}

/// H.273 Table 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
#[repr(u8)]
pub enum TransferCharacteristic {
    Bt709 = 1,
    #[default]
    Unspecified = 2,
    Gamma22 = 4,
    Gamma28 = 5,
    Smpte170m = 6,
    Smpte240m = 7,
    Linear = 8,
    /// Logarithmic over a 100:1 range.
    Log100 = 9,
    /// Logarithmic over a 100·√10:1 range.
    Log316 = 10,
    /// IEC 61966-2-4 (xvYCC): BT.709's curve extended through negative values.
    Iec61966_2_4 = 11,
    /// BT.1361 extended-colour-gamut system.
    Bt1361e = 12,
    Iec61966_2_1 = 13,
    Bt2020_10 = 14,
    Bt2020_12 = 15,
    /// PQ.
    Smpte2084 = 16,
    /// SMPTE ST 428-1 (D-Cinema).
    Smpte428 = 17,
    /// HLG.
    AribStdB67 = 18,
}

/// H.273 Table 4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
#[repr(u8)]
pub enum MatrixCoefficients {
    Identity = 0,
    Bt709 = 1,
    #[default]
    Unspecified = 2,
    Fcc = 4,
    Bt470bg = 5,
    Smpte170m = 6,
    Smpte240m = 7,
    YCgCo = 8,
    Bt2020Ncl = 9,
    Bt2020Cl = 10,
    /// SMPTE ST 2085 Y'D'zD'x.
    Smpte2085 = 11,
    /// Non-constant luminance, `Kr`/`Kb` derived from the colour primaries.
    ChromaDerivedNcl = 12,
    /// Constant luminance, `Kr`/`Kb` derived from the colour primaries.
    ChromaDerivedCl = 13,
    Ictcp = 14,
    /// IPT-C2.
    IptC2 = 15,
    /// YCgCo-R with an even (2-bit) chroma bit-depth increase.
    YCgCoRe = 16,
    /// YCgCo-R with an odd (1-bit) chroma bit-depth increase.
    YCgCoRo = 17,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorRange {
    #[default]
    Unspecified,
    /// 16-235 for 8-bit luma.
    Limited,
    /// 0-255 for 8-bit.
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChromaLocation {
    #[default]
    Unspecified,
    Left,
    Center,
    TopLeft,
    Top,
    BottomLeft,
    Bottom,
}

impl ColorInfo {
    /// Whether every property carries a value other than "unspecified".
    ///
    /// A caller that needs a complete description of the signal — a colour
    /// converter, a tone mapper — uses this to decide whether to apply its own
    /// defaulting policy. That policy is deliberately not here: it depends on
    /// resolution and container, which this crate knows nothing about.
    #[must_use]
    pub fn is_fully_specified(self) -> bool {
        self.primaries != ColorPrimaries::Unspecified
            && self.transfer != TransferCharacteristic::Unspecified
            && self.matrix != MatrixCoefficients::Unspecified
            && self.range != ColorRange::Unspecified
            && self.chroma_location != ChromaLocation::Unspecified
    }
}
