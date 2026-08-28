//! PNG (ISO/IEC 15948, W3C PNG 2nd edition): the signature and the `IHDR`
//! chunk.
//!
//! # Measured pixel-format mapping
//!
//! `color_type`/`bit_depth` combinations were probed with a real PNG per
//! combination (`Pillow`, since `ffmpeg`'s own PNG encoder always writes
//! 8-bit truecolor or truecolor+alpha and cannot produce the other rows):
//!
//! ```text
//! color_type=0 (gray)         bit_depth=8   -> gray
//! color_type=0 (gray)         bit_depth=16  -> gray16be
//! color_type=2 (truecolor)    bit_depth=8   -> rgb24
//! color_type=3 (indexed)      any           -> pal8
//! color_type=4 (gray+alpha)   bit_depth=8   -> ya8
//! color_type=6 (rgba)         bit_depth=8   -> rgba
//! ```
//!
//! The 16-bit truecolor/alpha rows (`rgb48be`/`rgba64be`/`ya16be`) follow the
//! same big-endian-doubled pattern `gray`/`gray16be` establishes but were not
//! independently probed — `Pillow`'s PNG encoder does not reach them either.
//! `bit_depth` 1/2/4 for grayscale (sub-byte packed samples) has no
//! `vaco-pixfmt` entry to map to and is left unrecognised (`None`) rather
//! than approximated.

use vaco_bitstream::ByteReader;
use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::MediaType;
use vaco_pixfmt::PixFmt;

use crate::parser::ImageHeader;

/// The 8-byte file signature, §5.2.
const SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

/// The [`PixFmt`] an `IHDR`'s `color_type`/`bit_depth` denotes. See the
/// module doc for what is measured and what is a same-pattern extrapolation.
#[must_use]
pub fn pixel_format(color_type: u8, bit_depth: u8) -> Option<PixFmt> {
    let name = match (color_type, bit_depth) {
        (0, 8) => "gray",
        (0, 16) => "gray16be",
        (2, 8) => "rgb24",
        (2, 16) => "rgb48be",
        (3, _) => "pal8",
        (4, 8) => "ya8",
        (4, 16) => "ya16be",
        (6, 8) => "rgba",
        (6, 16) => "rgba64be",
        _ => return None,
    };
    PixFmt::from_name(name).ok()
}

/// Reader for `IHDR`.
#[derive(Debug)]
pub struct Png;

impl ImageHeader for Png {
    fn parse(data: &[u8]) -> Option<CodecParameters> {
        let mut r = ByteReader::new(data);
        if r.bytes(8) != SIGNATURE {
            return None;
        }
        let length = r.be32();
        if length < 13 || r.bytes(4) != b"IHDR" {
            return None;
        }
        let width = r.be32();
        let height = r.be32();
        let bit_depth = r.u8();
        let color_type = r.u8();
        r.check().ok()?;
        if width == 0 || height == 0 {
            return None;
        }
        let mut params = CodecParameters::video().with_codec(CodecId::Png);
        params.media_type = Some(MediaType::Video);
        if let Some(v) = params.video.as_mut() {
            v.width = width;
            v.height = height;
            v.coded_width = width;
            v.coded_height = height;
            v.format = pixel_format(color_type, bit_depth);
        }
        Some(params)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code over fixed fixtures")]
mod tests {
    use super::*;

    /// A real 1x1 truecolor `Pillow`-written PNG's leading bytes: signature
    /// + `IHDR` stating 1x1, 8-bit, `color_type=2` (RGB).
    const REAL_IHDR: [u8; 33] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xde,
    ];

    #[test]
    fn a_real_ihdr_decodes() {
        let params = Png::parse(&REAL_IHDR).unwrap();
        assert_eq!(params.codec_id, Some(CodecId::Png));
        let v = params.video.unwrap();
        assert_eq!((v.width, v.height), (1, 1));
        assert_eq!(v.format, PixFmt::from_name("rgb24").ok());
    }

    #[test]
    fn pixel_format_matches_the_measured_reference() {
        assert_eq!(pixel_format(0, 8), PixFmt::from_name("gray").ok());
        assert_eq!(pixel_format(0, 16), PixFmt::from_name("gray16be").ok());
        assert_eq!(pixel_format(2, 8), PixFmt::from_name("rgb24").ok());
        assert_eq!(pixel_format(3, 8), PixFmt::from_name("pal8").ok());
        assert_eq!(pixel_format(3, 1), PixFmt::from_name("pal8").ok());
        assert_eq!(pixel_format(4, 8), PixFmt::from_name("ya8").ok());
        assert_eq!(pixel_format(6, 8), PixFmt::from_name("rgba").ok());
        assert_eq!(pixel_format(0, 1), None);
    }

    #[test]
    fn a_bad_signature_is_rejected() {
        let mut bad = REAL_IHDR;
        bad[0] = 0;
        assert!(Png::parse(&bad).is_none());
    }

    #[test]
    fn a_truncated_file_is_rejected_not_panicked() {
        for n in 0..REAL_IHDR.len() {
            let _ = Png::parse(REAL_IHDR.get(..n).unwrap());
        }
    }
}
