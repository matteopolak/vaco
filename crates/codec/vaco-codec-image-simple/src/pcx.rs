//! PCX (`ZSoft` Paintbrush): a 128-byte header, then one RLE-compressed plane
//! per scanline per channel, planes concatenated within a row.
//!
//! `Vaco-Spec-Ref: zsoft-pcx-spec` — the `ZSoft` PCX file format, cross-checked
//! against the reference codec's observable byte behaviour (D17): only
//! 3-plane 8-bit-per-plane (truecolor RGB) is supported; single-plane
//! 8bpp-with-palette is [`Error::Unsupported`] since this crate carries no
//! palette side-data type.

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

use crate::reader::Reader;

const HEADER_LEN: usize = 128;

struct Header {
    width: u32,
    height: u32,
    bytes_per_line: usize,
    nplanes: u8,
}

fn read_header(r: &mut Reader<'_>) -> Result<Header> {
    let manufacturer = r.u8()?;
    let _version = r.u8()?;
    let encoding = r.u8()?;
    let bpp = r.u8()?;
    if manufacturer != 0x0A || encoding != 1 || bpp != 8 {
        return Err(Error::Unsupported(
            "pcx: only RLE-encoded 8-bit planes are supported",
        ));
    }
    let xmin = r.u16_le()?;
    let ymin = r.u16_le()?;
    let xmax = r.u16_le()?;
    let ymax = r.u16_le()?;
    if xmax < xmin || ymax < ymin {
        return Err(Error::InvalidData("pcx: invalid window"));
    }
    let width = u32::from(xmax - xmin) + 1;
    let height = u32::from(ymax - ymin) + 1;
    r.bytes(4)?; // hdpi, vdpi
    r.bytes(48)?; // 16-colour EGA palette
    r.u8()?; // reserved
    let nplanes = r.u8()?;
    let bytes_per_line = usize::from(r.u16_le()?);
    r.seek(HEADER_LEN)?;
    if nplanes != 3 {
        return Err(Error::Unsupported("pcx: only 3-plane RGB is supported"));
    }
    Ok(Header {
        width,
        height,
        bytes_per_line,
        nplanes,
    })
}

/// One scanline's worth of RLE-compressed bytes, decoded into exactly `n`
/// output bytes.
fn rle_decode_line(r: &mut Reader<'_>, n: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    while out.len() < n {
        let byte = r.u8()?;
        if byte & 0xC0 == 0xC0 {
            let count = usize::from(byte & 0x3F);
            let value = r.u8()?;
            for _ in 0..count {
                out.push(value);
            }
        } else {
            out.push(byte);
        }
    }
    Ok(out)
}

/// The stream description PCX's own header states, without decoding a pixel.
///
/// `None` for anything [`decode`] itself would refuse — the two read the same
/// header through the same [`read_header`], so a file this describes is a
/// file that decodes.
#[must_use]
pub fn parameters(data: &[u8]) -> Option<vaco_codec_core::CodecParameters> {
    let header = read_header(&mut Reader::new(data)).ok()?;
    Some(crate::video_parameters(
        vaco_codec_core::CodecId::Pcx,
        header.width,
        header.height,
        PixFmt::Rgb24,
    ))
}

/// Decode a truecolor (3-plane, 8-bit) PCX image into `rgb24`.
///
/// # Errors
/// [`Error::Unsupported`] for a palette-based or non-RLE PCX,
/// [`Error::InvalidData`] for a malformed header or scanline,
/// [`Error::LimitExceeded`] if the declared dimensions exceed `budget`.
pub fn decode(data: &[u8], budget: &mut Budget) -> Result<Frame> {
    let mut r = Reader::new(data);
    let header = read_header(&mut r)?;

    let mut frame = Frame::alloc_video(budget, PixFmt::Rgb24, header.width, header.height)?;
    let FrameData::Video { planes, .. } = &mut frame.data else {
        return Err(Error::InvalidData("pcx: expected a video frame"));
    };
    let plane = planes
        .get_mut(0)
        .ok_or(Error::InvalidData("pcx: no plane 0"))?;
    let stride = plane.stride;
    let buf = plane.data.make_mut();
    let width = header.width as usize;

    for y in 0..header.height as usize {
        let row_start = y.saturating_mul(stride);
        for channel in 0..usize::from(header.nplanes) {
            let line = rle_decode_line(&mut r, header.bytes_per_line)?;
            for x in 0..width {
                let sample = line.get(x).copied().ok_or(Error::UnexpectedEof)?;
                let off = row_start.saturating_add(x * 3 + channel);
                if let Some(slot) = buf.get_mut(off) {
                    *slot = sample;
                }
            }
        }
    }
    Ok(frame)
}

