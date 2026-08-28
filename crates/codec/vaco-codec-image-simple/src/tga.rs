//! TGA (Truevision): an 18-byte header, an optional ID field, then truecolor
//! or grayscale pixels, raw or RLE-packeted.
//!
//! `Vaco-Spec-Ref: truevision-tga-spec` — the Truevision TGA File Format
//! specification. The reference tool ships a decoder (`targa`) but no
//! encoder, so this module's [`encode`] has no reference output to compare
//! against; it writes a spec-conformant uncompressed, top-to-bottom file
//! (image descriptor bit 5 set) rather than guessing at an unverifiable byte
//! layout. Colour-mapped images (`image_type` 1/9) and right-to-left storage
//! are [`Error::Unsupported`].

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

use crate::reader::Reader;

const HEADER_LEN: usize = 18;
const TOP_TO_BOTTOM: u8 = 0x20;
const RIGHT_TO_LEFT: u8 = 0x10;

struct Header {
    width: u32,
    height: u32,
    pixel_depth: u8,
    rle: bool,
    top_to_bottom: bool,
    id_length: u8,
}

fn read_header(r: &mut Reader<'_>) -> Result<Header> {
    let id_length = r.u8()?;
    let color_map_type = r.u8()?;
    let image_type = r.u8()?;
    r.bytes(5)?; // colour-map spec, unused without colour-mapped support
    let _x_origin = r.u16_le()?;
    let _y_origin = r.u16_le()?;
    let width = u32::from(r.u16_le()?);
    let height = u32::from(r.u16_le()?);
    let pixel_depth = r.u8()?;
    let descriptor = r.u8()?;

    if color_map_type != 0 {
        return Err(Error::Unsupported("tga: colour-mapped images are not supported"));
    }
    if descriptor & RIGHT_TO_LEFT != 0 {
        return Err(Error::Unsupported("tga: right-to-left storage is not supported"));
    }
    let (truecolor, rle) = match image_type {
        2 => (true, false),
        3 => (false, false),
        10 => (true, true),
        11 => (false, true),
        _ => return Err(Error::Unsupported("tga: unsupported image_type")),
    };
    if width == 0 || height == 0 {
        return Err(Error::InvalidData("tga: zero-sized image"));
    }
    if truecolor && !matches!(pixel_depth, 24 | 32) {
        return Err(Error::Unsupported("tga: unsupported truecolor pixel depth"));
    }
    if !truecolor && pixel_depth != 8 {
        return Err(Error::Unsupported("tga: unsupported grayscale pixel depth"));
    }
    Ok(Header {
        width,
        height,
        pixel_depth,
        rle,
        top_to_bottom: descriptor & TOP_TO_BOTTOM != 0,
        id_length,
    })
}

fn dest_row(file_row: u32, height: u32, top_to_bottom: bool) -> u32 {
    if top_to_bottom {
        file_row
    } else {
        height - 1 - file_row
    }
}

/// Decode a TGA image into `bgr24`, `bgra`, or `gray8`.
///
/// # Errors
/// [`Error::Unsupported`] for a colour-mapped, right-to-left, or otherwise
/// unhandled image, [`Error::InvalidData`] for a malformed header or packet,
/// [`Error::LimitExceeded`] if the declared dimensions exceed `budget`.
pub fn decode(data: &[u8], budget: &mut Budget) -> Result<Frame> {
    let mut r = Reader::new(data);
    let header = read_header(&mut r)?;
    r.bytes(usize::from(header.id_length))?;

    let pixel_bytes = usize::from(header.pixel_depth >> 3);
    let format = match header.pixel_depth {
        24 => PixFmt::Bgr24,
        32 => PixFmt::Bgra,
        _ => PixFmt::Gray8,
    };

    let mut frame = Frame::alloc_video(budget, format, header.width, header.height)?;
    let FrameData::Video { planes, .. } = &mut frame.data else {
        return Err(Error::InvalidData("tga: expected a video frame"));
    };
    let plane = planes
        .get_mut(0)
        .ok_or(Error::InvalidData("tga: no plane 0"))?;
    let stride = plane.stride;
    let buf = plane.data.make_mut();
    let row_bytes = (header.width as usize).saturating_mul(pixel_bytes);

    for file_row in 0..header.height {
        let out_row = dest_row(file_row, header.height, header.top_to_bottom) as usize;
        let dst_start = out_row.saturating_mul(stride);
        let dst = buf
            .get_mut(dst_start..dst_start.saturating_add(row_bytes))
            .ok_or(Error::InvalidData("tga: row out of bounds"))?;
        if header.rle {
            let mut filled = 0usize;
            while filled < row_bytes {
                let packet = r.u8()?;
                let count = usize::from(packet & 0x7F) + 1;
                if packet & 0x80 != 0 {
                    let px = r.bytes(pixel_bytes)?;
                    for _ in 0..count {
                        let end = filled.saturating_add(pixel_bytes);
                        dst.get_mut(filled..end)
                            .ok_or(Error::InvalidData("tga: rle overrun"))?
                            .copy_from_slice(px);
                        filled = end;
                    }
                } else {
                    let n = count.saturating_mul(pixel_bytes);
                    let px = r.bytes(n)?;
                    let end = filled.saturating_add(n);
                    dst.get_mut(filled..end)
                        .ok_or(Error::InvalidData("tga: raw packet overrun"))?
                        .copy_from_slice(px);
                    filled = end;
                }
            }
        } else {
            let src = r.bytes(row_bytes)?;
            dst.copy_from_slice(src);
        }
    }
    Ok(frame)
}

