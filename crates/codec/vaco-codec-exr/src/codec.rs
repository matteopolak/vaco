//! The byte format: `OpenEXR`, wrapping the `exr` crate.
//!
//! [`decode`] and [`encode`] are pure functions over bytes and [`Frame`]s;
//! the `SendReceive` wrappers in `lib.rs` never touch an `exr::` type.

use std::{cell::RefCell, io::Cursor};

use exr::prelude::*;
use exr::{meta::header::Header, prelude::MetaData};
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

fn check_scope(meta: &MetaData) -> Result<&Header> {
    if meta.requirements.has_multiple_layers || meta.headers.len() != 1 {
        return Err(Error::Unsupported("exr: multipart image"));
    }
    let header = meta
        .headers
        .first()
        .ok_or(Error::InvalidData("exr: missing header"))?;
    if meta.requirements.has_deep_data || header.deep {
        return Err(Error::Unsupported("exr: deep data"));
    }

    let channels = &header.channels.list;
    let has = |name: &str| channels.iter().any(|channel| channel.name.eq(name));
    let rgba_shape = matches!(channels.len(), 3 | 4)
        && has("R")
        && has("G")
        && has("B")
        && (channels.len() == 3 || has("A"));
    if !rgba_shape {
        return Err(Error::Unsupported("exr: non-RGB channel layout"));
    }
    if channels
        .iter()
        .any(|channel| channel.sampling != Vec2(1, 1))
    {
        return Err(Error::Unsupported("exr: subsampled channels"));
    }
    if matches!(
        header.compression,
        Compression::HTJ2K32 | Compression::HTJ2K256
    ) {
        return Err(Error::Unsupported("exr: HTJ2K compression"));
    }
    Ok(header)
}

