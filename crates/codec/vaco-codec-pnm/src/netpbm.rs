//! PBM/PGM/PPM: the classic `NetPBM` triple (`P1`/`P4`, `P2`/`P5`, `P3`/`P6`).
//!
//! `Vaco-Spec-Ref: netpbm-pnm-spec` — the PBM/PGM/PPM format descriptions at
//! <https://netpbm.sourceforge.net/doc/pbm.html> (and `pgm.html`/`ppm.html`),
//! cross-checked against the reference codec's observable byte behaviour
//! (D17).
//!
//! A raw (binary) sample above 255 is always big-endian, regardless of the
//! frame's native pixel-format endianness on the machine that wrote it —
//! measured by round-tripping `gray16le` and `gray16be` source frames through
//! the reference encoder and finding identical output bytes either way.
//!
//! When `maxval` is not 255 (8-bit) or 65535 (16-bit), the reference rescales
//! samples to fill the full range rather than passing them through, and the
//! exact tie-breaking rule for a sample that lands exactly halfway between two
//! output values was not fully pinned down (two probes at different maxvals
//! disagreed on which way a tie rounds); see `planning/CONFORMANCE-FINDINGS.md`.
//! This decoder rescales with ordinary round-half-up, which matched every
//! non-tie sample probed.

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

use crate::bits::{row_bytes_for_bits, set_bit};
use crate::reader::Reader;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Bitmap,
    Graymap,
    Pixmap,
}

#[expect(
    clippy::integer_division,
    reason = "genuine rescale by a runtime denominator, not a bit-shiftable constant"
)]
pub(crate) fn rescale(sample: u32, maxval: u32, out_max: u32) -> u32 {
    if maxval == out_max {
        return sample;
    }
    let num = u64::from(sample) * u64::from(out_max) * 2 + u64::from(maxval);
    let den = u64::from(maxval) * 2;
    u32::try_from(num / den).unwrap_or(out_max)
}

fn read_header(r: &mut Reader<'_>, kind: Kind) -> Result<(bool, u32, u32, u32)> {
    let magic = r.bytes(2)?;
    let (ascii, expect_maxval) = match (kind, magic) {
        (Kind::Bitmap, b"P1") => (true, false),
        (Kind::Bitmap, b"P4") => (false, false),
        (Kind::Graymap, b"P2") | (Kind::Pixmap, b"P3") => (true, true),
        (Kind::Graymap, b"P5") | (Kind::Pixmap, b"P6") => (false, true),
        _ => return Err(Error::InvalidData("pnm: bad magic for this format")),
    };
    let width = r.decimal()?;
    let height = r.decimal()?;
    if width == 0 || height == 0 {
        return Err(Error::InvalidData("pnm: zero-sized image"));
    }
    let maxval = if expect_maxval { r.decimal()? } else { 1 };
    if expect_maxval && !(1..=65535).contains(&maxval) {
        return Err(Error::InvalidData("pnm: maxval out of range"));
    }
    Ok((ascii, width, height, maxval))
}

/// Decode a PBM (`P1`/`P4`) image into [`PixFmt::MonoWhite`].
///
/// # Errors
/// [`Error::InvalidData`] for a malformed header or raster,
/// [`Error::LimitExceeded`] if the declared dimensions exceed `budget`.
pub fn decode_pbm(data: &[u8], budget: &mut Budget) -> Result<Frame> {
    let mut r = Reader::new(data);
    let (ascii, width, height, _) = read_header(&mut r, Kind::Bitmap)?;
    let mut frame = Frame::alloc_video(budget, PixFmt::MonoWhite, width, height)?;
    let FrameData::Video { planes, .. } = &mut frame.data else {
        return Err(Error::InvalidData("pnm: expected a video frame"));
    };
    let plane = planes
        .get_mut(0)
        .ok_or(Error::InvalidData("pnm: no plane 0"))?;
    let stride = plane.stride;
    let buf = plane.data.make_mut();

    if ascii {
        for y in 0..height {
            for x in 0..width {
                let tok = r.token()?;
                let bit = match tok {
                    b"0" => false,
                    b"1" => true,
                    _ => return Err(Error::InvalidData("pnm: pbm sample must be 0 or 1")),
                };
                set_bit(buf, stride, y as usize, x as usize, bit)?;
            }
        }
    } else {
        r.single_whitespace()?;
        let row_len = row_bytes_for_bits(width);
        for y in 0..height {
            let row = r.bytes(row_len)?;
            let dst_start = (y as usize).saturating_mul(stride);
            let dst = buf
                .get_mut(dst_start..dst_start.saturating_add(row_len))
                .ok_or(Error::InvalidData("pnm: row out of bounds"))?;
            dst.copy_from_slice(row);
        }
    }
    Ok(frame)
}

