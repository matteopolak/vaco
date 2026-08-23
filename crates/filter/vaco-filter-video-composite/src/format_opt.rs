//! `overlay`'s `format=` enum and its measured `PixFmt` mapping.
//!
//! # Measured against ffmpeg 8.1
//!
//! ```sh
//! ffprobe -f lavfi -i "color=white:8x8,format=yuv420p[m];color=red:4x4,format=yuv420p[o];\
//!   [m][o]overlay=format=$F" -show_entries stream=pix_fmt -of default=nk=1:nw=1
//! ```
//!
//! | `format=` | value | Output `pix_fmt` |
//! |---|---|---|
//! | `yuv420` | 0 (default) | `yuv420p` — **no alpha** |
//! | `yuv420p10` | 1 | `yuva420p10le` — has alpha |
//! | `yuv422` | 2 | `yuva422p` |
//! | `yuv422p10` | 3 | `yuva422p10le` |
//! | `yuv444` | 4 | `yuva444p` |
//! | `yuv444p10` | 5 | `yuva444p10le` |
//! | `rgb` | 6 | `rgb24` — no alpha |
//! | `gbrp` | 7 | `gbrap` |
//! | `auto` | 8 | the main input's own family, alpha added — see [`Format::resolve`] |
//!
//! `yuv420` is the one asymmetry: every wider-chroma or higher-depth option
//! gains an alpha plane, but the plain 8-bit 4:2:0 default does not. Not
//! derivable from any pattern; recorded because it looks like an omission
//! and is not one.
//!
//! `auto` was probed with `rgba`/`rgba` inputs (kept `rgba`), `yuv444p`/
//! `yuv420p` inputs (main's family won: `yuva444p`), and `nv12`/`yuv420p`
//! inputs (`yuva420p`) — in every case, the main input's own chroma family
//! with an alpha plane added, or unchanged if it already had one.
//! [`Format::resolve`] implements exactly that for the families this crate
//! composites (4:2:0, 4:2:2, 4:4:4, GBR, RGB); an input outside those
//! families is a reported gap, not a guess — see its doc.

use vaco_pixfmt::PixFmt;

/// `overlay`'s `format=` option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    #[default]
    Yuv420,
    Yuv420p10,
    Yuv422,
    Yuv422p10,
    Yuv444,
    Yuv444p10,
    Rgb,
    Gbrp,
    Auto,
}

impl Format {
    /// Parse the option value: the reference's names, or its numeric
    /// spelling `0`..=`8`.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "yuv420" | "0" => Some(Self::Yuv420),
            "yuv420p10" | "1" => Some(Self::Yuv420p10),
            "yuv422" | "2" => Some(Self::Yuv422),
            "yuv422p10" | "3" => Some(Self::Yuv422p10),
            "yuv444" | "4" => Some(Self::Yuv444),
            "yuv444p10" | "5" => Some(Self::Yuv444p10),
            "rgb" | "6" => Some(Self::Rgb),
            "gbrp" | "7" => Some(Self::Gbrp),
            "auto" | "8" => Some(Self::Auto),
            _ => None,
        }
    }

    /// The concrete blend format, given the main input's own negotiated
    /// format (only consulted for [`Format::Auto`]).
    ///
    /// # Errors
    /// [`vaco_core::Error::Unsupported`] for `Auto` when `main` is not one of
    /// the families this crate recognises (4:2:0/4:2:2/4:4:4 YUV, GBR, RGB) —
    /// reported rather than guessed, since the reference's own family
    /// coverage for `auto` was not exhaustively probed (see this module's
    /// doc: three families were checked, not all ~270 formats `vaco-pixfmt`
    /// knows).
    pub fn resolve(self, main: PixFmt) -> vaco_core::Result<PixFmt> {
        Ok(match self {
            Self::Yuv420 => PixFmt::Yuv420p,
            Self::Yuv420p10 => PixFmt::Yuva420p10le,
            Self::Yuv422 => PixFmt::Yuva422p,
            Self::Yuv422p10 => PixFmt::Yuva422p10le,
            Self::Yuv444 => PixFmt::Yuva444p,
            Self::Yuv444p10 => PixFmt::Yuva444p10le,
            Self::Rgb => PixFmt::Rgb24,
            Self::Gbrp => PixFmt::Gbrap,
            Self::Auto => auto_family(main)?,
        })
    }
}

/// `auto`'s measured rule: the main input's own chroma family, with an alpha
/// plane if it does not already have one.
fn auto_family(main: PixFmt) -> vaco_core::Result<PixFmt> {
    if main.has_alpha() {
        return Ok(main);
    }
    let target = match main {
        PixFmt::Rgb24 => PixFmt::Rgba,
        PixFmt::Gbrp => PixFmt::Gbrap,
        _ if main.is_rgb() || main.has(vaco_pixfmt::PixFmtFlags::XYZ) => {
            return Err(vaco_core::Error::Unsupported(
                "overlay: format=auto on this RGB-family input is not one of the measured cases",
            ));
        }
        _ => match main.log2_chroma() {
            (0, 0) => PixFmt::Yuva444p,
            (1, 0) => PixFmt::Yuva422p,
            (1, 1) => PixFmt::Yuva420p,
            _ => {
                return Err(vaco_core::Error::Unsupported(
                    "overlay: format=auto on this chroma layout is not one of the measured cases",
                ));
            }
        },
    };
    Ok(target)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn yuv420_has_no_alpha_but_every_wider_variant_does() {
        assert!(!Format::Yuv420.resolve(PixFmt::Yuv420p).unwrap().has_alpha());
        assert!(
            Format::Yuv420p10
                .resolve(PixFmt::Yuv420p)
                .unwrap()
                .has_alpha()
        );
        assert!(Format::Yuv422.resolve(PixFmt::Yuv420p).unwrap().has_alpha());
        assert!(Format::Yuv444.resolve(PixFmt::Yuv420p).unwrap().has_alpha());
        assert!(Format::Gbrp.resolve(PixFmt::Yuv420p).unwrap().has_alpha());
    }

    #[test]
    fn rgb_has_no_alpha() {
        assert!(!Format::Rgb.resolve(PixFmt::Yuv420p).unwrap().has_alpha());
        assert_eq!(Format::Rgb.resolve(PixFmt::Yuv420p).unwrap(), PixFmt::Rgb24);
    }

    #[test]
    fn auto_keeps_rgba_as_rgba() {
        assert_eq!(Format::Auto.resolve(PixFmt::Rgba).unwrap(), PixFmt::Rgba);
    }

    #[test]
    fn auto_adds_alpha_to_the_mains_own_chroma_family() {
        assert_eq!(
            Format::Auto.resolve(PixFmt::Yuv444p).unwrap(),
            PixFmt::Yuva444p
        );
        assert_eq!(
            Format::Auto.resolve(PixFmt::Nv12).unwrap(),
            PixFmt::Yuva420p
        );
        assert_eq!(
            Format::Auto.resolve(PixFmt::Yuv420p).unwrap(),
            PixFmt::Yuva420p
        );
    }

    #[test]
    fn option_names_and_numbers_both_parse() {
        assert_eq!(Format::from_name("yuv420"), Some(Format::Yuv420));
        assert_eq!(Format::from_name("0"), Some(Format::Yuv420));
        assert_eq!(Format::from_name("auto"), Some(Format::Auto));
        assert_eq!(Format::from_name("8"), Some(Format::Auto));
        assert_eq!(Format::from_name("nonsense"), None);
    }
}
