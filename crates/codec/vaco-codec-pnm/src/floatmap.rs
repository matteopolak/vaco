//! PFM (`Pf`/`PF`) and PHM (`Ph`/`PH`): a header naming a byte-order sign
//! plus a raw float raster, bottom row first.
//!
//! `Vaco-Spec-Ref: netpbm-pfm-spec` —
//! <https://netpbm.sourceforge.net/doc/pfm.html>; PHM is the reference
//! codec's half-float sibling of the same layout, confirmed by comparing a
//! `grayf32le` source frame's raw bytes against `Pf` output byte-for-byte
//! (D17): the raster is the frame's rows in reverse order, each row copied
//! verbatim — the scale field's sign selects the pixel format's endianness
//! and its magnitude is not otherwise interpreted here.

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

use crate::reader::Reader;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sample {
    F32,
    F16,
}

fn pixfmt_for(sample: Sample, color: bool, little_endian: bool) -> PixFmt {
    match (sample, color, little_endian) {
        (Sample::F32, false, true) => PixFmt::Grayf32le,
        (Sample::F32, false, false) => PixFmt::Grayf32be,
        (Sample::F32, true, true) => PixFmt::Rgbf32le,
        (Sample::F32, true, false) => PixFmt::Rgbf32be,
        (Sample::F16, false, true) => PixFmt::Grayf16le,
        (Sample::F16, false, false) => PixFmt::Grayf16be,
        (Sample::F16, true, true) => PixFmt::Rgbf16le,
        (Sample::F16, true, false) => PixFmt::Rgbf16be,
    }
}

/// Everything the `Pf`/`PF`/`Ph`/`PH` header states.
struct Header {
    width: u32,
    height: u32,
    color: bool,
    little_endian: bool,
}

fn read_header(r: &mut Reader<'_>, sample: Sample) -> Result<Header> {
    let magic = r.bytes(2)?;
    let (want_sample, color) = match magic {
        b"Pf" => (Sample::F32, false),
        b"PF" => (Sample::F32, true),
        b"Ph" => (Sample::F16, false),
        b"PH" => (Sample::F16, true),
        _ => return Err(Error::InvalidData("pfm/phm: bad magic")),
    };
    if want_sample != sample {
        return Err(Error::InvalidData(
            "pfm/phm: magic does not match this codec",
        ));
    }
    let width = r.decimal()?;
    let height = r.decimal()?;
    if width == 0 || height == 0 {
        return Err(Error::InvalidData("pfm/phm: zero-sized image"));
    }
    let scale_tok = r.token()?;
    let scale_str =
        std::str::from_utf8(scale_tok).map_err(|_| Error::InvalidData("pfm/phm: bad scale"))?;
    let scale: f64 = scale_str
        .parse()
        .map_err(|_| Error::InvalidData("pfm/phm: bad scale"))?;
    if scale == 0.0 {
        return Err(Error::InvalidData("pfm/phm: scale must be non-zero"));
    }
    r.single_whitespace()?;
    Ok(Header {
        width,
        height,
        color,
        little_endian: scale < 0.0,
    })
}

fn parameters_generic(
    data: &[u8],
    sample: Sample,
    codec: vaco_codec_core::CodecId,
) -> Option<vaco_codec_core::CodecParameters> {
    let header = read_header(&mut Reader::new(data), sample).ok()?;
    Some(crate::video_parameters(
        codec,
        header.width,
        header.height,
        pixfmt_for(sample, header.color, header.little_endian),
    ))
}

/// The stream description a PFM header states, without decoding a pixel. See
/// [`crate::video_parameters`].
#[must_use]
pub fn parameters_pfm(data: &[u8]) -> Option<vaco_codec_core::CodecParameters> {
    parameters_generic(data, Sample::F32, vaco_codec_core::CodecId::Pfm)
}

/// The stream description a PHM header states, without decoding a pixel.
#[must_use]
pub fn parameters_phm(data: &[u8]) -> Option<vaco_codec_core::CodecParameters> {
    parameters_generic(data, Sample::F16, vaco_codec_core::CodecId::Phm)
}

fn decode_generic(data: &[u8], budget: &mut Budget, sample: Sample) -> Result<Frame> {
    let mut r = Reader::new(data);
    let Header {
        width,
        height,
        color,
        little_endian,
    } = read_header(&mut r, sample)?;

    let format = pixfmt_for(sample, color, little_endian);
    let sample_bytes = if sample == Sample::F32 { 4 } else { 2 };
    let channels = if color { 3 } else { 1 };
    let row_len = (width as usize)
        .saturating_mul(channels)
        .saturating_mul(sample_bytes);

    let mut frame = Frame::alloc_video(budget, format, width, height)?;
    let FrameData::Video { planes, .. } = &mut frame.data else {
        return Err(Error::InvalidData("pfm/phm: expected a video frame"));
    };
    let plane = planes
        .get_mut(0)
        .ok_or(Error::InvalidData("pfm/phm: no plane 0"))?;
    let stride = plane.stride;
    let buf = plane.data.make_mut();

    for file_row in 0..height as usize {
        let src = r.bytes(row_len)?;
        let image_row = (height as usize) - 1 - file_row;
        let dst_start = image_row.saturating_mul(stride);
        let dst = buf
            .get_mut(dst_start..dst_start.saturating_add(row_len))
            .ok_or(Error::InvalidData("pfm/phm: row out of bounds"))?;
        dst.copy_from_slice(src);
    }
    Ok(frame)
}

