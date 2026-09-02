//! PAM (`P7`): a keyword header naming `WIDTH`/`HEIGHT`/`DEPTH`/`MAXVAL`/
//! `TUPLTYPE`, then one raw raster.
//!
//! `Vaco-Spec-Ref: netpbm-pam-spec` —
//! <https://netpbm.sourceforge.net/doc/pam.html>, cross-checked against the
//! reference codec's observable byte behaviour (D17).
//!
//! Only the five tuple types below are supported; anything else is
//! [`Error::Unsupported`] rather than guessed at. `BLACKANDWHITE` is the
//! subtle one: PAM's own raster stores it byte-per-sample (one `0`/`1` byte
//! per pixel, unlike PBM's bit-packing), but the reference decodes it into the
//! same bit-packed pixel format as PBM — measured by comparing a `BLACKANDWHITE`
//! PAM and a `P4` PBM built from the same source image and finding identical
//! decoded output. This decoder packs/unpacks to match.

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

use crate::bits::{get_bit, set_bit};
use crate::netpbm::rescale;
use crate::reader::Reader;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tuple {
    BlackAndWhite,
    Grayscale,
    GrayscaleAlpha,
    Rgb,
    RgbAlpha,
}

impl Tuple {
    fn from_name(name: &[u8], depth: u32) -> Result<Self> {
        let t = match name {
            b"BLACKANDWHITE" => Self::BlackAndWhite,
            b"GRAYSCALE" => Self::Grayscale,
            b"GRAYSCALE_ALPHA" => Self::GrayscaleAlpha,
            b"RGB" => Self::Rgb,
            b"RGB_ALPHA" => Self::RgbAlpha,
            _ => return Err(Error::Unsupported("pam: unsupported TUPLTYPE")),
        };
        if t.depth() != depth {
            return Err(Error::InvalidData("pam: DEPTH does not match TUPLTYPE"));
        }
        Ok(t)
    }

    const fn depth(self) -> u32 {
        match self {
            Self::BlackAndWhite | Self::Grayscale => 1,
            Self::GrayscaleAlpha => 2,
            Self::Rgb => 3,
            Self::RgbAlpha => 4,
        }
    }
}

struct Header {
    width: u32,
    height: u32,
    maxval: u32,
    tuple: Tuple,
}

fn read_header(r: &mut Reader<'_>) -> Result<Header> {
    let magic = r.bytes(2)?;
    if magic != b"P7" {
        return Err(Error::InvalidData("pam: bad magic"));
    }
    let (mut width, mut height, mut depth, mut maxval) = (None, None, None, None);
    let mut tupltype: Option<Vec<u8>> = None;
    loop {
        let key = r.token()?;
        match key {
            b"ENDHDR" => break,
            b"WIDTH" => width = Some(r.decimal()?),
            b"HEIGHT" => height = Some(r.decimal()?),
            b"DEPTH" => depth = Some(r.decimal()?),
            b"MAXVAL" => maxval = Some(r.decimal()?),
            b"TUPLTYPE" => tupltype = Some(r.token()?.to_vec()),
            _ => return Err(Error::InvalidData("pam: unknown header keyword")),
        }
    }
    r.single_whitespace()?;

    let width = width.ok_or(Error::InvalidData("pam: missing WIDTH"))?;
    let height = height.ok_or(Error::InvalidData("pam: missing HEIGHT"))?;
    let depth = depth.ok_or(Error::InvalidData("pam: missing DEPTH"))?;
    let maxval = maxval.ok_or(Error::InvalidData("pam: missing MAXVAL"))?;
    let tupltype = tupltype.ok_or(Error::InvalidData("pam: missing TUPLTYPE"))?;
    if width == 0 || height == 0 || !(1..=65535).contains(&maxval) {
        return Err(Error::InvalidData("pam: invalid dimensions or maxval"));
    }
    let tuple = Tuple::from_name(&tupltype, depth)?;
    if tuple == Tuple::BlackAndWhite && maxval != 1 {
        return Err(Error::InvalidData("pam: BLACKANDWHITE requires MAXVAL 1"));
    }
    Ok(Header {
        width,
        height,
        maxval,
        tuple,
    })
}

fn pixfmt_for(tuple: Tuple, maxval: u32) -> (PixFmt, usize, usize) {
    let wide = maxval > 255;
    match tuple {
        Tuple::BlackAndWhite => (PixFmt::MonoBlack, 0, 1),
        Tuple::Grayscale if wide => (PixFmt::Gray16be, 2, 1),
        Tuple::Grayscale => (PixFmt::Gray8, 1, 1),
        Tuple::GrayscaleAlpha if wide => (PixFmt::Ya16be, 2, 2),
        Tuple::GrayscaleAlpha => (PixFmt::Ya8, 1, 2),
        Tuple::Rgb if wide => (PixFmt::Rgb48be, 2, 3),
        Tuple::Rgb => (PixFmt::Rgb24, 1, 3),
        Tuple::RgbAlpha if wide => (PixFmt::Rgba64be, 2, 4),
        Tuple::RgbAlpha => (PixFmt::Rgba, 1, 4),
    }
}

