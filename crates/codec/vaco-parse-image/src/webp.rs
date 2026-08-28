//! WebP (Google, RIFF container): the three sub-formats a `WEBP` RIFF file
//! carries — lossy (`VP8 `), lossless (`VP8L`) and extended (`VP8X`).
//!
//! # Measured, per sub-format
//!
//! Probed with `Pillow` (this environment's `ffmpeg` has a WebP *decoder*
//! but no encoder to produce fixtures with):
//!
//! ```text
//! "VP8 " alone (lossy, no alpha)        -> pix_fmt=yuv420p
//! "VP8L" alone (lossless)               -> pix_fmt=argb (with or without
//!                                           alpha data — probed both ways)
//! "VP8X" + "ALPH" + "VP8 " (extended,
//!         lossy with alpha)             -> pix_fmt=yuva420p
//! ```
//!
//! `VP8X`'s own hex-dumped payload confirmed the two fields this crate reads
//! directly rather than through the wrapped image chunk: byte 0's bit `0x10`
//! is the `Alpha` flag (set exactly when an `ALPH` chunk follows), and bytes
//! 4..10 are `canvas_width_minus_1`/`canvas_height_minus_1`, 24-bit
//! little-endian.
//!
//! # The lossy sub-format reuses `vaco-parse-vpx`
//!
//! A `"VP8 "` chunk's payload is byte-for-byte a VP8 key frame — the exact
//! bitstream RFC 6386 §9.1 defines and `vaco-parse-vpx::vp8` already reads —
//! so this crate calls [`vaco_parse_vpx::parse_frame_tag`] rather than
//! re-implementing the frame tag (D19).

use vaco_bitstream::ByteReader;
use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::MediaType;
use vaco_pixfmt::PixFmt;

use crate::parser::ImageHeader;

/// `VP8X`'s `Alpha` flag, byte 0 bit 4 (measured; see the module doc).
const VP8X_ALPHA_FLAG: u8 = 0x10;

/// One RIFF chunk: its four-character type and payload.
struct Chunk<'a> {
    fourcc: &'a [u8],
    payload: &'a [u8],
}

/// Iterate a WebP's top-level RIFF chunks, from just after the 12-byte
/// `RIFF`/size/`WEBP` header.
fn chunks(mut data: &[u8]) -> impl Iterator<Item = Chunk<'_>> {
    core::iter::from_fn(move || {
        let mut r = ByteReader::new(data);
        let fourcc = r.bytes(4);
        let size = r.le32();
        let size = usize::try_from(size).ok()?;
        let payload = r.bytes(size);
        if r.overrun() || fourcc.len() < 4 {
            return None;
        }
        // Chunks are padded to an even length; the pad byte is not part of
        // any chunk's payload.
        let consumed = 8usize.checked_add(size)?.checked_add(size % 2)?;
        data = data.get(consumed..).unwrap_or(&[]);
        Some(Chunk { fourcc, payload })
    })
}

/// The VP8L (lossless) bitstream's own 5-byte header: a signature byte plus
/// a packed 32-bit little-endian field carrying 14-bit width-1, 14-bit
/// height-1, a 1-bit alpha flag and a 3-bit version — the WebP Lossless
/// Bitstream Specification's own layout, not derived from a probe (there is
/// no field here a probe could disagree with; it is five fixed-position bit
/// widths).
fn vp8l_dimensions(payload: &[u8]) -> Option<(u32, u32)> {
    let mut r = ByteReader::new(payload);
    if r.u8() != 0x2F {
        return None;
    }
    let packed = r.le32();
    r.check().ok()?;
    let width = (packed & 0x3FFF) + 1;
    let height = ((packed >> 14) & 0x3FFF) + 1;
    Some((width, height))
}

/// Reader for the RIFF/`WEBP` header and its first meaningful chunk(s).
#[derive(Debug)]
pub struct Webp;