/// Encode a [`PixFmt::MonoWhite`] frame as raw PBM (`P4`).
///
/// # Errors
/// [`Error::Unsupported`] for any other pixel format.
pub fn encode_pbm(frame: &Frame) -> Result<Vec<u8>> {
    let FrameData::Video {
        format,
        width,
        height,
        planes,
    } = &frame.data
    else {
        return Err(Error::InvalidData("pnm: expected a video frame"));
    };
    if *format != PixFmt::MonoWhite {
        return Err(Error::Unsupported("pbm: encoder needs monowhite input"));
    }
    let (width, height) = (*width, *height);
    let plane = planes
        .first()
        .ok_or(Error::InvalidData("pnm: no plane 0"))?;
    let row_len = row_bytes_for_bits(width);
    let mut out = format!("P4\n{width} {height}\n").into_bytes();
    let src = plane.data.as_slice();
    for y in 0..height as usize {
        let start = y.saturating_mul(plane.stride);
        let row = src
            .get(start..start.saturating_add(row_len))
            .ok_or(Error::InvalidData("pnm: row out of bounds"))?;
        out.extend_from_slice(row);
    }
    Ok(out)
}

fn sample_format(maxval: u32, planar_kind: Kind) -> Result<(PixFmt, usize)> {
    Ok(match (planar_kind, maxval <= 255) {
        (Kind::Graymap, true) => (PixFmt::Gray8, 1),
        (Kind::Graymap, false) => (PixFmt::Gray16be, 2),
        (Kind::Pixmap, true) => (PixFmt::Rgb24, 1),
        (Kind::Pixmap, false) => (PixFmt::Rgb48be, 2),
        (Kind::Bitmap, _) => return Err(Error::InvalidData("pnm: bitmap has no sample format")),
    })
}

fn decode_gray_or_rgb(data: &[u8], budget: &mut Budget, kind: Kind) -> Result<Frame> {
    let mut r = Reader::new(data);
    let (ascii, width, height, maxval) = read_header(&mut r, kind)?;
    let (format, sample_bytes) = sample_format(maxval, kind)?;
    let channels = if kind == Kind::Pixmap { 3 } else { 1 };
    let out_max = if sample_bytes == 1 { 255 } else { 65535 };

    let mut frame = Frame::alloc_video(budget, format, width, height)?;
    let FrameData::Video { planes, .. } = &mut frame.data else {
        return Err(Error::InvalidData("pnm: expected a video frame"));
    };
    let plane = planes
        .get_mut(0)
        .ok_or(Error::InvalidData("pnm: no plane 0"))?;
    let stride = plane.stride;
    let buf = plane.data.make_mut();
    let pixel_bytes = sample_bytes * channels;

    if !ascii {
        r.single_whitespace()?;
    }
    for y in 0..height {
        for x in 0..width {
            let mut pixel = [0u8; 6];
            for c in 0..channels {
                let raw = if ascii {
                    r.decimal()?
                } else if sample_bytes == 1 {
                    u32::from(r.u8()?)
                } else {
                    let b = r.bytes(2)?;
                    let &[hi, lo] = b else {
                        return Err(Error::UnexpectedEof);
                    };
                    u32::from(u16::from_be_bytes([hi, lo]))
                };
                if raw > maxval {
                    return Err(Error::InvalidData("pnm: sample exceeds maxval"));
                }
                let scaled = rescale(raw, maxval, out_max);
                let off = c * sample_bytes;
                if sample_bytes == 1 {
                    if let Some(slot) = pixel.get_mut(off) {
                        *slot = scaled as u8;
                    }
                } else {
                    let bytes = (scaled as u16).to_be_bytes();
                    if let Some(dst) = pixel.get_mut(off..off + 2) {
                        dst.copy_from_slice(&bytes);
                    }
                }
            }
            let dst_off = (y as usize)
                .saturating_mul(stride)
                .saturating_add((x as usize).saturating_mul(pixel_bytes));
            let dst = buf
                .get_mut(dst_off..dst_off.saturating_add(pixel_bytes))
                .ok_or(Error::InvalidData("pnm: pixel out of bounds"))?;
            let src = pixel
                .get(..pixel_bytes)
                .ok_or(Error::InvalidData("pnm: short pixel"))?;
            dst.copy_from_slice(src);
        }
    }
    Ok(frame)
}

