//! XWD (X Window Dump): a 25-field, all-big-endian-`u32` `XWDFileHeader`,
//! a null-terminated window name, then `ZPixmap` rows padded to
//! `bitmap_pad` bits.
//!
//! `Vaco-Spec-Ref: x11-xwd-header` — `X11/XWDFile.h`'s `XWDFileHeader`
//! layout, cross-checked against the reference codec's observable byte
//! behaviour (D17): it always writes 24-bit `ZPixmap`, `MSBFirst`, `bgr`
//! byte order (native pixel format `rgb24` — measured to be a genuine
//! `R`,`G`,`B` byte order, not `bgr24` as the mask fields alone might
//! suggest) with 32-bit row padding. Other depths/formats are
//! [`Error::Unsupported`]. The encoder does not reproduce the reference's
//! embedded window-name string (`lavcxwdenc`, an implementation detail, not
//! part of the format); it writes an empty name instead.

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

use crate::reader::Reader;

const FIELD_COUNT: usize = 25;
const FIXED_HEADER_LEN: usize = FIELD_COUNT * 4;

struct Header {
    data_offset: usize,
    width: u32,
    height: u32,
    bytes_per_line: usize,
}

fn read_header(r: &mut Reader<'_>) -> Result<Header> {
    let data_offset = r.u32_be()? as usize;
    let _file_version = r.u32_be()?;
    let pixmap_format = r.u32_be()?;
    let pixmap_depth = r.u32_be()?;
    let width = r.u32_be()?;
    let height = r.u32_be()?;
    r.u32_be()?; // xoffset
    let byte_order = r.u32_be()?;
    r.u32_be()?; // bitmap_unit
    r.u32_be()?; // bitmap_bit_order
    let bitmap_pad = r.u32_be()?;
    let bits_per_pixel = r.u32_be()?;
    let bytes_per_line = r.u32_be()? as usize;
    // visual_class, masks, bits_per_rgb, colormap_entries, ncolors: skip.
    r.bytes(4 * 7)?;
    let window_width = r.u32_be()?;
    let window_height = r.u32_be()?;
    r.bytes(4 * 3)?; // window_x, window_y, window_border_width

    if pixmap_format != 2 || bits_per_pixel != 24 || pixmap_depth != 24 {
        return Err(Error::Unsupported("xwd: only 24bpp ZPixmap is supported"));
    }
    if byte_order != 1 || bitmap_pad != 32 {
        return Err(Error::Unsupported(
            "xwd: only MSBFirst, 32-bit-padded rows are supported",
        ));
    }
    if width == 0 || height == 0 || width != window_width || height != window_height {
        return Err(Error::InvalidData("xwd: invalid dimensions"));
    }
    if data_offset < FIXED_HEADER_LEN {
        return Err(Error::InvalidData("xwd: data_offset too small"));
    }
    Ok(Header {
        data_offset,
        width,
        height,
        bytes_per_line,
    })
}

/// The stream description XWD's own `XWDFileHeader` states, without decoding
/// a pixel. See [`crate::video_parameters`].
#[must_use]
pub fn parameters(data: &[u8]) -> Option<vaco_codec_core::CodecParameters> {
    let header = read_header(&mut Reader::new(data)).ok()?;
    Some(crate::video_parameters(
        vaco_codec_core::CodecId::Xwd,
        header.width,
        header.height,
        PixFmt::Rgb24,
    ))
}