impl ImageHeader for Webp {
    fn parse(data: &[u8]) -> Option<CodecParameters> {
        let mut r = ByteReader::new(data);
        if r.bytes(4) != b"RIFF" {
            return None;
        }
        let _riff_size = r.le32();
        if r.bytes(4) != b"WEBP" {
            return None;
        }
        let rest = r.rest();

        let mut params = CodecParameters::video().with_codec(CodecId::Webp);
        params.media_type = Some(MediaType::Video);

        let first = chunks(rest).next()?;
        match first.fourcc {
            b"VP8 " => {
                let tag = vaco_parse_vpx::parse_frame_tag(first.payload)?;
                let (w, h) = tag.size?;
                if let Some(v) = params.video.as_mut() {
                    v.width = u32::from(w);
                    v.height = u32::from(h);
                    v.coded_width = v.width;
                    v.coded_height = v.height;
                    v.format = PixFmt::from_name("yuv420p").ok();
                }
            }
            b"VP8L" => {
                let (width, height) = vp8l_dimensions(first.payload)?;
                if let Some(v) = params.video.as_mut() {
                    v.width = width;
                    v.height = height;
                    v.coded_width = width;
                    v.coded_height = height;
                    v.format = PixFmt::from_name("argb").ok();
                }
            }
            b"VP8X" => {
                let mut vr = ByteReader::new(first.payload);
                let flags = vr.u8();
                let _reserved = vr.bytes(3);
                let width = vr.le24() + 1;
                let height = vr.le24() + 1;
                vr.check().ok()?;
                if width == 0 || height == 0 {
                    return None;
                }
                let has_alpha = flags & VP8X_ALPHA_FLAG != 0;
                // The actual image data is a later chunk (after any
                // `ICCP`/`ALPH`/`ANIM` chunks); only its type is needed here
                // to pick lossy-vs-lossless, since `VP8X` already states the
                // canvas size directly.
                let is_lossless = chunks(rest).any(|c| c.fourcc == b"VP8L");
                if let Some(v) = params.video.as_mut() {
                    v.width = width;
                    v.height = height;
                    v.coded_width = width;
                    v.coded_height = height;
                    v.format = PixFmt::from_name(if is_lossless {
                        "argb"
                    } else if has_alpha {
                        "yuva420p"
                    } else {
                        "yuv420p"
                    })
                    .ok();
                }
            }
            _ => return None,
        }
        Some(params)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code over fixed fixtures")]
mod tests {
    use super::*;

    /// A real 64x48 lossy `Pillow`-written WebP (`RIFF`/`WEBP`/`VP8 `), in
    /// full — [`chunks`] reads the chunk's declared size, so a truncated
    /// buffer shorter than that trips its own overrun check, same as a
    /// genuinely malformed file would.
    #[rustfmt::skip]
    const REAL_LOSSY: [u8; 92] = [
        0x52, 0x49, 0x46, 0x46, 0x54, 0x00, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50, 0x56, 0x50, 0x38, 0x20,
        0x48, 0x00, 0x00, 0x00, 0xf0, 0x03, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x00, 0x30, 0x00, 0x3e, 0x6d,
        0x36, 0x98, 0x49, 0x24, 0x23, 0x22, 0xa1, 0x22, 0xa8, 0x00, 0x80, 0x0d, 0x89, 0x67, 0x00, 0xd4,
        0xf6, 0x80, 0x7e, 0x00, 0x00, 0x15, 0x1a, 0x63, 0x1c, 0x3e, 0x0c, 0xc0, 0x00, 0xfe, 0xf0, 0x9b,
        0x43, 0xff, 0xf2, 0x0b, 0x96, 0x17, 0x5c, 0x8d, 0x7f, 0xff, 0x20, 0x3f, 0xe4, 0x07, 0xfc, 0x80,
        0xff, 0xf8, 0xf7, 0xc5, 0x0d, 0x8a, 0xb0, 0xc8, 0x00, 0x00, 0x00, 0x00,
    ];

    /// A real 64x48 lossless `Pillow`-written WebP, in full (see
    /// [`REAL_LOSSY`]'s doc for why the whole file rather than a prefix).
    const REAL_LOSSLESS: [u8; 42] = [
        0x52, 0x49, 0x46, 0x46, 0x22, 0x00, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50, 0x56, 0x50, 0x38,
        0x4c, 0x15, 0x00, 0x00, 0x00, 0x2f, 0x3f, 0xc0, 0x0b, 0x00, 0x07, 0x10, 0xfd, 0x8f, 0xfe,
        0x07, 0x80, 0x84, 0xf0, 0x7f, 0xbd, 0x18, 0xd1, 0xff, 0xd4, 0x0f, 0x00,
    ];

    /// A real 64x48 extended WebP with alpha, in full: `VP8X` (`Alpha` flag
    /// set) + `ALPH` + `VP8 `.
    #[rustfmt::skip]
    const REAL_EXTENDED_ALPHA: [u8; 134] = [
        0x52, 0x49, 0x46, 0x46, 0x7e, 0x00, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50, 0x56, 0x50, 0x38, 0x58,
        0x0a, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x3f, 0x00, 0x00, 0x2f, 0x00, 0x00, 0x41, 0x4c,
        0x50, 0x48, 0x10, 0x00, 0x00, 0x00, 0x01, 0x07, 0x50, 0xc0, 0x88, 0x08, 0x00, 0x09, 0xe1, 0xff,
        0x7a, 0x31, 0xa2, 0xff, 0xa9, 0x1f, 0x56, 0x50, 0x38, 0x20, 0x48, 0x00, 0x00, 0x00, 0xf0, 0x03,
        0x00, 0x9d, 0x01, 0x2a, 0x40, 0x00, 0x30, 0x00, 0x3e, 0x6d, 0x36, 0x98, 0x49, 0x24, 0x23, 0x22,
        0xa1, 0x22, 0xa8, 0x00, 0x80, 0x0d, 0x89, 0x67, 0x00, 0xd4, 0xf6, 0x80, 0x7e, 0x00, 0x00, 0x15,
        0x1a, 0x63, 0x1c, 0x3e, 0x0c, 0xc0, 0x00, 0xfe, 0xf0, 0x9b, 0x43, 0xff, 0xf2, 0x0b, 0x96, 0x17,
        0x5c, 0x8d, 0x7f, 0xff, 0x20, 0x3f, 0xe4, 0x07, 0xfc, 0x80, 0xff, 0xf8, 0xf7, 0xc5, 0x0d, 0x8a,
        0xb0, 0xc8, 0x00, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn a_real_lossy_header_decodes() {
        let params = Webp::parse(&REAL_LOSSY).unwrap();
        assert_eq!(params.codec_id, Some(CodecId::Webp));
        let v = params.video.unwrap();
        assert_eq!((v.width, v.height), (64, 48));
        assert_eq!(v.format, PixFmt::from_name("yuv420p").ok());
    }

    #[test]
    fn a_real_lossless_header_decodes() {
        let params = Webp::parse(&REAL_LOSSLESS).unwrap();
        let v = params.video.unwrap();
        assert_eq!((v.width, v.height), (64, 48));
        assert_eq!(v.format, PixFmt::from_name("argb").ok());
    }

    #[test]
    fn extended_with_alpha_reports_yuva420p() {
        let params = Webp::parse(&REAL_EXTENDED_ALPHA).unwrap();
        let v = params.video.unwrap();
        assert_eq!((v.width, v.height), (64, 48));
        assert_eq!(v.format, PixFmt::from_name("yuva420p").ok());
    }

    #[test]
    fn a_bad_signature_is_rejected() {
        let mut bad = REAL_LOSSY;
        bad[0] = 0;
        assert!(Webp::parse(&bad).is_none());
    }

    #[test]
    fn a_truncated_file_is_rejected_not_panicked() {
        for n in 0..REAL_LOSSY.len() {
            let _ = Webp::parse(REAL_LOSSY.get(..n).unwrap());
        }
        for n in 0..REAL_EXTENDED_ALPHA.len() {
            let _ = Webp::parse(REAL_EXTENDED_ALPHA.get(..n).unwrap());
        }
    }
}
