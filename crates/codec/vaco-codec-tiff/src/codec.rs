//! The byte format: TIFF, wrapping the `tiff` crate.
//!
//! [`decode`] and [`encode`] are pure functions over bytes and [`Frame`]s;
//! the `SendReceive` wrappers in `lib.rs` never touch a `tiff::` type.

use std::io::Cursor;

use tiff::decoder::DecodingResult;
use tiff::encoder::colortype;
use tiff::ColorType;
use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData, FrameFlags};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

const fn native16(le: PixFmt, be: PixFmt) -> PixFmt {
    if cfg!(target_endian = "big") { be } else { le }
}

/// Pack a native-endian `u16` sample vector into bytes at the frame's own
/// native endianness. `Vec<u16>` is already native-endian in memory, so this
/// is a plain byte-order-preserving copy, never a swap.
fn pack_u16(samples: &[u16]) -> Vec<u8> {
    let mut out = Vec::new();
    for &s in samples {
        out.extend_from_slice(&s.to_ne_bytes());
    }
    out
}

/// One decoded page's pixels, already laid out as bytes in the destination
/// [`PixFmt`]'s own native representation.
struct Page {
    width: u32,
    height: u32,
    format: PixFmt,
    row_bytes: usize,
    bytes: Vec<u8>,
}

/// Map one TIFF page's `(ColorType, DecodingResult)` to a [`PixFmt`] and its
/// packed bytes.
///
/// Covers 8- and 16-bit grayscale, grayscale+alpha, RGB and RGBA — the
/// common case the `tiff` crate decodes without any bitstream filter of its
/// own. Palette, CMYK, floating point and other bit depths are a documented
/// coverage gap (plan 15 §4A.2's TIFF risk note: "coverage, not
/// correctness"), reported as [`Error::Unsupported`] rather than guessed at.
fn page_from_result(width: u32, height: u32, color: ColorType, result: DecodingResult) -> Result<Page> {
    match (color, result) {
        (ColorType::Gray(8), DecodingResult::U8(v)) => Ok(Page {
            width,
            height,
            format: PixFmt::Gray8,
            row_bytes: width as usize,
            bytes: v,
        }),
        (ColorType::Gray(16), DecodingResult::U16(v)) => Ok(Page {
            width,
            height,
            format: native16(PixFmt::Gray16le, PixFmt::Gray16be),
            row_bytes: width as usize * 2,
            bytes: pack_u16(&v),
        }),
        (ColorType::GrayA(8), DecodingResult::U8(v)) => Ok(Page {
            width,
            height,
            format: PixFmt::Ya8,
            row_bytes: width as usize * 2,
            bytes: v,
        }),
        (ColorType::GrayA(16), DecodingResult::U16(v)) => Ok(Page {
            width,
            height,
            format: native16(PixFmt::Ya16le, PixFmt::Ya16be),
            row_bytes: width as usize * 4,
            bytes: pack_u16(&v),
        }),
        (ColorType::RGB(8), DecodingResult::U8(v)) => Ok(Page {
            width,
            height,
            format: PixFmt::Rgb24,
            row_bytes: width as usize * 3,
            bytes: v,
        }),
        (ColorType::RGB(16), DecodingResult::U16(v)) => Ok(Page {
            width,
            height,
            format: native16(PixFmt::Rgb48le, PixFmt::Rgb48be),
            row_bytes: width as usize * 6,
            bytes: pack_u16(&v),
        }),
        (ColorType::RGBA(8), DecodingResult::U8(v)) => Ok(Page {
            width,
            height,
            format: PixFmt::Rgba,
            row_bytes: width as usize * 4,
            bytes: v,
        }),
        (ColorType::RGBA(16), DecodingResult::U16(v)) => Ok(Page {
            width,
            height,
            format: native16(PixFmt::Rgba64le, PixFmt::Rgba64be),
            row_bytes: width as usize * 8,
            bytes: pack_u16(&v),
        }),
        _ => Err(Error::Unsupported("tiff: colour type/sample format")),
    }
}