/// The stream description a PAM header states, without decoding a pixel. See
/// [`crate::video_parameters`].
#[must_use]
pub fn parameters(data: &[u8]) -> Option<vaco_codec_core::CodecParameters> {
    let header = read_header(&mut Reader::new(data)).ok()?;
    let (format, _, _) = pixfmt_for(header.tuple, header.maxval);
    Some(crate::video_parameters(
        vaco_codec_core::CodecId::Pam,
        header.width,
        header.height,
        format,
    ))
}

/// Decode a PAM image.
///
/// # Errors
/// [`Error::InvalidData`] for a malformed header or raster,
/// [`Error::Unsupported`] for a `TUPLTYPE` this crate does not map,
/// [`Error::LimitExceeded`] if the declared dimensions exceed `budget`.
pub fn decode(data: &[u8], budget: &mut Budget) -> Result<Frame> {
    let mut r = Reader::new(data);
    let header = read_header(&mut r)?;
    let (format, sample_bytes, channels) = pixfmt_for(header.tuple, header.maxval);
    let out_max = if sample_bytes == 2 { 65535 } else { 255 };

    let mut frame = Frame::alloc_video(budget, format, header.width, header.height)?;
    let FrameData::Video { planes, .. } = &mut frame.data else {
        return Err(Error::InvalidData("pam: expected a video frame"));
    };
    let plane = planes
        .get_mut(0)
        .ok_or(Error::InvalidData("pam: no plane 0"))?;
    let stride = plane.stride;
    let buf = plane.data.make_mut();

    if header.tuple == Tuple::BlackAndWhite {
        for y in 0..header.height {
            for x in 0..header.width {
                let sample = r.u8()?;
                let bit = match sample {
                    0 => false,
                    1 => true,
                    _ => return Err(Error::InvalidData("pam: BLACKANDWHITE sample must be 0 or 1")),
                };
                set_bit(buf, stride, y as usize, x as usize, bit)?;
            }
        }
        return Ok(frame);
    }

    let pixel_bytes = sample_bytes * channels;
    for y in 0..header.height {
        for x in 0..header.width {
            let mut pixel = [0u8; 8];
            for c in 0..channels {
                let raw = if sample_bytes == 1 {
                    u32::from(r.u8()?)
                } else {
                    let b = r.bytes(2)?;
                    let &[hi, lo] = b else {
                        return Err(Error::UnexpectedEof);
                    };
                    u32::from(u16::from_be_bytes([hi, lo]))
                };
                if raw > header.maxval {
                    return Err(Error::InvalidData("pam: sample exceeds maxval"));
                }
                let scaled = rescale(raw, header.maxval, out_max);
                let off = c * sample_bytes;
                if sample_bytes == 1 {
                    if let Some(slot) = pixel.get_mut(off) {
                        *slot = scaled as u8;
                    }
                } else if let Some(dst) = pixel.get_mut(off..off + 2) {
                    dst.copy_from_slice(&(scaled as u16).to_be_bytes());
                }
            }
            let dst_off = (y as usize)
                .saturating_mul(stride)
                .saturating_add((x as usize).saturating_mul(pixel_bytes));
            let dst = buf
                .get_mut(dst_off..dst_off.saturating_add(pixel_bytes))
                .ok_or(Error::InvalidData("pam: pixel out of bounds"))?;
            let src = pixel
                .get(..pixel_bytes)
                .ok_or(Error::InvalidData("pam: short pixel"))?;
            dst.copy_from_slice(src);
        }
    }
    Ok(frame)
}

fn tuple_for_format(format: PixFmt) -> Result<(Tuple, &'static str)> {
    Ok(match format {
        PixFmt::MonoBlack => (Tuple::BlackAndWhite, "BLACKANDWHITE"),
        PixFmt::Gray8 | PixFmt::Gray16be => (Tuple::Grayscale, "GRAYSCALE"),
        PixFmt::Ya8 | PixFmt::Ya16be => (Tuple::GrayscaleAlpha, "GRAYSCALE_ALPHA"),
        PixFmt::Rgb24 | PixFmt::Rgb48be => (Tuple::Rgb, "RGB"),
        PixFmt::Rgba | PixFmt::Rgba64be => (Tuple::RgbAlpha, "RGB_ALPHA"),
        _ => return Err(Error::Unsupported("pam: encoder needs a mapped pixel format")),
    })
}

