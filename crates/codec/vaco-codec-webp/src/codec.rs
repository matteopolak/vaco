//! The byte format: WebP, wrapping the `image-webp` crate.
//!
//! [`decode`] and [`encode`] are pure functions over bytes and [`Frame`]s;
//! the `SendReceive` wrappers in `lib.rs` never touch an `image_webp::` type.

use std::io::Cursor;

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData, FrameFlags};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

/// Decode one WebP packet into every frame it carries.
///
/// A still image yields exactly one frame. An animated (`VP8X`/`ANMF`)
/// WebP yields one frame per `ANMF` chunk — `image_webp::WebPDecoder`
/// already composites each frame's dispose/blend onto the canvas
/// internally (unlike GIF/APNG, which this crate composites itself), so
/// `read_frame` is called in a plain loop.
///
/// # Errors
///
/// [`Error::InvalidData`] for a malformed RIFF/VP8(L/X) stream.
/// [`Error::UnexpectedEof`] for a truncated stream. [`Error::LimitExceeded`]
/// when the canvas exceeds `budget`.
pub fn decode(bytes: &[u8], budget: &mut Budget) -> Result<Vec<Frame>> {
    let mut decoder =
        image_webp::WebPDecoder::new(Cursor::new(bytes)).map_err(|_| Error::InvalidData("webp: header"))?;
    let (width, height) = decoder.dimensions();
    let has_alpha = decoder.has_alpha();
    let bpp = if has_alpha { 4 } else { 3 };
    budget.check_frame(width, height, bpp)?;
    let format = if has_alpha { PixFmt::Rgba } else { PixFmt::Rgb24 };

    let Some(buf_len) = decoder.output_buffer_size() else {
        return Err(Error::Unsupported("webp: image too large"));
    };

    let mut out = Vec::new();
    if !decoder.is_animated() {
        let mut buf: Vec<u8> = budget.alloc(buf_len)?;
        decoder.read_image(&mut buf).map_err(|_| Error::InvalidData("webp: image data"))?;
        let frame = frame_from_packed(budget, format, width, height, bpp as usize, &buf)?;
        out.push(frame);
        return Ok(out);
    }

    loop {
        let mut buf: Vec<u8> = budget.alloc(buf_len)?;
        let delay_ms = match decoder.read_frame(&mut buf) {
            Ok(delay) => delay,
            Err(_) if !out.is_empty() => break,
            Err(_) => return Err(Error::InvalidData("webp: frame data")),
        };
        let mut frame = frame_from_packed(budget, format, width, height, bpp as usize, &buf)?;
        // WebP's ANMF frame duration is in milliseconds.
        frame.time_base = vaco_core::Rational::new(1, 1000);
        frame.duration = vaco_core::Duration(i64::from(delay_ms));
        out.push(frame);
        if out.len() as u32 >= decoder.num_frames() {
            break;
        }
    }
    if out.is_empty() {
        return Err(Error::InvalidData("webp: no image data"));
    }
    Ok(out)
}

fn frame_from_packed(
    budget: &mut Budget,
    format: PixFmt,
    width: u32,
    height: u32,
    bpp: usize,
    packed: &[u8],
) -> Result<Frame> {
    let mut frame = Frame::alloc_video(budget, format, width, height)?;
    let row_bytes = width as usize * bpp;
    for mut plane in frame.planes_mut() {
        for row in 0..plane.rows() {
            let src_start = row * row_bytes;
            let Some(src) = packed.get(src_start..src_start + row_bytes) else {
                break;
            };
            if let Some(dst) = plane.row_mut(row) {
                let n = dst.len().min(src.len());
                if let (Some(d), Some(s)) = (dst.get_mut(..n), src.get(..n)) {
                    d.copy_from_slice(s);
                }
            }
        }
    }
    frame.flags = FrameFlags::KEY;
    Ok(frame)
}

/// Encode a single frame as a lossless (`VP8L`) WebP image.
///
/// `image-webp`'s encoder supports lossless still images only — no lossy
/// (`VP8`) path and no animation (`ANMF`) encode. Fidelity is D11 "Exact":
/// lossless WebP is an integer-exact transform, so every pixel this crate
/// supports round-trips exactly.
///
/// # Errors
///
/// [`Error::Unsupported`] for a non-video frame or a pixel format this
/// crate does not map to one of `image-webp`'s four lossless colour types
/// (`L8`/`La8`/`Rgb8`/`Rgba8`). [`Error::InvalidData`] on encoder failure.
pub fn encode(frame: &Frame) -> Result<Vec<u8>> {
    let FrameData::Video { format, width, height, .. } = &frame.data else {
        return Err(Error::Unsupported("webp: audio frame"));
    };
    let (width, height) = (*width, *height);
    let (color, bpp) = match format {
        PixFmt::Gray8 => (image_webp::ColorType::L8, 1),
        PixFmt::Ya8 => (image_webp::ColorType::La8, 2),
        PixFmt::Rgb24 => (image_webp::ColorType::Rgb8, 3),
        PixFmt::Rgba => (image_webp::ColorType::Rgba8, 4),
        _ => return Err(Error::Unsupported("webp: encode pixel format")),
    };
    let plane = frame.plane(0).ok_or(Error::InvalidData("webp: no plane"))?;
    let row_bytes = width as usize * bpp;
    let mut packed = vec![0u8; row_bytes * height as usize];
    for (row_idx, row) in plane.rows_iter().take(height as usize).enumerate() {
        if let Some(dst) = packed.get_mut(row_idx * row_bytes..(row_idx + 1) * row_bytes) {
            let n = dst.len().min(row.len());
            if let (Some(d), Some(s)) = (dst.get_mut(..n), row.get(..n)) {
                d.copy_from_slice(s);
            }
        }
    }

    let mut out: Vec<u8> = Vec::new();
    image_webp::WebPEncoder::new(&mut out)
        .encode(&packed, width, height, color)
        .map_err(|_| Error::InvalidData("webp: encode"))?;
    Ok(out)
}
