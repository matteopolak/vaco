//! Resolving an [`Rgba`] colour into a destination [`PixFmt`]'s own native
//! code values.
//!
//! RGB-family formats (`PixFmtFlags::RGB`) take R/G/B/A directly, scaled from
//! 8-bit to the format's own depth. YUV-family formats go through
//! [`MatrixCoefficients::rgb_to_ycbcr_with`] and [`ColorRange`]'s
//! quantisation levels — both already own the numbers; this module only
//! decides which matrix and calls them, the same division of labour
//! `vaco-scale::colour` documents for the full-frame conversion case.
//!
//! # The default matrix, measured
//!
//! `ffmpeg -f lavfi -i color=c=red:s=2x2:d=1 -pix_fmt yuv420p -f rawvideo -`
//! on an unspecified-colourspace source produces `Y=0x51 Cb=0x5a Cr=0xf0`
//! (limited range), which is exactly BT.601/`smpte170m`'s prediction for
//! pure red (`Y'=0.299`, `Cb'=-0.1687`, quantised) and not BT.709's
//! (`Y'=0.2126`, `Cb'=-0.1146`, which would print `Y=0x3f Cb=0x66`). So
//! [`MatrixCoefficients::Unspecified`] resolves to `Smpte170m` here, matching
//! the reference's own default for unspecified-colourspace content.

use vaco_color::{ColorInfo, ColorRange, MatrixCoefficients};
use vaco_core::{Error, Result};
use vaco_pixfmt::{PixFmt, PixFmtFlags};

use crate::color::Rgba;

/// A colour resolved to one code value per logical channel (`0..=3`,
/// aligned with [`vaco_pixfmt::PixFmtDescriptor::components`]'s own
/// ordering), ready to write with [`crate::sample::write`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Solid {
    pub channel: [u32; 4],
}

#[allow(
    clippy::integer_division,
    reason = "8-bit-to-depth rescale with an explicit +127 rounding term, not a truncating division"
)]
fn scale_to_depth(v8: u8, depth: u8) -> u32 {
    let max = max_code(depth);
    (u32::from(v8) * max + 127) / 255
}

const fn max_code(depth: u8) -> u32 {
    if depth == 0 {
        0
    } else if depth >= 32 {
        u32::MAX
    } else {
        (1u32 << depth) - 1
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "e is a normalised 0..1 or -0.5..0.5 fraction and levels bound the result to a u32-representable code value"
)]
fn quantise(e: f64, offset: u32, scale: u32, min: u32, max: u32) -> u32 {
    let code = f64::from(offset) + f64::from(scale) * e;
    (code.round() as i64).clamp(i64::from(min), i64::from(max)) as u32
}

