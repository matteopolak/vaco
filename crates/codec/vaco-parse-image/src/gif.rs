//! GIF (`GIF89a` / `GIF87a`, CompuServe): the signature and the Logical Screen
//! Descriptor.
//!
//! # `pix_fmt` is a constant, not a header field — measured
//!
//! GIF has no bit-depth or colour-model field to read: every pixel is a
//! palette index into either the global or a per-frame local colour table
//! (Logical Screen Descriptor §18, Image Descriptor §20). `ffprobe` does not
//! report `pal8` for it, though — probed on a `libavcodec`-encoded GIF,
//! `pix_fmt=bgra` regardless of whether the source had any transparency, so
//! the reference's decoder always resolves the palette (and the possible
//! transparent-colour index, Graphic Control Extension §23) into BGRA rather
//! than leaving the index unresolved. [`Gif::parse`] reports that constant
//! rather than `pal8`, matching what a caller comparing against the
//! reference actually sees.

use vaco_bitstream::ByteReader;
use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::MediaType;
use vaco_pixfmt::PixFmt;

use crate::parser::ImageHeader;

/// Reader for the signature and Logical Screen Descriptor.
#[derive(Debug)]
pub struct Gif;

impl ImageHeader for Gif {
    fn parse(data: &[u8]) -> Option<CodecParameters> {
        let mut r = ByteReader::new(data);
        let signature = r.bytes(6);
        if signature != b"GIF87a" && signature != b"GIF89a" {
            return None;
        }
        let width = r.le16();
        let height = r.le16();
        let _packed = r.u8();
        let _background_color_index = r.u8();
        let _pixel_aspect_ratio = r.u8();
        r.check().ok()?;
        if width == 0 || height == 0 {
            return None;
        }
        let mut params = CodecParameters::video().with_codec(CodecId::Gif);
        params.media_type = Some(MediaType::Video);
        if let Some(v) = params.video.as_mut() {
            v.width = u32::from(width);
            v.height = u32::from(height);
            v.coded_width = v.width;
            v.coded_height = v.height;
            // See the module doc: measured constant, not read from a field.
            v.format = PixFmt::from_name("bgra").ok();
        }
        Some(params)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code over fixed fixtures")]
mod tests {
    use super::*;

    /// Signature + Logical Screen Descriptor for a 64x48 `GIF89a`, real bytes
    /// (`ffmpeg -f lavfi -i testsrc=size=64x48 -frames:v 1 out.gif`).
    const REAL_HEADER: [u8; 13] = [
        0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x40, 0x00, 0x30, 0x00, 0xf7, 0x1f, 0x31,
    ];

    #[test]
    fn a_real_header_decodes() {
        let params = Gif::parse(&REAL_HEADER).unwrap();
        assert_eq!(params.codec_id, Some(CodecId::Gif));
        let v = params.video.unwrap();
        assert_eq!((v.width, v.height), (64, 48));
        assert_eq!(v.format, PixFmt::from_name("bgra").ok());
    }

    #[test]
    fn gif87a_is_also_accepted() {
        let mut data = REAL_HEADER;
        data[4] = b'7';
        assert!(Gif::parse(&data).is_some());
    }

    #[test]
    fn a_bad_signature_is_rejected() {
        let mut bad = REAL_HEADER;
        bad[0] = b'X';
        assert!(Gif::parse(&bad).is_none());
    }

    #[test]
    fn a_truncated_file_is_rejected_not_panicked() {
        for n in 0..REAL_HEADER.len() {
            let _ = Gif::parse(REAL_HEADER.get(..n).unwrap());
        }
    }
}
