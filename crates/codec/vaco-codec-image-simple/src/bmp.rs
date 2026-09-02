//! BMP: a `BITMAPFILEHEADER` + `BITMAPINFOHEADER`, then rows padded to a
//! 4-byte boundary, bottom row first unless the header's height is negative.
//!
//! `Vaco-Spec-Ref: microsoft-bmp-format` — the Windows/OS2 bitmap layout, cross-
//! checked against the reference codec's observable byte behaviour (D17):
//! its native pixel format for 24bpp is `bgr24`, i.e. the file's B,G,R byte
//! order is kept rather than swapped to RGB. Only uncompressed `BI_RGB` is
//! supported; RLE4/RLE8/BITFIELDS are [`Error::Unsupported`].

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

use crate::reader::Reader;

const FILE_HEADER_LEN: usize = 14;

fn row_stride(width: u32, bpp: u32) -> usize {
    (((width as usize) * (bpp as usize)).div_ceil(8)).next_multiple_of(4)
}

struct Header {
    width: u32,
    height: u32,
    top_down: bool,
    bpp: u32,
    data_offset: usize,
    colors_used: u32,
    palette_start: usize,
}

fn read_header(r: &mut Reader<'_>) -> Result<Header> {
    let magic = r.bytes(2)?;
    if magic != b"BM" {
        return Err(Error::InvalidData("bmp: bad magic"));
    }
    r.bytes(8)?; // file size, reserved
    let data_offset = r.u32_le()? as usize;
    let dib_size = r.u32_le()?;
    if dib_size < 40 {
        return Err(Error::Unsupported(
            "bmp: only BITMAPINFOHEADER is supported",
        ));
    }
    let width = r.i32_le()?;
    let height = r.i32_le()?;
    let _planes = r.u16_le()?;
    let bpp = u32::from(r.u16_le()?);
    let compression = r.u32_le()?;
    if compression != 0 {
        return Err(Error::Unsupported("bmp: only BI_RGB is supported"));
    }
    r.bytes(12)?; // image size, x/y pixels per metre
    let colors_used = r.u32_le()?;
    r.u32_le()?; // colors important
    // The rest of a >40-byte DIB header (BITMAPV4/V5) is bitfield/colour-space
    // data this decoder does not use for BI_RGB; the palette (if any) follows
    // the header's own declared length, not a fixed 40-byte offset.
    let palette_start = FILE_HEADER_LEN + dib_size as usize;

    if width <= 0 || height == 0 {
        return Err(Error::InvalidData("bmp: invalid dimensions"));
    }
    let top_down = height < 0;
    let height = height.unsigned_abs();
    Ok(Header {
        width: width as u32,
        height,
        top_down,
        bpp,
        data_offset,
        colors_used,
        palette_start,
    })
}

fn read_palette(r: &mut Reader<'_>, count: usize) -> Result<Vec<[u8; 3]>> {
    let mut palette = Vec::new();
    for _ in 0..count {
        let entry = r.bytes(4)?;
        let &[b, g, red, _] = entry else {
            return Err(Error::UnexpectedEof);
        };
        palette.push([red, g, b]);
    }
    Ok(palette)
}

fn dest_row(image_row: u32, height: u32, top_down: bool) -> u32 {
    if top_down {
        image_row
    } else {
        height - 1 - image_row
    }
}