/// The stream description a PBM header states, without decoding a pixel. See
/// [`crate::video_parameters`].
#[must_use]
pub fn parameters_pbm(data: &[u8]) -> Option<vaco_codec_core::CodecParameters> {
    let (_, width, height, _) = read_header(&mut Reader::new(data), Kind::Bitmap).ok()?;
    Some(crate::video_parameters(
        vaco_codec_core::CodecId::Pbm,
        width,
        height,
        PixFmt::MonoWhite,
    ))
}

fn parameters_gray_or_rgb(
    data: &[u8],
    kind: Kind,
    codec: vaco_codec_core::CodecId,
) -> Option<vaco_codec_core::CodecParameters> {
    let (_, width, height, maxval) = read_header(&mut Reader::new(data), kind).ok()?;
    let (format, _) = sample_format(maxval, kind).ok()?;
    Some(crate::video_parameters(codec, width, height, format))
}

/// The stream description a PGM header states, without decoding a pixel.
#[must_use]
pub fn parameters_pgm(data: &[u8]) -> Option<vaco_codec_core::CodecParameters> {
    parameters_gray_or_rgb(data, Kind::Graymap, vaco_codec_core::CodecId::Pgm)
}

/// The stream description a PPM header states, without decoding a pixel.
#[must_use]
pub fn parameters_ppm(data: &[u8]) -> Option<vaco_codec_core::CodecParameters> {
    parameters_gray_or_rgb(data, Kind::Pixmap, vaco_codec_core::CodecId::Ppm)
}

/// Decode a PGM (`P2`/`P5`) image into `gray8` or `gray16be`, chosen by the
/// header's `maxval`.
///
/// # Errors
/// See [`decode_pbm`].
pub fn decode_pgm(data: &[u8], budget: &mut Budget) -> Result<Frame> {
    decode_gray_or_rgb(data, budget, Kind::Graymap)
}

/// Decode a PPM (`P3`/`P6`) image into `rgb24` or `rgb48be`, chosen by the
/// header's `maxval`.
///
/// # Errors
/// See [`decode_pbm`].
pub fn decode_ppm(data: &[u8], budget: &mut Budget) -> Result<Frame> {
    decode_gray_or_rgb(data, budget, Kind::Pixmap)
}

fn encode_gray_or_rgb(frame: &Frame, kind: Kind) -> Result<Vec<u8>> {
    let FrameData::Video {
        format,
        width,
        height,
        planes,
    } = &frame.data
    else {
        return Err(Error::InvalidData("pnm: expected a video frame"));
    };
    let (magic, want_wide, channels) = match kind {
        Kind::Graymap => ("P5", *format == PixFmt::Gray16be, 1usize),
        Kind::Pixmap => ("P6", *format == PixFmt::Rgb48be, 3usize),
        Kind::Bitmap => return Err(Error::Unsupported("pnm: not a gray/rgb format")),
    };
    let narrow = match kind {
        Kind::Graymap => *format == PixFmt::Gray8,
        Kind::Pixmap => *format == PixFmt::Rgb24,
        Kind::Bitmap => false,
    };
    if !narrow && !want_wide {
        return Err(Error::Unsupported(
            "pnm: unexpected pixel format for encoder",
        ));
    }
    let sample_bytes = if want_wide { 2 } else { 1 };
    let maxval = if want_wide { 65535 } else { 255 };
    let (width, height) = (*width, *height);
    let plane = planes
        .first()
        .ok_or(Error::InvalidData("pnm: no plane 0"))?;
    let src = plane.data.as_slice();
    let pixel_bytes = sample_bytes * channels;

    let mut out = format!("{magic}\n{width} {height}\n{maxval}\n").into_bytes();
    for y in 0..height as usize {
        let row_start = y.saturating_mul(plane.stride);
        let row = src
            .get(row_start..row_start.saturating_add((width as usize).saturating_mul(pixel_bytes)))
            .ok_or(Error::InvalidData("pnm: row out of bounds"))?;
        out.extend_from_slice(row);
    }
    Ok(out)
}