impl Solid {
    /// Resolve `color` into `fmt`'s native code values, using `color_info`
    /// to pick the RGB→YUV matrix and quantisation range for a non-RGB
    /// format. RGB-family formats ignore `color_info` entirely.
    ///
    /// # Errors
    /// [`Error::Unsupported`] for palette, bitstream-packed, hardware-surface
    /// or floating-point formats, for XYZ formats, and for the handful of
    /// [`MatrixCoefficients`] variants that are not a linear matrix on R'G'B'
    /// (constant-luminance, `Ictcp`, `IptC2`, the reversible YCgCo-R
    /// variants) — none of this crate's callers need those today.
    pub fn resolve(color: Rgba, fmt: PixFmt, color_info: ColorInfo) -> Result<Self> {
        let desc = fmt.descriptor();
        if desc
            .flags
            .intersects(PixFmtFlags::PALETTE | PixFmtFlags::BITSTREAM | PixFmtFlags::HW_ACCEL | PixFmtFlags::FLOAT)
        {
            return Err(Error::Unsupported("vaco-filter-draw: palette/bitstream/hw/float pixel format"));
        }
        if desc.flags.contains(PixFmtFlags::XYZ) {
            return Err(Error::Unsupported("vaco-filter-draw: XYZ pixel formats"));
        }

        let mut channel = [0u32; 4];
        let depth_of = |i: usize| desc.components.get(i).map_or(0, |c| c.depth);

        if desc.flags.contains(PixFmtFlags::RGB) {
            channel[0] = scale_to_depth(color.r, depth_of(0));
            channel[1] = scale_to_depth(color.g, depth_of(1));
            channel[2] = scale_to_depth(color.b, depth_of(2));
            if desc.flags.contains(PixFmtFlags::ALPHA) {
                channel[3] = scale_to_depth(color.a, depth_of(3));
            }
            return Ok(Self { channel });
        }

        // YUV family.
        let matrix = match color_info.matrix {
            MatrixCoefficients::Unspecified => MatrixCoefficients::Smpte170m,
            m => m,
        };
        let coeffs = matrix
            .rgb_to_ycbcr_with(color_info.primaries)
            .ok_or(Error::Unsupported("vaco-filter-draw: matrix has no linear R'G'B'->Y'CbCr form"))?;
        let red = f64::from(color.r) / 255.0;
        let green = f64::from(color.g) / 255.0;
        let blue = f64::from(color.b) / 255.0;
        let luma_e = coeffs[0][0] * red + coeffs[0][1] * green + coeffs[0][2] * blue;
        let cb = coeffs[1][0] * red + coeffs[1][1] * green + coeffs[1][2] * blue;
        let cr = coeffs[2][0] * red + coeffs[2][1] * green + coeffs[2][2] * blue;

        // Measured: `ffmpeg -f lavfi -i color=c=red -pix_fmt gray -f rawvideo -`
        // prints `0x4c` (76 = round(255*0.299), BT.601 *full* range) while the
        // same source through `yuv420p` prints `0x51` (81 = BT.601 *limited*
        // range) — a single-component "gray" format defaults to full range
        // even when a multi-component YUV format defaults to limited, so the
        // component count decides the default here, not a single rule for
        // every non-RGB format.
        let range = match color_info.range {
            ColorRange::Unspecified if desc.components.len() == 1 => ColorRange::Full,
            ColorRange::Unspecified => ColorRange::Limited,
            r => r,
        };
        let y_depth = depth_of(0);
        let luma = range
            .luma_levels(u32::from(y_depth))
            .ok_or(Error::Unsupported("vaco-filter-draw: unsupported luma bit depth"))?;
        channel[0] = quantise(luma_e, luma.offset, luma.scale, luma.min, luma.max);

        // A single-component format (`gray8`, `gray16le`, ...) has no Cb/Cr
        // plane to fill, so it never asks `ColorRange` for a chroma
        // quantisation at all — asking anyway would fail on such formats'
        // component depth of 0, for a channel this crate would never write.
        if desc.components.len() > 1 {
            let c_depth = depth_of(1).max(depth_of(2));
            let chroma = range
                .chroma_levels(u32::from(c_depth))
                .ok_or(Error::Unsupported("vaco-filter-draw: unsupported chroma bit depth"))?;
            channel[1] = quantise(cb, chroma.offset, chroma.scale, chroma.min, chroma.max);
            channel[2] = quantise(cr, chroma.offset, chroma.scale, chroma.min, chroma.max);
        }
        if desc.flags.contains(PixFmtFlags::ALPHA) {
            channel[3] = scale_to_depth(color.a, depth_of(3));
        }
        Ok(Self { channel })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;

    fn red() -> Rgba {
        Rgba { r: 255, g: 0, b: 0, a: 255 }
    }

    #[test]
    fn red_on_yuv420p_matches_the_measured_bt601_limited_range_values() {
        let info = ColorInfo::default(); // everything Unspecified
        let solid = Solid::resolve(red(), PixFmt::Yuv420p, info).unwrap();
        assert_eq!(solid.channel[0], 0x51);
        assert_eq!(solid.channel[1], 0x5a);
        assert_eq!(solid.channel[2], 0xf0);
    }

    #[test]
    fn red_on_gbrp_is_the_identity_scale() {
        let info = ColorInfo::default();
        let solid = Solid::resolve(red(), PixFmt::Gbrp, info).unwrap();
        assert_eq!(solid.channel[0], 255);
        assert_eq!(solid.channel[1], 0);
        assert_eq!(solid.channel[2], 0);
    }

    #[test]
    fn ten_bit_rgb_scales_into_the_wider_range() {
        let info = ColorInfo::default();
        let solid = Solid::resolve(red(), PixFmt::Gbrp10le, info).unwrap();
        assert_eq!(solid.channel[0], 1023);
        assert_eq!(solid.channel[1], 0);
    }

    #[test]
    fn full_range_black_is_the_neutral_chroma_point() {
        let info = ColorInfo {
            range: ColorRange::Full,
            matrix: MatrixCoefficients::Bt709,
            ..ColorInfo::default()
        };
        let black = Rgba { r: 0, g: 0, b: 0, a: 255 };
        let solid = Solid::resolve(black, PixFmt::Yuv444p, info).unwrap();
        assert_eq!(solid.channel[0], 0);
        assert_eq!(solid.channel[1], 128);
        assert_eq!(solid.channel[2], 128);
    }

    #[test]
    fn alpha_formats_carry_the_fourth_channel() {
        let info = ColorInfo::default();
        let translucent = Rgba { r: 255, g: 0, b: 0, a: 128 };
        let solid = Solid::resolve(translucent, PixFmt::Yuva420p, info).unwrap();
        assert_eq!(solid.channel[3], 128);
    }

    #[test]
    fn palette_formats_are_rejected() {
        let info = ColorInfo::default();
        assert!(Solid::resolve(red(), PixFmt::Pal8, info).is_err());
    }
}
