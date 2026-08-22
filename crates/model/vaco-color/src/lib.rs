//! Colour signalling per ITU-T H.273.
//!
//! These four properties travel together everywhere — a pixel format alone does
//! not say how to interpret the values — so they are one struct rather than four
//! fields repeated on every frame, stream and filter link.

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
    Iec61966_2_1 = 13,
    Bt2020_10 = 14,
    Bt2020_12 = 15,
    /// PQ.
    Smpte2084 = 16,
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
    Ictcp = 14,
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

impl MatrixCoefficients {
    /// The luma coefficients (Kr, Kb) this matrix implies.
    ///
    /// Derived from the primaries where H.273 says to; returns `None` for
    /// matrices that are not a simple luma projection (`Identity`, `Ictcp`).
    #[must_use]
    pub fn luma_coefficients(self) -> Option<(f64, f64)> {
        todo!("P0-03 freeze: H.273 derivation")
    }
}