/// Encode a frame as PAM, choosing `TUPLTYPE` from its pixel format.
///
/// # Errors
/// [`Error::Unsupported`] for a pixel format with no `TUPLTYPE` mapping.
pub fn encode(frame: &Frame) -> Result<Vec<u8>> {
    let FrameData::Video {
        format,
        width,
        height,
        planes,
    } = &frame.data
    else {
        return Err(Error::InvalidData("pam: expected a video frame"));
    };
    let (tuple, tupltype_name) = tuple_for_format(*format)?;
    let (width, height) = (*width, *height);
    let plane = planes.first().ok_or(Error::InvalidData("pam: no plane 0"))?;
    let depth = tuple.depth();

    if tuple == Tuple::BlackAndWhite {
        let mut out =
            format!("P7\nWIDTH {width}\nHEIGHT {height}\nDEPTH 1\nMAXVAL 1\nTUPLTYPE {tupltype_name}\nENDHDR\n")
                .into_bytes();
        let src = plane.data.as_slice();
        for y in 0..height as usize {
            for x in 0..width as usize {
                out.push(u8::from(get_bit(src, plane.stride, y, x)?));
            }
        }
        return Ok(out);
    }

    let wide = matches!(
        *format,
        PixFmt::Gray16be | PixFmt::Ya16be | PixFmt::Rgb48be | PixFmt::Rgba64be
    );
    let sample_bytes = if wide { 2 } else { 1 };
    let maxval = if wide { 65535 } else { 255 };
    let pixel_bytes = sample_bytes * depth as usize;
    let mut out = format!(
        "P7\nWIDTH {width}\nHEIGHT {height}\nDEPTH {depth}\nMAXVAL {maxval}\nTUPLTYPE {tupltype_name}\nENDHDR\n"
    )
    .into_bytes();
    let src = plane.data.as_slice();
    for y in 0..height as usize {
        let start = y.saturating_mul(plane.stride);
        let row = src
            .get(start..start.saturating_add((width as usize).saturating_mul(pixel_bytes)))
            .ok_or(Error::InvalidData("pam: row out of bounds"))?;
        out.extend_from_slice(row);
    }
    Ok(out)
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

    fn header(depth: u32, maxval: u32, tupltype: &str, w: u32, h: u32) -> Vec<u8> {
        format!("P7\nWIDTH {w}\nHEIGHT {h}\nDEPTH {depth}\nMAXVAL {maxval}\nTUPLTYPE {tupltype}\nENDHDR\n")
            .into_bytes()
    }

    #[test]
    fn grayscale_round_trips() {
        let mut data = header(1, 255, "GRAYSCALE", 2, 1);
        data.extend_from_slice(&[10, 200]);
        let mut budget = Budget::new(Limits::permissive());
        let frame = decode(&data, &mut budget).expect("decode");
        assert_eq!(encode(&frame).expect("encode"), data);
    }

    #[test]
    fn rgb_alpha_round_trips() {
        let mut data = header(4, 255, "RGB_ALPHA", 1, 1);
        data.extend_from_slice(&[1, 2, 3, 4]);
        let mut budget = Budget::new(Limits::permissive());
        let frame = decode(&data, &mut budget).expect("decode");
        assert_eq!(encode(&frame).expect("encode"), data);
    }

    #[test]
    fn blackandwhite_round_trips() {
        let mut data = header(1, 1, "BLACKANDWHITE", 4, 1);
        data.extend_from_slice(&[0, 1, 1, 0]);
        let mut budget = Budget::new(Limits::permissive());
        let frame = decode(&data, &mut budget).expect("decode");
        assert_eq!(encode(&frame).expect("encode"), data);
    }

    #[test]
    fn blackandwhite_matches_pbm_bit_convention() {
        // sample=1 in BLACKANDWHITE must land on the same bit as PBM's `1`.
        let mut data = header(1, 1, "BLACKANDWHITE", 8, 1);
        data.extend_from_slice(&[1, 0, 1, 0, 1, 0, 1, 0]);
        let mut budget = Budget::new(Limits::permissive());
        let frame = decode(&data, &mut budget).expect("decode");
        let FrameData::Video { format, planes, .. } = &frame.data else {
            panic!()
        };
        assert_eq!(*format, PixFmt::MonoBlack);
        assert_eq!(planes[0].data.as_slice()[0], 0b1010_1010);
    }

    #[test]
    fn rejects_unsupported_tupltype() {
        let data = header(5, 255, "CMYK", 1, 1);
        let mut budget = Budget::new(Limits::permissive());
        assert!(decode(&data, &mut budget).is_err());
    }

    #[test]
    fn rejects_depth_mismatch() {
        let data = header(2, 255, "RGB", 1, 1);
        let mut budget = Budget::new(Limits::permissive());
        assert!(decode(&data, &mut budget).is_err());
    }
}
