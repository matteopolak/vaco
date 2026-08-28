//! The byte format: `OpenEXR`, wrapping the `exr` crate.
//!
//! [`decode`] and [`encode`] are pure functions over bytes and [`Frame`]s;
//! the `SendReceive` wrappers in `lib.rs` never touch an `exr::` type.

use std::io::Cursor;

use exr::prelude::*;
use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData, FrameFlags};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

const fn native32(le: PixFmt, be: PixFmt) -> PixFmt {
    if cfg!(target_endian = "big") { be } else { le }
}

/// The read-side pixel storage: a flat, row-major `f32` RGBA buffer plus the
/// width needed to convert `(x, y)` into an index (the `exr` crate's
/// `set_pixel` callback receives a position but not the buffer's own
/// stride).
struct RgbaBuf {
    width: usize,
    data: Vec<f32>,
}

/// Decode the first RGB(A) layer of an `OpenEXR` image into one [`Frame`].
///
/// Only the RGB(A) channel shape is supported — deep data, multi-part
/// files and arbitrary/multi-channel (non-colour) layouts are a documented
/// coverage gap (plan 15 §4A.2's `OpenEXR` risk note), reported as
/// [`Error::Unsupported`] rather than guessed at. Compression (PIZ/ZIP/RLE/
/// PXR24/B44/DWA) is transparent: the `exr` crate decodes whichever method
/// the file declares.
///
/// # Errors
///
/// [`Error::InvalidData`] for a malformed header or corrupt block.
/// [`Error::Unsupported`] for a layer with no RGB(A) channels, deep data, or
/// dimensions the `exr` crate itself refuses. [`Error::LimitExceeded`] when
/// the image exceeds `budget`.
pub fn decode(bytes: &[u8], budget: &mut Budget) -> Result<Frame> {
    let image = read()
        .no_deep_data()
        .largest_resolution_level()
        .rgba_channels(
            |size: Vec2<usize>, _channels: &RgbaChannels| RgbaBuf {
                width: size.width(),
                data: vec![0.0f32; size.area() * 4],
            },
            |buf: &mut RgbaBuf, pos: Vec2<usize>, (r, g, b, a): (f32, f32, f32, f32)| {
                let idx = (pos.y() * buf.width + pos.x()) * 4;
                if let Some(px) = buf.data.get_mut(idx..idx + 4) {
                    px.copy_from_slice(&[r, g, b, a]);
                }
            },
        )
        .first_valid_layer()
        .all_attributes()
        .from_buffered(Cursor::new(bytes))
        .map_err(|e| map_err(&e))?;

    let width = image.layer_data.size.width() as u32;
    let height = image.layer_data.size.height() as u32;
    budget.check_frame(width, height, 16)?;

    let format = native32(PixFmt::Rgbaf32le, PixFmt::Rgbaf32be);
    let mut frame = Frame::alloc_video(budget, format, width, height)?;
    let src = &image.layer_data.channel_data.pixels.data;
    for mut plane in frame.planes_mut() {
        for row in 0..plane.rows() {
            let row_floats = width as usize * 4;
            let src_start = row * row_floats;
            let Some(src_row) = src.get(src_start..src_start + row_floats) else {
                break;
            };
            if let Some(dst) = plane.row_mut(row) {
                for (d, &s) in dst.chunks_exact_mut(4).zip(src_row.iter()) {
                    d.copy_from_slice(&s.to_ne_bytes());
                }
            }
        }
    }
    frame.flags = FrameFlags::KEY;
    Ok(frame)
}

fn map_err(e: &exr::error::Error) -> Error {
    match e {
        exr::error::Error::Io(_) => Error::UnexpectedEof,
        exr::error::Error::NotSupported(_) => Error::Unsupported("exr: unsupported layout"),
        exr::error::Error::Invalid(_) | exr::error::Error::Aborted => Error::InvalidData("exr: malformed stream"),
    }
}