/// Encode a `gray8`/`gray16be` frame as PGM (`P5`).
///
/// # Errors
/// [`Error::Unsupported`] for any other pixel format.
pub fn encode_pgm(frame: &Frame) -> Result<Vec<u8>> {
    encode_gray_or_rgb(frame, Kind::Graymap)
}

/// Encode a `rgb24`/`rgb48be` frame as PPM (`P6`).
///
/// # Errors
/// [`Error::Unsupported`] for any other pixel format.
pub fn encode_ppm(frame: &Frame) -> Result<Vec<u8>> {
    encode_gray_or_rgb(frame, Kind::Pixmap)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code exercising the decoder, not the untrusted-input surface \
              the lint protects"
)]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    #[test]
    fn pbm_ascii_and_raw_agree() {
        let raw = b"P4\n8 2\n\xAA\x55";
        let ascii = b"P1\n8 2\n1 0 1 0 1 0 1 0 0 1 0 1 0 1 0 1";
        let mut b1 = Budget::new(Limits::permissive());
        let mut b2 = Budget::new(Limits::permissive());
        let f1 = decode_pbm(raw, &mut b1).expect("raw");
        let f2 = decode_pbm(ascii, &mut b2).expect("ascii");
        let FrameData::Video { planes: p1, .. } = &f1.data else {
            panic!()
        };
        let FrameData::Video { planes: p2, .. } = &f2.data else {
            panic!()
        };
        assert_eq!(p1[0].data.as_slice(), p2[0].data.as_slice());
    }

    #[test]
    fn pbm_round_trips() {
        let raw = b"P4\n8 2\n\xAA\x55";
        let mut budget = Budget::new(Limits::permissive());
        let frame = decode_pbm(raw, &mut budget).expect("decode");
        let encoded = encode_pbm(&frame).expect("encode");
        assert_eq!(encoded, raw);
    }

    #[test]
    fn pgm_8bit_round_trips() {
        let raw = b"P5\n3 2\n255\n\x00\x7f\xff\x10\x20\x30";
        let mut budget = Budget::new(Limits::permissive());
        let frame = decode_pgm(raw, &mut budget).expect("decode");
        let encoded = encode_pgm(&frame).expect("encode");
        assert_eq!(encoded, raw);
    }

    #[test]
    fn pgm_16bit_round_trips() {
        let mut raw = b"P5\n2 1\n65535\n".to_vec();
        raw.extend_from_slice(&[0x01, 0x02, 0xFF, 0xEE]);
        let mut budget = Budget::new(Limits::permissive());
        let frame = decode_pgm(&raw, &mut budget).expect("decode");
        let encoded = encode_pgm(&frame).expect("encode");
        assert_eq!(encoded, raw);
    }

    #[test]
    fn ppm_8bit_round_trips() {
        let raw = b"P6\n2 1\n255\n\x01\x02\x03\x04\x05\x06";
        let mut budget = Budget::new(Limits::permissive());
        let frame = decode_ppm(raw, &mut budget).expect("decode");
        let encoded = encode_ppm(&frame).expect("encode");
        assert_eq!(encoded, raw);
    }

    #[test]
    fn ppm_ascii_matches_raw() {
        let raw = b"P6\n2 1\n255\n\x01\x02\x03\x04\x05\x06";
        let ascii = b"P3\n2 1\n255\n1 2 3 4 5 6";
        let mut b1 = Budget::new(Limits::permissive());
        let mut b2 = Budget::new(Limits::permissive());
        let f1 = decode_ppm(raw, &mut b1).expect("raw");
        let f2 = decode_ppm(ascii, &mut b2).expect("ascii");
        assert_eq!(encode_ppm(&f1).unwrap(), encode_ppm(&f2).unwrap());
    }

    #[test]
    fn maxval_255_is_not_rescaled() {
        // The identity case: every sample must pass through unchanged.
        for v in [0u32, 1, 127, 254, 255] {
            assert_eq!(rescale(v, 255, 255), v);
        }
    }

    #[test]
    fn rejects_bad_magic() {
        let mut budget = Budget::new(Limits::permissive());
        assert!(decode_pbm(b"P5\n1 1\n255\n\x00", &mut budget).is_err());
    }
}