/// Decode an XWD image into `rgb24`.
///
/// # Errors
/// [`Error::Unsupported`] for any depth/format other than 24bpp `ZPixmap`,
/// [`Error::InvalidData`] for a malformed header,
/// [`Error::LimitExceeded`] if the declared dimensions exceed `budget`.
pub fn decode(data: &[u8], budget: &mut Budget) -> Result<Frame> {
    let mut r = Reader::new(data);
    let header = read_header(&mut r)?;
    r.seek(header.data_offset)?;

    let mut frame = Frame::alloc_video(budget, PixFmt::Rgb24, header.width, header.height)?;
    let FrameData::Video { planes, .. } = &mut frame.data else {
        return Err(Error::InvalidData("xwd: expected a video frame"));
    };
    let plane = planes
        .get_mut(0)
        .ok_or(Error::InvalidData("xwd: no plane 0"))?;
    let stride = plane.stride;
    let buf = plane.data.make_mut();
    let row_bytes = (header.width as usize).saturating_mul(3);

    for y in 0..header.height as usize {
        let row = r.bytes(header.bytes_per_line)?;
        let src = row.get(..row_bytes).ok_or(Error::UnexpectedEof)?;
        let start = y.saturating_mul(stride);
        let dst = buf
            .get_mut(start..start.saturating_add(row_bytes))
            .ok_or(Error::InvalidData("xwd: row out of bounds"))?;
        dst.copy_from_slice(src);
    }
    Ok(frame)
}

/// Encode an `rgb24` frame as a 24bpp `ZPixmap` XWD image with an empty
/// window name.
///
/// Not byte-identical to the reference encoder, which embeds its own name
/// (`lavcxwdenc`) as the window-name string: that is an ffmpeg-specific
/// value, not part of the XWD format, so this writes an empty name instead
/// — the pixel data and every header field besides `header_size` and the
/// name bytes still match exactly.
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
        return Err(Error::InvalidData("xwd: expected a video frame"));
    };
    if *format != PixFmt::Rgb24 {
        return Err(Error::Unsupported("xwd: encoder needs rgb24 input"));
    }
    let (width, height) = (*width, *height);
    let plane = planes
        .first()
        .ok_or(Error::InvalidData("xwd: no plane 0"))?;
    let bytes_per_line = ((width as usize) * 3).next_multiple_of(4);
    let data_offset = FIXED_HEADER_LEN + 1; // one NUL byte, no name text

    let fields: [u32; FIELD_COUNT] = [
        data_offset as u32,
        7,  // file_version
        2,  // pixmap_format: ZPixmap
        24, // pixmap_depth
        width,
        height,
        0,  // xoffset
        1,  // byte_order: MSBFirst
        32, // bitmap_unit
        0,  // bitmap_bit_order
        32, // bitmap_pad
        24, // bits_per_pixel
        bytes_per_line as u32,
        4, // visual_class: TrueColor
        0x00FF_0000,
        0x0000_FF00,
        0x0000_00FF,
        8, // bits_per_rgb
        0, // colormap_entries
        0, // ncolors
        width,
        height,
        0, // window_x
        0, // window_y
        0, // window_border_width
    ];
    let mut out = Vec::new();
    for field in fields {
        out.extend_from_slice(&field.to_be_bytes());
    }
    out.push(0); // empty, NUL-terminated window name

    let src = plane.data.as_slice();
    let row_len = (width as usize).saturating_mul(3);
    let pad = bytes_per_line - row_len;
    for y in 0..height as usize {
        let start = y.saturating_mul(plane.stride);
        let row = src
            .get(start..start.saturating_add(row_len))
            .ok_or(Error::InvalidData("xwd: row out of bounds"))?;
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

    #[test]
    fn round_trips_rgb24() {
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
                buf[off] = (x * 10) as u8;
                buf[off + 1] = (y * 30) as u8;
                buf[off + 2] = 99;
            }
        }
        let encoded = encode(&frame).expect("encode");
        let mut budget2 = Budget::new(Limits::permissive());
        let decoded = decode(&encoded, &mut budget2).expect("decode");
        assert_eq!(encode(&decoded).unwrap(), encoded);
    }

    #[test]
    fn rejects_wrong_depth() {
        let mut header = [0u8; FIXED_HEADER_LEN];
        header[3 * 4..4 * 4].copy_from_slice(&8u32.to_be_bytes()); // pixmap_depth
        let mut budget = Budget::new(Limits::permissive());
        assert!(matches!(
            decode(&header, &mut budget),
            Err(Error::Unsupported(_))
        ));
    }
}
