//! BMP (Windows and OS/2 bitmap): `BITMAPFILEHEADER` and the leading fields
//! of `BITMAPINFOHEADER`.
//!
//! # Measured pixel-format mapping
//!
//! Probed per bit depth with `Pillow`-written files (`ffmpeg`'s own BMP
//! encoder only ever writes 24bpp) plus one `ffmpeg -pix_fmt rgb555le` case
//! for 16bpp:
//!
//! ```text
//! bpp=1,  compression=BI_RGB (0)         -> pal8
//! bpp=8,  compression=BI_RGB (0)         -> pal8
//! bpp=16, compression=BI_RGB (0)         -> rgb555le
//! bpp=24, compression=BI_RGB (0)         -> bgr24
//! bpp=32, compression=BI_RGB (0)         -> bgr0 (not `bgra` — a plain
//!         40-byte `BITMAPINFOHEADER` states no alpha mask, and probed
//!         output confirms the reference does not assume one)
//! ```
//!
//! `bpp=4` was not independently probed — `Pillow`'s BMP writer silently
//! ignores a requested 4-bit depth for an RGB source — but is mapped to
//! `pal8` by the same pattern 1 and 8 bpp both confirm (every indexed depth
//! needs a palette to mean anything). `BI_BITFIELDS` (`compression == 3`,
//! which chooses the RGB bit layout of a 16 or 32 bpp image from three
//! explicit masks) is not read at all; those depths report no format rather
//! than guessing at a layout this crate has not measured.

use vaco_bitstream::ByteReader;
use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::MediaType;
use vaco_pixfmt::PixFmt;

use crate::parser::ImageHeader;

/// `BI_RGB`: no compression, no bitfield masks.
const BI_RGB: u32 = 0;

/// The [`PixFmt`] a bit depth and compression method denote. See the module
/// doc for what is measured.
#[must_use]
pub fn pixel_format(bits_per_pixel: u16, compression: u32) -> Option<PixFmt> {
    if compression != BI_RGB {
        return None;
    }
    let name = match bits_per_pixel {
        1 | 4 | 8 => "pal8",
        16 => "rgb555le",
        24 => "bgr24",
        32 => "bgr0",
        _ => return None,
    };
    PixFmt::from_name(name).ok()
}

/// Reader for `BITMAPFILEHEADER` + `BITMAPINFOHEADER`'s leading fields.
#[derive(Debug)]
pub struct Bmp;

impl ImageHeader for Bmp {
    fn parse(data: &[u8]) -> Option<CodecParameters> {
        let mut r = ByteReader::new(data);
        if r.bytes(2) != b"BM" {
            return None;
        }
        let _file_size = r.le32();
        let _reserved = r.le32();
        let _pixel_data_offset = r.le32();
        let dib_header_size = r.le32();
        if dib_header_size < 40 {
            // `BITMAPCOREHEADER` (12 bytes, 16-bit dimensions) and other
            // pre-Windows-3.0 variants are a different, unmeasured layout.
            return None;
        }
        let width = r.get_i32_le();
        let height = r.get_i32_le();
        let _planes = r.le16();
        let bits_per_pixel = r.le16();
        let compression = r.le32();
        r.check().ok()?;
        // Height is negative for a top-down bitmap (§ the OS/2 2.0
        // extension every modern writer supports); the magnitude is what
        // `CodecParameters` wants either way.
        let (width, height) = (width.unsigned_abs(), height.unsigned_abs());
        if width == 0 || height == 0 {
            return None;
        }
        let mut params = CodecParameters::video().with_codec(CodecId::Bmp);
        params.media_type = Some(MediaType::Video);
        if let Some(v) = params.video.as_mut() {
            v.width = width;
            v.height = height;
            v.coded_width = width;
            v.coded_height = height;
            v.format = pixel_format(bits_per_pixel, compression);
        }
        Some(params)
    }
}

/// A little-endian signed 32-bit read. [`ByteReader`] has no signed 32-bit
/// accessor (every other format this crate reads is unsigned), so this
/// wraps the unsigned one rather than adding a method upstream for one
/// caller.
trait ByteReaderExt {
    fn get_i32_le(&mut self) -> i32;
}

impl ByteReaderExt for ByteReader<'_> {
    fn get_i32_le(&mut self) -> i32 {
        self.le32().cast_signed()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code over fixed fixtures")]
mod tests {
    use super::*;

    /// A real 16x16 24bpp `Pillow`-written BMP's header bytes.
    const REAL_HEADER_24BPP: [u8; 34] = [
        0x42, 0x4d, 0x36, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x36, 0x00, 0x00, 0x00, 0x28,
        0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x01, 0x00, 0x18, 0x00,
        0x00, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn a_real_24bpp_header_decodes() {
        let params = Bmp::parse(&REAL_HEADER_24BPP).unwrap();
        assert_eq!(params.codec_id, Some(CodecId::Bmp));
        let v = params.video.unwrap();
        assert_eq!((v.width, v.height), (16, 16));
        assert_eq!(v.format, PixFmt::from_name("bgr24").ok());
    }

    #[test]
    fn pixel_format_matches_the_measured_reference() {
        assert_eq!(pixel_format(1, BI_RGB), PixFmt::from_name("pal8").ok());
        assert_eq!(pixel_format(8, BI_RGB), PixFmt::from_name("pal8").ok());
        assert_eq!(pixel_format(16, BI_RGB), PixFmt::from_name("rgb555le").ok());
        assert_eq!(pixel_format(24, BI_RGB), PixFmt::from_name("bgr24").ok());
        assert_eq!(pixel_format(32, BI_RGB), PixFmt::from_name("bgr0").ok());
        assert_eq!(pixel_format(16, 3), None, "BI_BITFIELDS is unmeasured");
    }

    #[test]
    fn a_bad_signature_is_rejected() {
        let mut bad = REAL_HEADER_24BPP;
        bad[0] = 0;
        assert!(Bmp::parse(&bad).is_none());
    }

    #[test]
    fn a_truncated_file_is_rejected_not_panicked() {
        for n in 0..REAL_HEADER_24BPP.len() {
            let _ = Bmp::parse(REAL_HEADER_24BPP.get(..n).unwrap());
        }
    }
}