/// Read one frame's plane as a flat `f32` RGBA buffer, upconverting 8-bit
/// integer formats and filling alpha as `1.0` for the three-channel ones.
fn frame_to_rgba_f32(frame: &Frame) -> Result<(usize, usize, Vec<f32>)> {
    let FrameData::Video { format, width, height, .. } = &frame.data else {
        return Err(Error::Unsupported("exr: audio frame"));
    };
    let (width, height) = (*width as usize, *height as usize);
    let plane = frame.plane(0).ok_or(Error::InvalidData("exr: no plane"))?;
    let mut out = vec![0f32; width * height * 4];

    match format {
        PixFmt::Rgbaf32le | PixFmt::Rgbaf32be => {
            for (row_idx, row) in plane.rows_iter().take(height).enumerate() {
                let dst_start = row_idx * width * 4;
                for (i, chunk) in row.chunks_exact(4).enumerate() {
                    if let (Some(dst), Ok(bytes)) = (out.get_mut(dst_start + i), <[u8; 4]>::try_from(chunk)) {
                        *dst = f32::from_ne_bytes(bytes);
                    }
                }
            }
        }
        PixFmt::Rgbf32le | PixFmt::Rgbf32be => {
            for (row_idx, row) in plane.rows_iter().take(height).enumerate() {
                for (px, chunk) in row.chunks_exact(12).enumerate() {
                    let dst_start = (row_idx * width + px) * 4;
                    for (c, bytes4) in chunk.chunks_exact(4).enumerate() {
                        if let (Some(dst), Ok(b)) = (out.get_mut(dst_start + c), <[u8; 4]>::try_from(bytes4)) {
                            *dst = f32::from_ne_bytes(b);
                        }
                    }
                    if let Some(a) = out.get_mut(dst_start + 3) {
                        *a = 1.0;
                    }
                }
            }
        }
        PixFmt::Rgba => {
            for (row_idx, row) in plane.rows_iter().take(height).enumerate() {
                for (px, chunk) in row.chunks_exact(4).enumerate() {
                    let dst_start = (row_idx * width + px) * 4;
                    if let (Some(dst), &[r, g, b, a]) = (out.get_mut(dst_start..dst_start + 4), chunk) {
                        dst.copy_from_slice(&[
                            f32::from(r) / 255.0,
                            f32::from(g) / 255.0,
                            f32::from(b) / 255.0,
                            f32::from(a) / 255.0,
                        ]);
                    }
                }
            }
        }
        PixFmt::Rgb24 => {
            for (row_idx, row) in plane.rows_iter().take(height).enumerate() {
                for (px, chunk) in row.chunks_exact(3).enumerate() {
                    let dst_start = (row_idx * width + px) * 4;
                    if let (Some(dst), &[r, g, b]) = (out.get_mut(dst_start..dst_start + 4), chunk) {
                        dst.copy_from_slice(&[f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0, 1.0]);
                    }
                }
            }
        }
        _ => return Err(Error::Unsupported("exr: encode pixel format")),
    }
    Ok((width, height, out))
}

/// Encode one frame as an `OpenEXR` RGBA image (`f32` channels, the `exr`
/// crate's own default compression).
///
/// Fidelity is D11 "Exact for ZIP/RLE/PIZ": the `f32` transform is a
/// straight copy for a source already carrying `f32` samples, and an
/// upconversion (`/255`) for an 8-bit source, which is inherently lossy in
/// the reverse direction — this crate never claims a round trip through an
/// 8-bit format is exact.
///
/// # Errors
///
/// [`Error::Unsupported`] for a non-video frame or a pixel format
/// [`frame_to_rgba_f32`] does not cover. [`Error::InvalidData`] on encoder
/// failure.
pub fn encode(frame: &Frame) -> Result<Vec<u8>> {
    let (width, height, data) = frame_to_rgba_f32(frame)?;
    let channels = SpecificChannels::rgba(move |Vec2(x, y): Vec2<usize>| {
        let idx = (y * width + x) * 4;
        let get = |i: usize| data.get(idx + i).copied().unwrap_or(0.0);
        (get(0), get(1), get(2), get(3))
    });
    let image = Image::from_channels((width, height), channels);

    let mut out: Vec<u8> = Vec::new();
    {
        let mut cursor = Cursor::new(&mut out);
        image.write().to_buffered(&mut cursor).map_err(|_| Error::InvalidData("exr: encode"))?;
    }
    Ok(out)
}
