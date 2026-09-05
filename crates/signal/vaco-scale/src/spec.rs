//! What a conversion is *between*: format, size and colour signalling.
//!
//! # Defaulting `Unspecified`
//!
//! Real files are full of unspecified colour signalling, and a converter cannot
//! refuse to run because of it. The rules here are the reference's observable
//! behaviour, not a preference:
//!
//! | Field | Default |
//! |---|---|
//! | `matrix` | `Bt470bg` (BT.601-625) for `Y'CbCr`; `Identity` is never inferred. |
//! | `range` | `Full` for `R'G'B'`, for gray, and for the `yuvj*` formats; `Limited` otherwise. |
//! | `chroma_location` | no phase shift, which is what the reference applies. |
//!
//! The `yuvj*` rule is load-bearing: those formats *are* full range by
//! definition, and treating one as limited scales every value by 255/219.

use vaco_color::{
    ColorInfo, ColorPrimaries, ColorRange, MatrixCoefficients, TransferCharacteristic,
};
use vaco_pixfmt::{PixFmt, PixFmtFlags};

use crate::colour::Space;

/// One end of a conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageSpec {
    /// Pixel format.
    pub format: PixFmt,
    /// Width in luma samples.
    pub width: u32,
    /// Height in luma samples.
    pub height: u32,
    /// Colour signalling. Any field may be `Unspecified`.
    pub color: ColorInfo,
}

impl ImageSpec {
    /// A spec with default (unspecified) colour signalling.
    #[must_use]
    pub fn new(format: PixFmt, width: u32, height: u32) -> Self {
        Self {
            format,
            width,
            height,
            color: ColorInfo::default(),
        }
    }

    /// Replace the colour signalling.
    #[must_use]
    pub const fn with_color(mut self, color: ColorInfo) -> Self {
        self.color = color;
        self
    }

    /// How this format's channels are interpreted.
    #[must_use]
    pub fn space(&self) -> Space {
        if self.format.has(PixFmtFlags::RGB) {
            Space::Rgb
        } else if self.format.component_count() <= 2 {
            // gray, gray16, ya8: one luma channel plus optional alpha.
            Space::Gray
        } else {
            Space::YCbCr
        }
    }

    /// The range to use, resolving `Unspecified`.
    ///
    /// **Gray is always full range**, whatever the signalling says. That is the
    /// reference's behaviour and it is consistent in every direction: `gray`
    /// into `rgb24` is the identity, `gray` into `yuv420p` compresses to
    /// 16..235, `yuv420p` into `gray` expands out of it, and `-in_range=tv` on a
    /// gray input changes none of it. Honouring the signalling instead would
    /// scale every gray conversion by 255/219.
    #[must_use]
    pub fn effective_range(&self) -> ColorRange {
        if matches!(self.space(), Space::Gray) {
            return ColorRange::Full;
        }
        match self.color.range {
            ColorRange::Limited => ColorRange::Limited,
            ColorRange::Full => ColorRange::Full,
            ColorRange::Unspecified => {
                if matches!(self.space(), Space::Rgb) || is_jpeg_format(self.format) {
                    ColorRange::Full
                } else {
                    ColorRange::Limited
                }
            }
        }
    }

    /// The matrix to use, resolving `Unspecified`.
    #[must_use]
    pub fn effective_matrix(&self) -> MatrixCoefficients {
        match self.color.matrix {
            MatrixCoefficients::Unspecified => MatrixCoefficients::Bt470bg,
            other => other,
        }
    }

    /// The primary set after resolving omitted signalling from the matrix.
    #[must_use]
    pub fn effective_primaries(&self) -> ColorPrimaries {
        if !matches!(self.color.primaries, ColorPrimaries::Unspecified) {
            return self.color.primaries;
        }
        if matches!(self.space(), Space::Rgb)
            && matches!(self.color.matrix, MatrixCoefficients::Unspecified)
        {
            return ColorPrimaries::Bt709;
        }
        match self.effective_matrix() {
            MatrixCoefficients::Bt2020Ncl | MatrixCoefficients::Bt2020Cl => ColorPrimaries::Bt2020,
            MatrixCoefficients::Bt470bg => ColorPrimaries::Bt470bg,
            MatrixCoefficients::Smpte170m => ColorPrimaries::Smpte170m,
            MatrixCoefficients::Smpte240m => ColorPrimaries::Smpte240m,
            _ => ColorPrimaries::Bt709,
        }
    }

    /// The transfer characteristic after resolving omitted signalling from the
    /// resolved primary set.
    #[must_use]
    pub fn effective_transfer(&self) -> TransferCharacteristic {
        if !matches!(self.color.transfer, TransferCharacteristic::Unspecified) {
            return self.color.transfer;
        }
        match self.effective_primaries() {
            ColorPrimaries::Bt470m => TransferCharacteristic::Gamma22,
            ColorPrimaries::Bt470bg => TransferCharacteristic::Gamma28,
            ColorPrimaries::Smpte170m => TransferCharacteristic::Smpte170m,
            ColorPrimaries::Smpte240m => TransferCharacteristic::Smpte240m,
            ColorPrimaries::Bt2020 => TransferCharacteristic::Bt2020_10,
            ColorPrimaries::Smpte428 => TransferCharacteristic::Smpte428,
            _ => TransferCharacteristic::Bt709,
        }
    }

    /// Whether this spec is byte-for-byte the same picture description as
    /// `other`, which is what lets a conversion degrade to a plane copy.
    #[must_use]
    pub fn is_same_picture(&self, other: &Self) -> bool {
        self.format == other.format
            && self.width == other.width
            && self.height == other.height
            && self.effective_range() == other.effective_range()
            && self.effective_matrix() == other.effective_matrix()
            && self.effective_primaries() == other.effective_primaries()
            && self.effective_transfer() == other.effective_transfer()
    }
}

/// The `yuvj*` family, which is full range by definition rather than by
/// signalling.
fn is_jpeg_format(fmt: PixFmt) -> bool {
    fmt.name().starts_with("yuvj")
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::many_single_char_names,
    clippy::float_cmp,
    clippy::integer_division,
    clippy::needless_range_loop,
    clippy::field_reassign_with_default,
    clippy::unreadable_literal,
    clippy::cast_possible_wrap,
    reason = "a failing assertion in a test is a failing test"
)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_formats_default_to_full_range() {
        assert_eq!(
            ImageSpec::new(PixFmt::Yuvj420p, 8, 8).effective_range(),
            ColorRange::Full
        );
        assert_eq!(
            ImageSpec::new(PixFmt::Yuv420p, 8, 8).effective_range(),
            ColorRange::Limited
        );
        assert_eq!(
            ImageSpec::new(PixFmt::Rgb24, 8, 8).effective_range(),
            ColorRange::Full
        );
    }

    #[test]
    fn gray_is_always_full_range() {
        let mut s = ImageSpec::new(PixFmt::Gray8, 8, 8);
        s.color.range = ColorRange::Limited;
        assert_eq!(s.effective_range(), ColorRange::Full);
    }

    #[test]
    fn gray_is_its_own_space() {
        assert_eq!(ImageSpec::new(PixFmt::Gray8, 8, 8).space(), Space::Gray);
        assert_eq!(ImageSpec::new(PixFmt::Ya8, 8, 8).space(), Space::Gray);
        assert_eq!(ImageSpec::new(PixFmt::Yuv420p, 8, 8).space(), Space::YCbCr);
        assert_eq!(ImageSpec::new(PixFmt::Gbrp, 8, 8).space(), Space::Rgb);
    }
}