/// Decode a single RGB(A) layer of an `OpenEXR` image into one [`Frame`].
///
/// The supported shape is exactly `R`, `G`, `B`, and optional `A`, with no
/// channel subsampling. Scan-line and tiled images are accepted; for a tiled
/// image with multiple resolution levels, only the largest level is decoded.
/// Deep data, multipart files, and arbitrary channel layouts are reported as
/// [`Error::Unsupported`] rather than guessed at. Compression
/// (PIZ/ZIP/RLE/PXR24/B44/DWA) is transparent: the `exr` crate decodes
/// whichever method the file declares.
///
/// # Errors
///
/// [`Error::InvalidData`] for a malformed header or corrupt block.
/// [`Error::Unsupported`] for a channel/layout feature outside that scope.
/// [`Error::LimitExceeded`] when the image or its RGBA staging buffer exceeds
/// `budget`, including when their size arithmetic overflows.
pub fn decode(bytes: &[u8], budget: &mut Budget) -> Result<Frame> {
    let chunks = exr::block::read(Cursor::new(bytes), false).map_err(|e| map_err(&e))?;
    let header = check_scope(chunks.meta_data())?;
    let width = u32::try_from(header.layer_size.width()).map_err(|_| Error::LimitExceeded {
        limit: "max_dimension (width)",
        requested: u64::MAX,
        cap: u64::from(budget.limits().max_dimension),
    })?;
    let height = u32::try_from(header.layer_size.height()).map_err(|_| Error::LimitExceeded {
        limit: "max_dimension (height)",
        requested: u64::MAX,
        cap: u64::from(budget.limits().max_dimension),
    })?;
    budget.check_frame(width, height, 16)?;
    let rgba_len = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|area| area.checked_mul(4))
        .ok_or(Error::LimitExceeded {
            limit: "size_computation",
            requested: u64::MAX,
            cap: u64::MAX,
        })?;
    let rgba = RefCell::new(Some(budget.alloc::<f32>(rgba_len)?));

    let image = read()
        .no_deep_data()
        .largest_resolution_level()
        .rgba_channels(
            |size: Vec2<usize>, _channels: &RgbaChannels| RgbaBuf {
                width: size.width(),
                data: rgba.borrow_mut().take().unwrap_or_default(),
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
        .from_chunks(chunks)
        .map_err(|e| map_err(&e))?;

    let format = native32(PixFmt::Rgbaf32le, PixFmt::Rgbaf32be);
    let src = &image.layer_data.channel_data.pixels.data;
    if src.len() != rgba_len {
        return Err(Error::InvalidData("exr: decoded pixel count"));
    }
    let mut frame = Frame::alloc_video(budget, format, width, height)?;
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

/// The stream description an `OpenEXR` header states, without decoding a
/// pixel.
///
/// Reads and validates only the header table, never a scanline or tile — this
/// crate's `vaco-parse-image` caller (`still::Exr`) is a "no decode" parser by
/// contract. The same [`check_scope`] gate used by [`decode`] rejects deep,
/// multipart, non-RGB(A), subsampled, and unsupported-compression inputs, so
/// parameters are never advertised for a stream this codec will refuse.
/// [`decode`] always produces [`native32`]`(Rgbaf32le, Rgbaf32be)` regardless
/// of the source channel sample type, so that format is reported
/// unconditionally here too.
#[must_use]
pub fn parameters(data: &[u8]) -> Option<vaco_codec_core::CodecParameters> {
    let reader = exr::block::read(Cursor::new(data), false).ok()?;
    let header = check_scope(reader.meta_data()).ok()?;
    let width = u32::try_from(header.layer_size.width()).ok()?;
    let height = u32::try_from(header.layer_size.height()).ok()?;

    let mut params =
        vaco_codec_core::CodecParameters::video().with_codec(vaco_codec_core::CodecId::Exr);
    if let Some(v) = params.video.as_mut() {
        v.width = width;
        v.height = height;
        v.coded_width = width;
        v.coded_height = height;
        v.format = Some(native32(PixFmt::Rgbaf32le, PixFmt::Rgbaf32be));
    }
    Some(params)
}

fn map_err(e: &exr::error::Error) -> Error {
    match e {
        exr::error::Error::Io(_) => Error::UnexpectedEof,
        exr::error::Error::NotSupported(_) => Error::Unsupported("exr: unsupported layout"),
        exr::error::Error::Invalid(_) | exr::error::Error::Aborted => {
            Error::InvalidData("exr: malformed stream")
        }
    }
}

/// Read one frame's plane as a flat `f32` RGBA buffer, upconverting 8-bit
/// integer formats and filling alpha as `1.0` for the three-channel ones.
fn frame_to_rgba_f32(frame: &Frame) -> Result<(usize, usize, Vec<f32>)> {
    let FrameData::Video {
        format,
        width,
        height,
        ..
    } = &frame.data
    else {
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
                    if let (Some(dst), Ok(bytes)) =
                        (out.get_mut(dst_start + i), <[u8; 4]>::try_from(chunk))
                    {
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
                        if let (Some(dst), Ok(b)) =
                            (out.get_mut(dst_start + c), <[u8; 4]>::try_from(bytes4))
                        {
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
                    if let (Some(dst), &[r, g, b, a]) =
                        (out.get_mut(dst_start..dst_start + 4), chunk)
                    {
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
                    if let (Some(dst), &[r, g, b]) = (out.get_mut(dst_start..dst_start + 4), chunk)
                    {
                        dst.copy_from_slice(&[
                            f32::from(r) / 255.0,
                            f32::from(g) / 255.0,
                            f32::from(b) / 255.0,
                            1.0,
                        ]);
                    }
                }
            }
        }
        _ => return Err(Error::Unsupported("exr: encode pixel format")),
    }
    Ok((width, height, out))
}

/// `-compression`'s four values (`ffmpeg -h encoder=exr`), kept independent
/// of the D11 boundary's `exr::compression::Compression` -- that mapping
/// lives in [`encode`] alone. Named `CompressionAlgo` rather than
/// `Compression` so it cannot be confused with the `exr` crate's own type
/// even via a glob import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionAlgo {
    /// `0`/`none`: no compression.
    None,
    /// `1`/`rle`: run-length encoding.
    Rle,
    /// `2`/`zip1`: ZIP, one scanline per block.
    Zip1,
    /// `3`/`zip16`: ZIP, 16 scanlines per block.
    Zip16,
}

/// Encoder knobs mirroring `ffmpeg exr`'s own `AVOption`s, measured against
/// `ffmpeg -h encoder=exr`. `None` leaves this crate's existing default
/// (`Encoding::default()`, RLE with 64x64 tiles) untouched rather than
/// forcing a choice nobody asked for; `ffmpeg`'s own default is `none`
/// (uncompressed), which only matters for file size since every one of
/// these four is lossless.
#[derive(Debug, Clone, Copy, Default)]
pub struct EncodeOptions {
    /// `-compression`.
    pub compression: Option<CompressionAlgo>,
}

/// Encode one frame as an `OpenEXR` RGBA image (`f32` channels).
///
/// Fidelity is D11 "Exact for ZIP/RLE/PIZ": the `f32` transform is a
/// straight copy for a source already carrying `f32` samples, and an
/// upconversion (`/255`) for an 8-bit source, which is inherently lossy in
/// the reverse direction — this crate never claims a round trip through an
/// 8-bit format is exact. All four `-compression` choices are themselves
/// lossless, so which one is picked changes only the byte count.
///
/// # Errors
///
/// [`Error::Unsupported`] for a non-video frame or a pixel format
/// [`frame_to_rgba_f32`] does not cover. [`Error::InvalidData`] on encoder
/// failure.
pub fn encode(frame: &Frame, options: &EncodeOptions) -> Result<Vec<u8>> {
    let (width, height, data) = frame_to_rgba_f32(frame)?;
    let channels = SpecificChannels::rgba(move |Vec2(x, y): Vec2<usize>| {
        let idx = (y * width + x) * 4;
        let get = |i: usize| data.get(idx + i).copied().unwrap_or(0.0);
        (get(0), get(1), get(2), get(3))
    });
    let mut encoding = Encoding::default();
    if let Some(algo) = options.compression {
        encoding.compression = match algo {
            CompressionAlgo::None => Compression::Uncompressed,
            CompressionAlgo::Rle => Compression::RLE,
            CompressionAlgo::Zip1 => Compression::ZIP1,
            CompressionAlgo::Zip16 => Compression::ZIP16,
        };
    }
    let image = Image::from_encoded_channels((width, height), encoding, channels);

    let mut out: Vec<u8> = Vec::new();
    {
        let mut cursor = Cursor::new(&mut out);
        image
            .write()
            .to_buffered(&mut cursor)
            .map_err(|_| Error::InvalidData("exr: encode"))?;
    }
    Ok(out)
}