/// Encode a `bgr24`, `bgra`, or `gray8` frame as an uncompressed,
/// top-to-bottom TGA.
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
        return Err(Error::InvalidData("tga: expected a video frame"));
    };
    let (image_type, pixel_depth): (u8, u8) = match *format {
        PixFmt::Bgr24 => (2, 24),
        PixFmt::Bgra => (2, 32),
        PixFmt::Gray8 => (3, 8),
        _ => return Err(Error::Unsupported("tga: encoder needs bgr24, bgra or gray8 input")),
    };
    let (width, height) = (*width, *height);
    let plane = planes.first().ok_or(Error::InvalidData("tga: no plane 0"))?;
    let pixel_bytes = usize::from(pixel_depth >> 3);
    let row_bytes = (width as usize).saturating_mul(pixel_bytes);

    let mut out = vec![0u8; HEADER_LEN];
    if let Some(slot) = out.get_mut(2) {
        *slot = image_type;
    }
    out.get_mut(12..14)
        .ok_or(Error::InvalidData("tga: header"))?
        .copy_from_slice(&(width as u16).to_le_bytes());
    out.get_mut(14..16)
        .ok_or(Error::InvalidData("tga: header"))?
        .copy_from_slice(&(height as u16).to_le_bytes());
    if let Some(slot) = out.get_mut(16) {
        *slot = pixel_depth;
    }
    if let Some(slot) = out.get_mut(17) {
        *slot = TOP_TO_BOTTOM;
    }

    let src = plane.data.as_slice();
    for y in 0..height as usize {
        let start = y.saturating_mul(plane.stride);
        let row = src
            .get(start..start.saturating_add(row_bytes))
            .ok_or(Error::InvalidData("tga: row out of bounds"))?;
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

    #[test]
    fn round_trips_uncompressed_bgr24() {
        let mut budget = Budget::new(Limits::permissive());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Bgr24, 4, 3).expect("alloc");
        let FrameData::Video { planes, .. } = &mut frame.data else {
            panic!()
        };
        let stride = planes[0].stride;
        let buf = planes[0].data.make_mut();
        for y in 0..3 {
            for x in 0..4 {
                let off = y * stride + x * 3;
                buf[off] = (x * 10) as u8;
                buf[off + 1] = (y * 20) as u8;
                buf[off + 2] = 77;
            }
        }
        let encoded = encode(&frame).expect("encode");
        let mut budget2 = Budget::new(Limits::permissive());
        let decoded = decode(&encoded, &mut budget2).expect("decode");
        assert_eq!(encode(&decoded).unwrap(), encoded);
    }

    #[test]
    fn rle_and_raw_agree() {
        let mut budget = Budget::new(Limits::permissive());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Gray8, 6, 1).expect("alloc");
        let FrameData::Video { planes, .. } = &mut frame.data else {
            panic!()
        };
        let buf = planes[0].data.make_mut();
        buf[0..6].copy_from_slice(&[9, 9, 9, 1, 2, 2]);
        let raw = encode(&frame).expect("encode");

        // Hand-build the RLE equivalent: run of 3 nines, raw packet of one 1,
        // run of 2 twos.
        let mut rle = raw[..HEADER_LEN].to_vec();
        rle[2] = 11; // grayscale RLE
        rle.push(0x80 | 2); // run, count=3
        rle.push(9);
        rle.push(0x00); // raw, count=1
        rle.push(1);
        rle.push(0x80 | 1); // run, count=2
        rle.push(2);

        let mut b1 = Budget::new(Limits::permissive());
        let mut b2 = Budget::new(Limits::permissive());
        let f1 = decode(&raw, &mut b1).expect("raw decode");
        let f2 = decode(&rle, &mut b2).expect("rle decode");
        assert_eq!(encode(&f1).unwrap(), encode(&f2).unwrap());
    }

    #[test]
    fn rejects_colormapped() {
        let mut header = vec![0u8; HEADER_LEN];
        header[1] = 1; // colour_map_type
        header[2] = 1; // colormap image_type
        let mut budget = Budget::new(Limits::permissive());
        assert!(matches!(decode(&header, &mut budget), Err(Error::Unsupported(_))));
    }
}
