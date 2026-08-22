//! [`ColorRange`] and the quantisation it selects.

use crate::ColorRange;

/// How a normalised component maps to integer code values at a given bit depth.
///
/// The contract is one line:
///
/// ```text
///   code = clamp(round(offset + scale · E), min, max)
/// ```
///
/// where `E` is `0..=1` for luma and R'G'B', and `-0.5..=0.5` for `Cb`/`Cr`. Both
/// `offset` and `scale` are exact integers at every depth, which is what lets a
/// fixed-point kernel fold them into its own coefficients without a rounding
/// step of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Levels {
    /// The code value `E = 0` maps to: black for luma, the neutral point for
    /// chroma.
    pub offset: u32,
    /// The number of code values one whole unit of `E` spans.
    pub scale: u32,
    /// Lowest representable code value. Always 0.
    pub min: u32,
    /// Highest representable code value. Always `2^depth − 1`.
    pub max: u32,
}

/// Bit depths [`ColorRange::luma_levels`] and [`ColorRange::chroma_levels`]
/// answer for.
///
/// The lower bound is H.273's: it defines the quantisation for `BitDepth` ≥ 8 and
/// nothing below. The upper bound is where `scale` stops fitting in a `u32`
/// (`(2^n − 1)` at n = 32 is exactly `u32::MAX`), which is far past any real
/// pixel format — the deepest integer format in `vaco-pixfmt` is 16.
pub const SUPPORTED_DEPTHS: std::ops::RangeInclusive<u32> = 8..=32;

impl ColorRange {
    /// Every variant, in code-point order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[Self::Unspecified, Self::Limited, Self::Full]
    }

    /// The reference tool's `AVColorRange` code point.
    ///
    /// H.273 itself has no such enumeration — it carries a one-bit
    /// `video_full_range_flag`. The 0/1/2 numbering is the reference's and is
    /// what appears in its option table, so it is what a CLI-compatible tool has
    /// to speak. See [`Self::from_full_range_flag`] for the bitstream form.
    #[inline]
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        match self {
            Self::Unspecified => 0,
            Self::Limited => 1,
            Self::Full => 2,
        }
    }

    /// The variant for a reference `AVColorRange` code point.
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Unspecified,
            1 => Self::Limited,
            2 => Self::Full,
            _ => return None,
        })
    }

    /// The H.273 / H.264 / H.265 `video_full_range_flag`.
    ///
    /// The flag is one bit and cannot express "unspecified": a bitstream that
    /// carries VUI colour description always says one or the other. Absence of
    /// the VUI is what maps to [`Self::Unspecified`], and that is the caller's
    /// to detect.
    #[must_use]
    pub const fn from_full_range_flag(full: bool) -> Self {
        if full { Self::Full } else { Self::Limited }
    }

    /// The name the reference tool prints as `color_range` in
    /// `ffprobe -show_streams`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            // D17: prints "unknown", not "unspecified".
            Self::Unspecified => "unknown",
            // `tv` and `pc` rather than `limited`/`full`: those are aliases in
            // the option table but not the printed spelling.
            Self::Limited => "tv",
            Self::Full => "pc",
        }
    }

    /// Parse a name as the reference's `-color_range` option accepts it.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "unknown" | "unspecified" => Self::Unspecified,
            "tv" | "mpeg" | "limited" => Self::Limited,
            "pc" | "jpeg" | "full" => Self::Full,
            _ => return None,
        })
    }

    /// Quantisation for a luma or R'G'B' component at `bit_depth`.
    ///
    /// [`Self::Unspecified`] is treated as [`Self::Limited`]: that is the
    /// defaulting every decoder applies to a stream that does not say, and
    /// returning `None` here would only push the same decision onto every
    /// caller. `None` means the depth is outside [`SUPPORTED_DEPTHS`].
    ///
    /// # Derivation
    ///
    /// H.273 §8.3, with `n` the bit depth:
    ///
    /// ```text
    ///   narrow: Y' = Clip1( Round( (219·E'Y + 16) · 2^(n−8) ) )
    ///   full:   Y' = Clip1( Round( (2^n − 1)·E'Y ) )
    /// ```
    ///
    /// Note that `Clip1` is to the whole `0..2^n − 1` range in both cases, not
    /// to 16..235: the footroom and headroom of a narrow-range signal are legal
    /// code values that carry real picture information, and clipping them away
    /// at the quantiser is a bug that shows up as clipped specular highlights.
    #[must_use]
    pub const fn luma_levels(self, bit_depth: u32) -> Option<Levels> {
        let Some((min, max, shift)) = bounds(bit_depth) else {
            return None;
        };
        Some(match self {
            Self::Full => Levels {
                offset: 0,
                scale: max,
                min,
                max,
            },
            // 16 and 219 are H.273's, scaled up by 2^(n−8) with the depth.
            Self::Limited | Self::Unspecified => Levels {
                offset: 16 << shift,
                scale: 219 << shift,
                min,
                max,
            },
        })
    }

    /// Quantisation for a Cb or Cr component at `bit_depth`.
    ///
    /// `offset` is the neutral point, so a caller feeds `E` in `-0.5..=0.5`.
    ///
    /// # Derivation
    ///
    /// ```text
    ///   narrow: Cb = Clip1( Round( (224·E'Cb + 128) · 2^(n−8) ) )
    ///   full:   Cb = Clip1( Round( (2^n − 1)·E'Cb ) + 2^(n−1) )
    /// ```
    ///
    /// The full-range neutral point is `2^(n−1)`, which is 128 at 8 bits — one
    /// above the arithmetic centre of `0..=255`. That asymmetry is the
    /// specification's and is why full-range chroma cannot reach `-0.5`
    /// exactly.
    #[must_use]
    pub const fn chroma_levels(self, bit_depth: u32) -> Option<Levels> {
        let Some((min, max, shift)) = bounds(bit_depth) else {
            return None;
        };
        Some(match self {
            Self::Full => Levels {
                offset: 1 << (bit_depth - 1),
                scale: max,
                min,
                max,
            },
            Self::Limited | Self::Unspecified => Levels {
                offset: 128 << shift,
                scale: 224 << shift,
                min,
                max,
            },
        })
    }
}

/// `(min, max, n − 8)` for a supported bit depth.
const fn bounds(bit_depth: u32) -> Option<(u32, u32, u32)> {
    match bit_depth {
        // `1u64 << 32` rather than `1u32 << 32`, which would overflow.
        8..=32 => Some((0, ((1u64 << bit_depth) - 1) as u32, bit_depth - 8)),
        _ => None,
    }
}
