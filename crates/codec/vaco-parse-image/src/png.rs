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
//!
//! # `sample_aspect_ratio`, from an optional `pHYs` chunk
//!
//! Measured on a real `ffmpeg -f image2 frame.png` fixture
//! (`fuzz/seeds/diff/image2/frame.png`), which carries a `pHYs` chunk
//! stating `pixels_per_unit_x = pixels_per_unit_y = 1` (unit
//! "unspecified") — and the reference states `sample_aspect_ratio=1:1`,
//! not `N/A`. `pHYs` is optional and, when present, ordered before `IDAT`
//! (§4.3), so this scans chunks after `IHDR` only that far: as soon as
//! `IDAT`/`IEND` is reached, or [`MAX_CHUNKS_SCANNED`] ancillary chunks
//! have gone by, scanning stops — `pHYs` is either found by then or it
//! does not exist. The ratio is `pixels_per_unit_y / pixels_per_unit_x`
//! (a denser X axis means a physically narrower pixel), verified only
//! against this one, square-pixel fixture — not against a file with a
//! genuinely non-square `pHYs`, which this crate has not measured.
//!
//! # `color_range`/`color_space`: measured for truecolor only
//!
//! The same fixture states `color_range=pc` (PNG is always full-range —
//! there is no limited-range convention for it) and `color_space=gbr`
//! (`ffprobe`'s name for the identity/no-transform matrix, i.e. "this is
//! RGB, not YCbCr"). Applied only to `color_type=2` (truecolor), the exact
//! configuration measured — grayscale, indexed and the alpha variants are
//! not asserted one way or the other, the same restraint the pixel-format
//! table above already takes for the 16-bit rows.

use vaco_bitstream::ByteReader;
use vaco_codec_core::{CodecId, CodecParameters};
use vaco_color::{ColorRange, MatrixCoefficients};
use vaco_core::{MediaType, Rational};
use vaco_pixfmt::PixFmt;

use crate::parser::ImageHeader;

/// The 8-byte file signature, §5.2.
const SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

/// Ancillary chunks examined after `IHDR`, looking for `pHYs`, before giving
/// up. Bounds the scan against a file with many small chunks ahead of
/// `IDAT`; real encoders write at most a handful (`pHYs`, `gAMA`, `cHRM`,
/// `sRGB`, `iCCP`, text chunks) before the first `IDAT`.
const MAX_CHUNKS_SCANNED: u32 = 32;