/// Decode a BMP image.
///
/// 24bpp decodes to `bgr24`, 32bpp to `bgra`, and 1/4/8bpp (paletted) expand
/// through the palette into `rgb24` — this crate carries no palette side-data
/// type, so a paletted BMP cannot round-trip back to a paletted BMP; see
/// `docs/codec/vaco-codec-image-simple.md`.
///
/// # Errors
/// [`Error::InvalidData`] for a malformed header, [`Error::Unsupported`] for
/// RLE/BITFIELDS compression or an unhandled bit depth,
/// [`Error::LimitExceeded`] if the declared dimensions exceed `budget`.
pub fn decode(data: &[u8], budget: &mut Budget) -> Result<Frame> {
    let mut r = Reader::new(data);
    let header = read_header(&mut r)?;
    let src_stride = row_stride(header.width, header.bpp);

    let paletted = matches!(header.bpp, 1 | 4 | 8);
    let format = match header.bpp {
        1 | 4 | 8 => PixFmt::Rgb24,
        24 => PixFmt::Bgr24,
        32 => PixFmt::Bgra,
        _ => return Err(Error::Unsupported("bmp: unsupported bit depth")),
    };
    let palette = if paletted {
        r.seek(header.palette_start)?;
        let count = if header.colors_used == 0 {
            1usize << header.bpp
        } else {
            header.colors_used as usize
        };
        read_palette(&mut r, count)?
    } else {
        Vec::new()
    };
    r.seek(header.data_offset)?;

    let mut frame = Frame::alloc_video(budget, format, header.width, header.height)?;
    let FrameData::Video { planes, .. } = &mut frame.data else {
        return Err(Error::InvalidData("bmp: expected a video frame"));
    };
    let plane = planes
        .get_mut(0)
        .ok_or(Error::InvalidData("bmp: no plane 0"))?;
    let stride = plane.stride;
    let buf = plane.data.make_mut();

    for file_row in 0..header.height {
        let row = r.bytes(src_stride)?;
        let out_row = dest_row(file_row, header.height, header.top_down) as usize;
        let dst_start = out_row.saturating_mul(stride);

        match header.bpp {
            24 | 32 => {
                let pixel_bytes = (header.bpp >> 3) as usize;
                let n = (header.width as usize).saturating_mul(pixel_bytes);
                let src = row.get(..n).ok_or(Error::UnexpectedEof)?;
                let dst = buf
                    .get_mut(dst_start..dst_start.saturating_add(n))
                    .ok_or(Error::InvalidData("bmp: row out of bounds"))?;
                dst.copy_from_slice(src);
            }
            8 => {
                for x in 0..header.width as usize {
                    let idx = row.get(x).copied().ok_or(Error::UnexpectedEof)? as usize;
                    let rgb = palette.get(idx).copied().unwrap_or([0, 0, 0]);
                    let off = dst_start.saturating_add(x * 3);
                    if let Some(dst) = buf.get_mut(off..off + 3) {
                        dst.copy_from_slice(&rgb);
                    }
                }
            }
            4 => {
                for x in 0..header.width as usize {
                    let byte = row.get(x >> 1).copied().ok_or(Error::UnexpectedEof)?;
                    let idx = if x % 2 == 0 { byte >> 4 } else { byte & 0x0F } as usize;
                    let rgb = palette.get(idx).copied().unwrap_or([0, 0, 0]);
                    let off = dst_start.saturating_add(x * 3);
                    if let Some(dst) = buf.get_mut(off..off + 3) {
                        dst.copy_from_slice(&rgb);
                    }
                }
            }
            1 => {
                for x in 0..header.width as usize {
                    let byte = row.get(x >> 3).copied().ok_or(Error::UnexpectedEof)?;
                    let bit = (byte >> (7 - (x % 8))) & 1;
                    let rgb = palette.get(bit as usize).copied().unwrap_or([0, 0, 0]);
                    let off = dst_start.saturating_add(x * 3);
                    if let Some(dst) = buf.get_mut(off..off + 3) {
                        dst.copy_from_slice(&rgb);
                    }
                }
            }
            _ => return Err(Error::Unsupported("bmp: unsupported bit depth")),
        }
    }
    Ok(frame)
}