fn encode_generic(frame: &Frame, sample: Sample) -> Result<Vec<u8>> {
    let FrameData::Video {
        format,
        width,
        height,
        planes,
    } = &frame.data
    else {
        return Err(Error::InvalidData("pfm/phm: expected a video frame"));
    };
    let (magic, channels, little_endian) = match (*format, sample) {
        (PixFmt::Grayf32le, Sample::F32) => ("Pf", 1, true),
        (PixFmt::Grayf32be, Sample::F32) => ("Pf", 1, false),
        (PixFmt::Rgbf32le, Sample::F32) => ("PF", 3, true),
        (PixFmt::Rgbf32be, Sample::F32) => ("PF", 3, false),
        (PixFmt::Grayf16le, Sample::F16) => ("Ph", 1, true),
        (PixFmt::Grayf16be, Sample::F16) => ("Ph", 1, false),
        (PixFmt::Rgbf16le, Sample::F16) => ("PH", 3, true),
        (PixFmt::Rgbf16be, Sample::F16) => ("PH", 3, false),
        _ => {
            return Err(Error::Unsupported(
                "pfm/phm: unexpected pixel format for encoder",
            ));
        }
    };
    let sample_bytes = if sample == Sample::F32 { 4 } else { 2 };
    let (width, height) = (*width, *height);
    let plane = planes
        .first()
        .ok_or(Error::InvalidData("pfm/phm: no plane 0"))?;
    let row_len = (width as usize)
        .saturating_mul(channels)
        .saturating_mul(sample_bytes);
    let scale = if little_endian { -1.0 } else { 1.0 };

    let mut out = format!("{magic}\n{width} {height}\n{scale:.6}\n").into_bytes();
    let src = plane.data.as_slice();
    for file_row in 0..height as usize {
        let image_row = (height as usize) - 1 - file_row;
        let start = image_row.saturating_mul(plane.stride);
        let row = src
            .get(start..start.saturating_add(row_len))
            .ok_or(Error::InvalidData("pfm/phm: row out of bounds"))?;
        out.extend_from_slice(row);
    }
    Ok(out)
}

/// Decode a PFM (`Pf`/`PF`) image into a 32-bit float `PixFmt`.
///
/// # Errors
/// [`Error::InvalidData`] for a malformed header or truncated raster,
/// [`Error::LimitExceeded`] if the declared dimensions exceed `budget`.
pub fn decode_pfm(data: &[u8], budget: &mut Budget) -> Result<Frame> {
    decode_generic(data, budget, Sample::F32)
}

/// Encode a 32-bit float frame as PFM.
///
/// # Errors
/// [`Error::Unsupported`] for any other pixel format.
pub fn encode_pfm(frame: &Frame) -> Result<Vec<u8>> {
    encode_generic(frame, Sample::F32)
}

/// Decode a PHM (`Ph`/`PH`) image into a 16-bit float `PixFmt`.
///
/// # Errors
/// See [`decode_pfm`].
pub fn decode_phm(data: &[u8], budget: &mut Budget) -> Result<Frame> {
    decode_generic(data, budget, Sample::F16)
}

/// Encode a 16-bit float frame as PHM.
///
/// # Errors
/// [`Error::Unsupported`] for any other pixel format.
pub fn encode_phm(frame: &Frame) -> Result<Vec<u8>> {
    encode_generic(frame, Sample::F16)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::float_cmp,
    reason = "test code comparing exact bit patterns round-tripped through raw \
              bytes, not the untrusted-input surface the lint protects"
)]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn two_row_pfm() -> Vec<u8> {
        // 1x2 grayscale float, file row 0 = bottom (value 2.0), file row 1 =
        // top (value 1.0) — decode must land 1.0 in image row 0.
        let mut data = b"Pf\n1 2\n-1.000000\n".to_vec();
        data.extend_from_slice(&2.0f32.to_le_bytes());
        data.extend_from_slice(&1.0f32.to_le_bytes());
        data
    }

    #[test]
    fn row_order_is_flipped() {
        let data = two_row_pfm();
        let mut budget = Budget::new(Limits::permissive());
        let frame = decode_pfm(&data, &mut budget).expect("decode");
        let FrameData::Video { format, planes, .. } = &frame.data else {
            panic!()
        };
        assert_eq!(*format, PixFmt::Grayf32le);
        let row0 = &planes[0].data.as_slice()[0..4];
        assert_eq!(f32::from_le_bytes(row0.try_into().unwrap()), 1.0);
    }

    #[test]
    fn pfm_round_trips() {
        let data = two_row_pfm();
        let mut budget = Budget::new(Limits::permissive());
        let frame = decode_pfm(&data, &mut budget).expect("decode");
        assert_eq!(encode_pfm(&frame).expect("encode"), data);
    }

    #[test]
    fn phm_round_trips() {
        let mut data = b"Ph\n2 1\n-1.000000\n".to_vec();
        data.extend_from_slice(&[0x00, 0x3c, 0x00, 0x40]); // two half-floats, raw bits
        let mut budget = Budget::new(Limits::permissive());
        let frame = decode_phm(&data, &mut budget).expect("decode");
        assert_eq!(encode_phm(&frame).expect("encode"), data);
    }

    #[test]
    fn color_pfm_round_trips() {
        let mut data = b"PF\n1 1\n-1.000000\n".to_vec();
        for v in [0.25f32, 0.5, 0.75] {
            data.extend_from_slice(&v.to_le_bytes());
        }
        let mut budget = Budget::new(Limits::permissive());
        let frame = decode_pfm(&data, &mut budget).expect("decode");
        assert_eq!(encode_pfm(&frame).expect("encode"), data);
    }

    #[test]
    fn rejects_wrong_magic_for_codec() {
        let data = b"PF\n1 1\n-1.000000\n\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";
        let mut budget = Budget::new(Limits::permissive());
        assert!(decode_phm(data, &mut budget).is_err());
    }
}