fn frame_from_page(budget: &mut Budget, page: &Page) -> Result<Frame> {
    let mut frame = Frame::alloc_video(budget, page.format, page.width, page.height)?;
    for mut plane in frame.planes_mut() {
        for row in 0..plane.rows() {
            let src_start = row * page.row_bytes;
            let Some(src) = page.bytes.get(src_start..src_start + page.row_bytes) else {
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

/// Decode every page of a (possibly multi-page) TIFF.
///
/// # Errors
///
/// [`Error::InvalidData`] for a malformed header or IFD chain.
/// [`Error::Unsupported`] for a colour type this crate does not map to a
/// [`PixFmt`] (see [`page_from_result`]) or a compression the `tiff` crate
/// itself does not implement (CCITT G3/G4, JPEG-in-TIFF — a genuine coverage
/// gap, not a bug). [`Error::LimitExceeded`] when a page exceeds `budget`.
pub fn decode(bytes: &[u8], budget: &mut Budget) -> Result<Vec<Frame>> {
    let mut decoder =
        tiff::decoder::Decoder::new(Cursor::new(bytes)).map_err(|_| Error::InvalidData("tiff: header"))?;
    let mut out = Vec::new();
    loop {
        let (width, height) = decoder
            .dimensions()
            .map_err(|_| Error::InvalidData("tiff: IFD dimensions"))?;
        let color = decoder
            .colortype()
            .map_err(|_| Error::Unsupported("tiff: colour type"))?;
        budget.check_frame(width, height, 8)?;
        let result = decoder
            .read_image()
            .map_err(|_| Error::Unsupported("tiff: compression or sample layout"))?;
        let page = page_from_result(width, height, color, result)?;
        out.push(frame_from_page(budget, &page)?);

        if !decoder.more_images() {
            break;
        }
        decoder
            .next_image()
            .map_err(|_| Error::InvalidData("tiff: IFD chain"))?;
    }
    Ok(out)
}

/// One frame's pixel format translated to the `tiff` crate's own
/// `ColorType` marker type, dispatched at each call site since
/// `TiffEncoder::write_image` is generic over it.
enum Encodable {
    Gray8(Vec<u8>),
    Gray16(Vec<u16>),
    Rgb8(Vec<u8>),
    Rgb16(Vec<u16>),
    Rgba8(Vec<u8>),
    Rgba16(Vec<u16>),
}

fn unpack_u16(bytes: &[u8]) -> Vec<u16> {
    bytes
        .chunks_exact(2)
        .filter_map(|c| <[u8; 2]>::try_from(c).ok())
        .map(u16::from_ne_bytes)
        .collect()
}

/// Bytes for one frame's single plane, tightly packed (no row padding).
fn packed_plane_bytes(frame: &Frame, row_bytes: usize) -> Result<Vec<u8>> {
    let FrameData::Video { height, .. } = &frame.data else {
        return Err(Error::Unsupported("tiff: audio frame"));
    };
    let plane = frame.plane(0).ok_or(Error::InvalidData("tiff: no plane"))?;
    let mut out = vec![0u8; row_bytes * *height as usize];
    for (row_idx, row) in plane.rows_iter().take(*height as usize).enumerate() {
        if let Some(dst) = out.get_mut(row_idx * row_bytes..(row_idx + 1) * row_bytes) {
            let n = dst.len().min(row.len());
            if let (Some(d), Some(s)) = (dst.get_mut(..n), row.get(..n)) {
                d.copy_from_slice(s);
            }
        }
    }
    Ok(out)
}

/// Encode one or more frames as a (possibly multi-page) TIFF.
///
/// Fidelity is D11 "Exact for what it covers": every pixel this crate
/// supports round-trips exactly, since neither direction applies lossy
/// compression by default.
///
/// # Errors
///
/// [`Error::InvalidData`] for an empty frame list or an encoder failure.
/// [`Error::Unsupported`] for a pixel format this crate does not map to a
/// TIFF colour type.
pub fn encode(frames: &[Frame]) -> Result<Vec<u8>> {
    if frames.is_empty() {
        return Err(Error::InvalidData("tiff: no frames to encode"));
    }
    let mut out: Vec<u8> = Vec::new();
    {
        let mut cursor = Cursor::new(&mut out);
        let mut encoder =
            tiff::encoder::TiffEncoder::new(&mut cursor).map_err(|_| Error::InvalidData("tiff: header encode"))?;
        for frame in frames {
            let FrameData::Video { format, width, height, .. } = &frame.data else {
                return Err(Error::Unsupported("tiff: audio frame"));
            };
            let (width, height) = (*width, *height);
            match to_encodable(frame, *format)? {
                Encodable::Gray8(d) => encoder
                    .write_image::<colortype::Gray8>(width, height, &d)
                    .map_err(|_| Error::InvalidData("tiff: page encode"))?,
                Encodable::Gray16(d) => encoder
                    .write_image::<colortype::Gray16>(width, height, &d)
                    .map_err(|_| Error::InvalidData("tiff: page encode"))?,
                Encodable::Rgb8(d) => encoder
                    .write_image::<colortype::RGB8>(width, height, &d)
                    .map_err(|_| Error::InvalidData("tiff: page encode"))?,
                Encodable::Rgb16(d) => encoder
                    .write_image::<colortype::RGB16>(width, height, &d)
                    .map_err(|_| Error::InvalidData("tiff: page encode"))?,
                Encodable::Rgba8(d) => encoder
                    .write_image::<colortype::RGBA8>(width, height, &d)
                    .map_err(|_| Error::InvalidData("tiff: page encode"))?,
                Encodable::Rgba16(d) => encoder
                    .write_image::<colortype::RGBA16>(width, height, &d)
                    .map_err(|_| Error::InvalidData("tiff: page encode"))?,
            }
        }
    }
    Ok(out)
}

fn to_encodable(frame: &Frame, format: PixFmt) -> Result<Encodable> {
    Ok(match format {
        PixFmt::Gray8 => Encodable::Gray8(packed_plane_bytes(frame, plane_row_bytes(frame, 1)?)?),
        PixFmt::Gray16le | PixFmt::Gray16be => {
            Encodable::Gray16(unpack_u16(&packed_plane_bytes(frame, plane_row_bytes(frame, 2)?)?))
        }
        // The `tiff` crate's encoder has no grayscale+alpha `ColorType`
        // marker (only `Gray*`, `RGB*`, `RGBA*`, `CMYK*`, `YCbCr8`), so
        // `Ya8`/`Ya16` cannot round-trip through encode — a real coverage
        // gap, not an oversight; decode still supports them.
        PixFmt::Rgb24 => Encodable::Rgb8(packed_plane_bytes(frame, plane_row_bytes(frame, 3)?)?),
        PixFmt::Rgb48le | PixFmt::Rgb48be => {
            Encodable::Rgb16(unpack_u16(&packed_plane_bytes(frame, plane_row_bytes(frame, 6)?)?))
        }
        PixFmt::Rgba => Encodable::Rgba8(packed_plane_bytes(frame, plane_row_bytes(frame, 4)?)?),
        PixFmt::Rgba64le | PixFmt::Rgba64be => {
            Encodable::Rgba16(unpack_u16(&packed_plane_bytes(frame, plane_row_bytes(frame, 8)?)?))
        }
        _ => return Err(Error::Unsupported("tiff: encode pixel format")),
    })
}

fn plane_row_bytes(frame: &Frame, bytes_per_pixel: usize) -> Result<usize> {
    let FrameData::Video { width, .. } = &frame.data else {
        return Err(Error::Unsupported("tiff: audio frame"));
    };
    Ok(*width as usize * bytes_per_pixel)
}