fn rle_encode_line(out: &mut Vec<u8>, line: &[u8]) {
    let mut i = 0;
    while i < line.len() {
        let value = line.get(i).copied().unwrap_or(0);
        let mut run = 1usize;
        while run < 63 && line.get(i + run).copied() == Some(value) {
            run += 1;
        }
        if run > 1 || value & 0xC0 == 0xC0 {
            out.push(0xC0 | (run as u8));
            out.push(value);
        } else {
            out.push(value);
        }
        i += run;
    }
}

fn put(out: &mut [u8], off: usize, bytes: &[u8]) -> Result<()> {
    out.get_mut(off..off + bytes.len())
        .ok_or(Error::InvalidData("pcx: header write out of bounds"))?
        .copy_from_slice(bytes);
    Ok(())
}

/// Encode an `rgb24` frame as a 3-plane, RLE-compressed PCX.
///
/// # Errors
/// [`Error::Unsupported`] for any other pixel format.
pub fn encode(frame: &Frame) -> Result<Vec<u8>> {
    let FrameData::Video {
        format,
        width,
        height,
        planes,
    } = &frame.data
    else {
        return Err(Error::InvalidData("pcx: expected a video frame"));
    };
    if *format != PixFmt::Rgb24 {
        return Err(Error::Unsupported("pcx: encoder needs rgb24 input"));
    }
    let (width, height) = (*width, *height);
    let plane = planes
        .first()
        .ok_or(Error::InvalidData("pcx: no plane 0"))?;
    let bytes_per_line = (width as usize).next_multiple_of(2);

    let mut out = vec![0u8; HEADER_LEN];
    put(&mut out, 0, &[0x0A, 5, 1, 8])?;
    put(&mut out, 4, &0u16.to_le_bytes())?;
    put(&mut out, 6, &0u16.to_le_bytes())?;
    put(&mut out, 8, &(width as u16 - 1).to_le_bytes())?;
    put(&mut out, 10, &(height as u16 - 1).to_le_bytes())?;
    put(&mut out, 12, &1u16.to_le_bytes())?;
    put(&mut out, 14, &1u16.to_le_bytes())?;
    put(&mut out, 65, &[3])?; // nplanes
    put(&mut out, 66, &(bytes_per_line as u16).to_le_bytes())?;

    let src = plane.data.as_slice();
    let mut line = vec![0u8; bytes_per_line];
    for y in 0..height as usize {
        let row_start = y.saturating_mul(plane.stride);
        for channel in 0..3usize {
            for (x, slot) in line.iter_mut().enumerate().take(width as usize) {
                *slot = src
                    .get(row_start + x * 3 + channel)
                    .copied()
                    .ok_or(Error::InvalidData("pcx: row out of bounds"))?;
            }
            for slot in line.iter_mut().skip(width as usize) {
                *slot = 0;
            }
            rle_encode_line(&mut out, &line);
        }
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

    #[test]
    fn round_trips_small_rgb() {
        let mut budget = Budget::new(Limits::permissive());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgb24, 5, 3).expect("alloc");
        let FrameData::Video { planes, .. } = &mut frame.data else {
            panic!()
        };
        let stride = planes[0].stride;
        let buf = planes[0].data.make_mut();
        for y in 0..3 {
            for x in 0..5 {
                let off = y * stride + x * 3;
                buf[off] = (x * 40) as u8;
                buf[off + 1] = (y * 60) as u8;
                buf[off + 2] = 200;
            }
        }
        let expected = buf.to_vec();

        let encoded = encode(&frame).expect("encode");
        let mut budget2 = Budget::new(Limits::permissive());
        let decoded = decode(&encoded, &mut budget2).expect("decode");
        let FrameData::Video { planes: p2, .. } = &decoded.data else {
            panic!()
        };
        for y in 0..3usize {
            for x in 0..5usize {
                let o1 = y * stride + x * 3;
                let o2 = y * p2[0].stride + x * 3;
                assert_eq!(expected[o1..o1 + 3], p2[0].data.as_slice()[o2..o2 + 3]);
            }
        }
    }

    #[test]
    fn rejects_non_rle() {
        let mut header = vec![0u8; 128];
        header[0] = 0x0A;
        header[2] = 0; // encoding = uncompressed, unsupported
        header[3] = 8;
        let mut budget = Budget::new(Limits::permissive());
        assert!(matches!(
            decode(&header, &mut budget),
            Err(Error::Unsupported(_))
        ));
    }
}
