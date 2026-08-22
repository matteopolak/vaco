//! [`ChromaLocation`]: where a subsampled chroma sample sits relative to the
//! luma grid.

use crate::ChromaLocation;

impl ChromaLocation {
    /// Every variant, in code-point order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Unspecified,
            Self::Left,
            Self::Center,
            Self::TopLeft,
            Self::Top,
            Self::BottomLeft,
            Self::Bottom,
        ]
    }

    /// The reference tool's `AVChromaLocation` code point.
    ///
    /// This numbering is one greater than H.264's and H.265'
    /// `chroma_sample_loc_type`, which starts its own enumeration at 0 for
    /// "left" and has no "unspecified" member — absence of the syntax element is
    /// what means unspecified there. See [`Self::from_h264_loc_type`].
    #[inline]
    #[must_use]
    pub const fn to_u8(self) -> u8 {
        match self {
            Self::Unspecified => 0,
            Self::Left => 1,
            Self::Center => 2,
            Self::TopLeft => 3,
            Self::Top => 4,
            Self::BottomLeft => 5,
            Self::Bottom => 6,
        }
    }

    /// The variant for a reference `AVChromaLocation` code point.
    #[must_use]
    pub const fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Unspecified,
            1 => Self::Left,
            2 => Self::Center,
            3 => Self::TopLeft,
            4 => Self::Top,
            5 => Self::BottomLeft,
            6 => Self::Bottom,
            _ => return None,
        })
    }

    /// The variant for an H.264 / H.265 `chroma_sample_loc_type`, which runs
    /// 0..=5 and is offset by one from [`Self::from_u8`].
    #[must_use]
    pub const fn from_h264_loc_type(t: u8) -> Option<Self> {
        if t > 5 {
            return None;
        }
        Self::from_u8(t + 1)
    }

    /// The name the reference tool prints as `chroma_location` in
    /// `ffprobe -show_streams`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            // D17: this one prints "unspecified", while `color_range`,
            // `color_space`, `color_transfer` and `color_primaries` all print
            // "unknown" for their own unspecified value. The inconsistency is
            // the reference's; both spellings are observable in `-show_streams`
            // output, so both are reproduced. Verified against ffmpeg 8.1.
            Self::Unspecified => "unspecified",
            Self::Left => "left",
            Self::Center => "center",
            Self::TopLeft => "topleft",
            Self::Top => "top",
            Self::BottomLeft => "bottomleft",
            Self::Bottom => "bottom",
        }
    }

    /// Parse a name as the reference's `-chroma_sample_location` option accepts
    /// it.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "unknown" | "unspecified" => Self::Unspecified,
            "left" => Self::Left,
            "center" => Self::Center,
            "topleft" => Self::TopLeft,
            "top" => Self::Top,
            "bottomleft" => Self::BottomLeft,
            "bottom" => Self::Bottom,
            _ => return None,
        })
    }

    /// `(horizontal, vertical)` position of the chroma sample within its 2×2
    /// luma group, in luma-sample units.
    ///
    /// `(0.0, 0.0)` is the top-left luma sample of the group; `(1.0, 1.0)` is
    /// the top-left sample of the *next* group down and right. These are the
    /// offsets a 4:2:0 resampler must apply so that chroma lands where the
    /// encoder said it was — get them wrong and edges acquire a coloured fringe
    /// on one side.
    ///
    /// `None` for [`Self::Unspecified`]: there is no position to report, and
    /// substituting a default here would hide the fact that one was chosen.
    /// Most decoders default to [`Self::Left`], which is MPEG-2 siting.
    ///
    /// # For other subsamplings
    ///
    /// 4:2:2 has no vertical decimation, so only the horizontal component
    /// applies. 4:4:4 has neither and the siting is meaningless. The values
    /// below are stated for 4:2:0, which is the only case where all six
    /// positions are distinguishable.
    #[must_use]
    pub const fn sample_offset_420(self) -> Option<(f32, f32)> {
        Some(match self {
            Self::Unspecified => return None,
            // MPEG-2: horizontally co-sited with the left luma column, vertically
            // halfway between the two luma rows.
            Self::Left => (0.0, 0.5),
            // MPEG-1 / JPEG: centred in the group both ways.
            Self::Center => (0.5, 0.5),
            // Fully co-sited with the top-left luma sample.
            Self::TopLeft => (0.0, 0.0),
            Self::Top => (0.5, 0.0),
            Self::BottomLeft => (0.0, 1.0),
            Self::Bottom => (0.5, 1.0),
        })
    }
}