/// Encode a `bgr24` or `bgra` frame as an uncompressed 24bpp/32bpp BMP,
/// bottom row first.
///
/// # Errors
/// [`Error::Unsupported`] for any other pixel format — this encoder mirrors
/// [`decode`]'s two truecolor cases and does not quantise a palette.
pub fn encode(frame: &Frame) -> Result<Vec<u8>> {
    let FrameData::Video {
        format,
        width,
        height,
        planes,
    } = &frame.data
    else {
        return Err(Error::InvalidData("bmp: expected a video frame"));
    };
    let bpp: u32 = match *format {
        PixFmt::Bgr24 => 24,
        PixFmt::Bgra => 32,
        _ => return Err(Error::Unsupported("bmp: encoder needs bgr24 or bgra input")),
    };
    let (width, height) = (*width, *height);
    let plane = planes
        .first()
        .ok_or(Error::InvalidData("bmp: no plane 0"))?;
    let pixel_bytes = (bpp >> 3) as usize;
    let dst_stride = row_stride(width, bpp);
    let pixel_data_len = dst_stride * height as usize;

    let mut out = Vec::new();
    out.extend_from_slice(b"BM");
    let file_size = (FILE_HEADER_LEN + 40 + pixel_data_len) as u32;
    out.extend_from_slice(&file_size.to_le_bytes());
    out.extend_from_slice(&[0u8; 4]);
    out.extend_from_slice(&((FILE_HEADER_LEN + 40) as u32).to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&width.cast_signed().to_le_bytes());
    out.extend_from_slice(&height.cast_signed().to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(bpp as u16).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&(pixel_data_len as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 16]); // ppm x2, colors used, colors important

    let src = plane.data.as_slice();
    let row_len = (width as usize).saturating_mul(pixel_bytes);
    let pad = dst_stride - row_len;
    for file_row in 0..height {
        let image_row = height - 1 - file_row; // bottom row first
        let start = (image_row as usize).saturating_mul(plane.stride);
        let row = src
            .get(start..start.saturating_add(row_len))
            .ok_or(Error::InvalidData("bmp: row out of bounds"))?;
        out.extend_from_slice(row);
        out.extend(std::iter::repeat_n(0u8, pad));
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

    fn tiny_bgr24() -> Vec<u8> {
        // 2x2, bottom-up, rows padded to 4 bytes (2*3=6 -> pad to 8).
        let mut data = Vec::new();
        data.extend_from_slice(b"BM");
        data.extend_from_slice(&70u32.to_le_bytes());
        data.extend_from_slice(&[0u8; 4]);
        data.extend_from_slice(&54u32.to_le_bytes());
        data.extend_from_slice(&40u32.to_le_bytes());
        data.extend_from_slice(&2i32.to_le_bytes());
        data.extend_from_slice(&2i32.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&24u16.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&16u32.to_le_bytes());
        data.extend_from_slice(&[0u8; 16]);
        // bottom row then top row, each padded to 8 bytes
        data.extend_from_slice(&[1, 2, 3, 4, 5, 6, 0, 0]);
        data.extend_from_slice(&[7, 8, 9, 10, 11, 12, 0, 0]);
        data
    }

    #[test]
    fn round_trips_24bpp() {
        let raw = tiny_bgr24();
        let mut budget = Budget::new(Limits::permissive());
        let frame = decode(&raw, &mut budget).expect("decode");
        assert_eq!(encode(&frame).expect("encode"), raw);
    }

    #[test]
    fn top_down_matches_bottom_up_flipped() {
        let mut raw = tiny_bgr24();
        // Flip to top-down (negative height) and swap the two rows.
        let height_off = 14 + 4 + 4;
        raw[height_off..height_off + 4].copy_from_slice(&(-2i32).to_le_bytes());
        let (a, b) = raw.split_at_mut(54 + 8);
        let (row0, row1) = (a[54..62].to_vec(), b[..8].to_vec());
        raw[54..62].copy_from_slice(&row1);
        raw[62..70].copy_from_slice(&row0);

        let bottom_up = tiny_bgr24();
        let mut b1 = Budget::new(Limits::permissive());
        let mut b2 = Budget::new(Limits::permissive());
        let f1 = decode(&bottom_up, &mut b1).expect("bottom-up");
        let f2 = decode(&raw, &mut b2).expect("top-down");
        assert_eq!(encode(&f1).unwrap(), encode(&f2).unwrap());
    }

    #[test]
    fn rejects_rle_compression() {
        let mut raw = tiny_bgr24();
        raw[30..34].copy_from_slice(&1u32.to_le_bytes()); // BI_RLE8
        let mut budget = Budget::new(Limits::permissive());
        assert!(matches!(
            decode(&raw, &mut budget),
            Err(Error::Unsupported(_))
        ));
    }
}
