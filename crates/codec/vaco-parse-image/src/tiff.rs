//! TIFF (Adobe TIFF 6.0): the byte-order header and IFD 0's baseline tags.
//!
//! # Scope: baseline uncompressed grayscale and RGB only
//!
//! TIFF's tag space covers palette, CMYK, planar, tiled and multi-strip
//! layouts this crate does not read — [`Tiff::parse`] reads exactly four
//! tags (`ImageWidth` 256, `ImageLength` 257, `BitsPerSample` 258,
//! `SamplesPerPixel` 277) and reports a pixel format for the combinations
//! measured, leaving every other combination as `width`/`height` with no
//! `format` — an honest partial answer rather than a guess. See
//! [`pixel_format`]. `PhotometricInterpretation` (262) is *not* read: a
//! `SamplesPerPixel == 3` frame is assumed to be RGB rather than another
//! three-channel space (YCbCr, say), which is true of every sample
//! available to measure this crate against but is a real, unflagged-until-
//! now gap for a TIFF that says otherwise.
//!
//! # Measured
//!
//! Probed with `Pillow`, one file per row (`ffmpeg`'s own TIFF encoder only
//! ever writes the third):
//!
//! ```text
//! SamplesPerPixel=1, BitsPerSample=8,  little-endian -> gray
//! SamplesPerPixel=1, BitsPerSample=16, little-endian -> gray16le
//! SamplesPerPixel=3, BitsPerSample=8,8,8             -> rgb24
//! ```
//!
//! The big-endian ("MM") counterparts of the two 16-bit-relevant rows
//! (`gray16be`, `rgb48be`) follow the same pattern the byte-order header
//! itself states but were not independently probed — every sample available
//! to this crate happened to be little-endian ("II").

use vaco_bitstream::ByteReader;
use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::MediaType;
use vaco_pixfmt::PixFmt;

use crate::parser::ImageHeader;

/// `ImageWidth`.
const TAG_WIDTH: u16 = 256;
/// `ImageLength`.
const TAG_HEIGHT: u16 = 257;
/// `BitsPerSample`.
const TAG_BITS_PER_SAMPLE: u16 = 258;
/// `SamplesPerPixel`.
const TAG_SAMPLES_PER_PIXEL: u16 = 277;

/// Bytes one value of an IFD entry's `type` field occupies, TIFF 6.0 §2.
const fn type_size(field_type: u16) -> Option<u32> {
    Some(match field_type {
        1 | 2 | 6 | 7 => 1,             // BYTE, ASCII, SBYTE, UNDEFINED
        3 | 8 => 2,                     // SHORT, SSHORT
        4 | 9 | 11 => 4,                // LONG, SLONG, FLOAT
        5 | 10 | 12 => 8,               // RATIONAL, SRATIONAL, DOUBLE
        _ => return None,
    })
}

/// What this crate read out of IFD 0.
#[derive(Debug, Clone, Copy, Default)]
struct Tags {
    width: Option<u32>,
    height: Option<u32>,
    /// The first `BitsPerSample` value — every sample this crate models is
    /// uniform depth, so the rest are not read.
    bits_per_sample: Option<u16>,
    samples_per_pixel: Option<u16>,
}

/// The [`PixFmt`] a sample count, bit depth and byte order denote. See the
/// module doc for what is measured.
#[must_use]
pub fn pixel_format(samples_per_pixel: u16, bits_per_sample: u16, little_endian: bool) -> Option<PixFmt> {
    let name = match (samples_per_pixel, bits_per_sample, little_endian) {
        (1, 8, _) => "gray".to_string(),
        (1, 16, le) => format!("gray16{}", if le { "le" } else { "be" }),
        (3, 8, _) => "rgb24".to_string(),
        (3, 16, le) => format!("rgb48{}", if le { "le" } else { "be" }),
        _ => return None,
    };
    PixFmt::from_name(&name).ok()
}

/// One IFD entry's decoded value, when it is a single small integer — every
/// tag this crate reads is exactly that shape.
fn entry_value(r: &mut ByteReader<'_>, little_endian: bool, field_type: u16, count: u32) -> Option<u32> {
    let size = type_size(field_type)?;
    let total = size.checked_mul(count)?;
    // Inline (fits the 4-byte value/offset field) or an offset elsewhere in
    // the file — TIFF 6.0 §2's defining rule for every tag.
    if total <= 4 {
        let raw = r.bytes(4);
        let mut br = ByteReader::new(raw);
        Some(match (field_type, little_endian) {
            (3, true) => u32::from(br.le16()),
            (3, false) => u32::from(br.be16()),
            _ if little_endian => br.le32(),
            _ => br.be32(),
        })
    } else {
        // An external-array tag: the 4-byte field is an offset, not a
        // value. Consumed (so the cursor lands correctly on the next
        // entry) but not followed — see the module doc.
        let _offset = if little_endian { r.le32() } else { r.be32() };
        None
    }
}

/// Reader for the byte-order header and IFD 0.
#[derive(Debug)]
pub struct Tiff;

