#![forbid(unsafe_code)]
//! Colour signalling per ITU-T H.273, and the conversion maths it implies.
//!
//! These values travel together everywhere: a pixel format alone does not say
//! how to interpret its samples, so [`ColorInfo`] carries the H.273 vocabulary
//! and the associated conversion parameters as one value.
//!
//! The enums use H.273 code points directly. [`Chromaticity`] supplies primary
//! coordinates; matrix coefficients, transfer functions, inverses, and narrow
//! or full-range levels provide the numbers consumed by `vaco-scale`.
//! Nothing here allocates, and well-formed inputs do not fail.
//!
//! Y'`CbCr` matrices are derived from the specification's `Kr`/`Kb` constants at
//! the point of use. For coefficients 12 and 13, `Kr`/`Kb` come from the primary
//! chromaticities (see [`Chromaticity::luma_coefficients`]); values stay `f64`
//! until a fixed-point consumer rounds at its chosen precision.
//!
//! The reference has separate input and output name tables: for example,
//! `-color_trc gamma22` prints as `bt470m`, and `-colorspace rgb` as `gbr`.
//! [`TransferCharacteristic::name`] and `from_name` (and the corresponding
//! matrix methods) are therefore not inverses. These D17 mappings are observable
//! in `-show_streams` and retain byte-identical output as the contract.
//!
//! H.273-reserved code points are not invented as enum variants:
//! [`ColorPrimaries::from_u8`] and friends return `None`. A demuxer that must
//! round-trip an unassigned byte should retain it separately.

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