/// `pHYs`'s pixel-aspect-ratio pair, if the chunk is present, well-formed,
/// and reached within [`MAX_CHUNKS_SCANNED`] chunks of `IHDR`.
///
/// `r` must be positioned right after `IHDR`'s payload and CRC. Advances `r`
/// exactly to the end of whichever chunk it stopped on (`pHYs`, `IDAT`,
/// `IEND`, or wherever the scan cap or a truncation ended it) — the caller
/// does nothing further with `r` after this, so leaving it mid-chunk on a
/// stop condition other than `pHYs` is harmless.
fn find_phys(r: &mut ByteReader<'_>) -> Option<Rational> {
    let mut found = None;
    for _ in 0..MAX_CHUNKS_SCANNED {
        if r.remaining() < 8 {
            break;
        }
        let chunk_len = r.be32();
        let chunk_type = r.bytes(4);
        if chunk_type == b"pHYs" {
            if chunk_len >= 9 {
                let x_per_unit = r.be32();
                let y_per_unit = r.be32();
                let _unit = r.u8();
                if x_per_unit > 0 && y_per_unit > 0 {
                    found = Some(Rational::new(
                        y_per_unit.cast_signed(),
                        x_per_unit.cast_signed(),
                    ));
                }
                r.skip(usize::try_from(chunk_len).unwrap_or(0).saturating_sub(9));
            } else {
                r.skip(usize::try_from(chunk_len).unwrap_or(0));
            }
            r.skip(4); // CRC
            break;
        }
        if chunk_type == b"IDAT" || chunk_type == b"IEND" {
            break;
        }
        r.skip(usize::try_from(chunk_len).unwrap_or(0));
        r.skip(4); // CRC
        if r.overrun() {
            break;
        }
    }
    found
}

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
        // The rest of IHDR's fixed 13-byte payload (compression method,
        // filter method, interlace method — none read, all fixed-position
        // regardless of what `length` claims) plus its CRC, so `find_phys`
        // starts exactly on the next chunk's length field.
        r.skip(3);
        r.skip(4);
        let sample_aspect_ratio = find_phys(&mut r);

        let mut params = CodecParameters::video().with_codec(CodecId::Png);
        params.media_type = Some(MediaType::Video);
        if let Some(v) = params.video.as_mut() {
            v.width = width;
            v.height = height;
            v.coded_width = width;
            v.coded_height = height;
            v.format = pixel_format(color_type, bit_depth);
            if let Some(sar) = sample_aspect_ratio {
                v.sample_aspect_ratio = sar;
            }
            // Measured only for truecolor — see the module doc's
            // "color_range/color_space: measured for truecolor only"
            // section for why the other `color_type`s are left alone.
            if color_type == 2 {
                v.color.range = ColorRange::Full;
                v.color.matrix = MatrixCoefficients::Identity;
            }
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

    /// Appends a chunk (any 4-byte type, real or not) after `REAL_IHDR`.
    /// The CRC is never checked by this parser (measured: neither is
    /// `IHDR`'s own), so it is left as four zero bytes rather than computed.
    fn with_chunk(chunk_type: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut v = REAL_IHDR.to_vec();
        v.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        v.extend_from_slice(&chunk_type);
        v.extend_from_slice(payload);
        v.extend_from_slice(&[0, 0, 0, 0]); // CRC, unchecked
        v
    }

    /// The exact `pHYs` payload measured on a real `ffmpeg -f image2
    /// frame.png` fixture (`fuzz/seeds/diff/image2/frame.png`, offset
    /// `0x25`): `pixels_per_unit_x = pixels_per_unit_y = 1`, unit
    /// unspecified — a stated (not merely absent) 1:1 pixel aspect ratio.
    #[test]
    fn a_measured_phys_chunk_yields_a_1_1_sample_aspect_ratio() {
        let bytes = with_chunk(*b"pHYs", &[0, 0, 0, 1, 0, 0, 0, 1, 0]);
        let v = Png::parse(&bytes).unwrap().video.unwrap();
        assert_eq!(v.sample_aspect_ratio, Rational::new(1, 1));
    }

    #[test]
    fn a_non_square_phys_chunk_divides_the_right_way_round() {
        // Twice as many samples per unit on X as on Y: each pixel is
        // physically twice as tall as it is wide, so covers *less* width
        // per sample — the ratio comes out below 1, not above it.
        let bytes = with_chunk(*b"pHYs", &[0, 0, 0, 2, 0, 0, 0, 1, 0]);
        let v = Png::parse(&bytes).unwrap().video.unwrap();
        assert_eq!(v.sample_aspect_ratio, Rational::new(1, 2));
    }

    #[test]
    fn no_phys_chunk_leaves_sample_aspect_ratio_at_its_default() {
        let bytes = with_chunk(*b"tEXt", b"comment");
        let v = Png::parse(&bytes).unwrap().video.unwrap();
        assert_eq!(v.sample_aspect_ratio, Rational::default());
    }

    #[test]
    fn scanning_stops_at_idat_without_finding_a_later_phys() {
        // A `pHYs` after `IDAT` is not spec-legal (§4.3 orders it earlier),
        // so this must not be found — confirms the scan actually stops
        // rather than reading past the image data looking for it.
        let mut bytes = with_chunk(*b"IDAT", &[0, 1, 2, 3]);
        let phys_only = with_chunk(*b"pHYs", &[0, 0, 0, 5, 0, 0, 0, 5, 0]);
        let tail = phys_only.get(REAL_IHDR.len()..).unwrap_or(&[]);
        bytes.extend_from_slice(tail);
        let v = Png::parse(&bytes).unwrap().video.unwrap();
        assert_eq!(v.sample_aspect_ratio, Rational::default());
    }

    /// `REAL_IHDR` is `color_type=2` (truecolor) — see its own doc comment —
    /// so this is the exact configuration measured against the reference.
    #[test]
    fn truecolor_states_the_measured_colour_metadata() {
        let v = Png::parse(&REAL_IHDR).unwrap().video.unwrap();
        assert_eq!(v.color.range, ColorRange::Full);
        assert_eq!(v.color.matrix, MatrixCoefficients::Identity);
    }

    #[test]
    fn grayscale_does_not_get_the_truecolor_colour_metadata() {
        // color_type=0 (gray), otherwise identical to REAL_IHDR.
        let mut gray = REAL_IHDR;
        gray[25] = 0;
        let v = Png::parse(&gray).unwrap().video.unwrap();
        assert_eq!(v.color.range, ColorRange::Unspecified);
        assert_eq!(v.color.matrix, MatrixCoefficients::Unspecified);
    }

    #[test]
    fn a_truncated_phys_chunk_does_not_panic() {
        let full = with_chunk(*b"pHYs", &[0, 0, 0, 1, 0, 0, 0, 1, 0]);
        for n in REAL_IHDR.len()..full.len() {
            let _ = Png::parse(full.get(..n).unwrap());
        }
    }
}