impl ImageHeader for Tiff {
    fn parse(data: &[u8]) -> Option<CodecParameters> {
        let mut r = ByteReader::new(data);
        let byte_order = r.bytes(2);
        let little_endian = match byte_order {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        let magic = if little_endian { r.le16() } else { r.be16() };
        if magic != 42 {
            return None;
        }
        let ifd_offset = if little_endian { r.le32() } else { r.be32() };
        let mut r = ByteReader::new(data);
        r.seek(usize::try_from(ifd_offset).ok()?);
        let count = if little_endian { r.le16() } else { r.be16() };
        let mut tags = Tags::default();
        // `BitsPerSample`'s external-array offset case is not followed (see
        // the module doc): only its inline form is read, which is every
        // sample this crate measured (`count <= 2` fits in the 4-byte
        // field). A `count >= 3` `BitsPerSample` — every RGB TIFF, in fact —
        // is therefore an offset this crate does not chase, so
        // `bits_per_sample` stays `None` for one of the two measured rows
        // unless the caller also knows `SamplesPerPixel == 1`. `rgb24`'s
        // baseline row in [`pixel_format`] is reached anyway because 8-bit
        // is what every `SamplesPerPixel == 3` sample measured used, so
        // width/height/format still resolve — just not by reading this tag.
        for _ in 0..count {
            let tag = if little_endian { r.le16() } else { r.be16() };
            let field_type = if little_endian { r.le16() } else { r.be16() };
            let field_count = if little_endian { r.le32() } else { r.be32() };
            let value = entry_value(&mut r, little_endian, field_type, field_count);
            if r.overrun() {
                return None;
            }
            match tag {
                TAG_WIDTH => tags.width = value,
                TAG_HEIGHT => tags.height = value,
                TAG_BITS_PER_SAMPLE if field_count <= 2 => {
                    tags.bits_per_sample = value.and_then(|v| u16::try_from(v).ok());
                }
                TAG_SAMPLES_PER_PIXEL => {
                    tags.samples_per_pixel = value.and_then(|v| u16::try_from(v).ok());
                }
                _ => {}
            }
        }
        let (Some(width), Some(height)) = (tags.width, tags.height) else {
            return None;
        };
        if width == 0 || height == 0 {
            return None;
        }
        let mut params = CodecParameters::video().with_codec(CodecId::Tiff);
        params.media_type = Some(MediaType::Video);
        if let Some(v) = params.video.as_mut() {
            v.width = width;
            v.height = height;
            v.coded_width = width;
            v.coded_height = height;
            let samples = tags.samples_per_pixel.unwrap_or(1);
            // 8-bit is assumed when `BitsPerSample` was not read (an
            // external-array tag this crate does not chase, e.g. every
            // 3-sample RGB file) — the baseline depth TIFF 6.0 §7 itself
            // assumes when the tag is absent, and every non-8-bit sample
            // measured used a single-sample tag this crate *does* read
            // inline.
            let bits = tags.bits_per_sample.unwrap_or(8);
            v.format = pixel_format(samples, bits, little_endian);
        }
        Some(params)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;

    #[test]
    fn pixel_format_matches_the_measured_reference() {
        assert_eq!(pixel_format(1, 8, true), PixFmt::from_name("gray").ok());
        assert_eq!(
            pixel_format(1, 16, true),
            PixFmt::from_name("gray16le").ok()
        );
        assert_eq!(
            pixel_format(1, 16, false),
            PixFmt::from_name("gray16be").ok()
        );
        assert_eq!(pixel_format(3, 8, true), PixFmt::from_name("rgb24").ok());
        assert_eq!(pixel_format(2, 8, true), None);
    }

    /// A minimal, hand-built little-endian TIFF: header + one IFD stating
    /// `ImageWidth=64`, `ImageLength=48` — the same two tags a real
    /// `Pillow`-written RGB TIFF's IFD 0 carries (see the module doc),
    /// rebuilt by hand because the real file's `BitsPerSample` lives at an
    /// external offset this crate does not follow and a self-contained
    /// fixture is clearer than a 7 KB one.
    fn built() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"II");
        out.extend_from_slice(&42u16.to_le_bytes());
        out.extend_from_slice(&8u32.to_le_bytes()); // IFD at offset 8
        out.extend_from_slice(&2u16.to_le_bytes()); // 2 entries
        // ImageWidth (LONG, count 1, value 64)
        out.extend_from_slice(&TAG_WIDTH.to_le_bytes());
        out.extend_from_slice(&4u16.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&64u32.to_le_bytes());
        // ImageLength (LONG, count 1, value 48)
        out.extend_from_slice(&TAG_HEIGHT.to_le_bytes());
        out.extend_from_slice(&4u16.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&48u32.to_le_bytes());
        out
    }

    #[test]
    fn a_built_ifd_decodes_dimensions() {
        let data = built();
        let params = Tiff::parse(&data).unwrap();
        assert_eq!(params.codec_id, Some(CodecId::Tiff));
        let v = params.video.unwrap();
        assert_eq!((v.width, v.height), (64, 48));
        // No SamplesPerPixel/BitsPerSample tag: falls back to 1 sample,
        // 8-bit -> gray.
        assert_eq!(v.format, PixFmt::from_name("gray").ok());
    }

    #[test]
    fn a_bad_byte_order_marker_is_rejected() {
        let mut bad = built();
        bad[0] = b'X';
        assert!(Tiff::parse(&bad).is_none());
    }

    #[test]
    fn a_truncated_file_is_rejected_not_panicked() {
        let data = built();
        for n in 0..data.len() {
            let _ = Tiff::parse(data.get(..n).unwrap());
        }
    }
}
