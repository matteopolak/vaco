//! The byte format: GIF, wrapping the `gif` crate.
//!
//! [`decode`] and [`encode`] are pure functions over bytes and [`Frame`]s;
//! the `SendReceive` wrappers in `lib.rs` never touch a `gif::` type.

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData, FrameFlags};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

/// Composite one already-RGBA GIF subframe onto the canvas at `(x, y)`.
///
/// GIF has no partial alpha: `gif::ColorOutput::Rgba` already resolved each
/// pixel to either fully opaque or the transparent index (alpha 0), so
/// compositing is "replace, except transparent pixels leave the canvas
/// alone" — there is no blend operation to choose between, unlike APNG.
#[allow(
    clippy::many_single_char_names,
    reason = "x/y/w/h read naturally for a pixel-rectangle composite"
)]
fn composite(canvas: &mut [u8], canvas_w: u32, x: u32, y: u32, w: u32, h: u32, src: &[u8]) {
    for row in 0..h {
        let cy = y + row;
        let Some(src_row) = src.get(row as usize * w as usize * 4..(row as usize + 1) * w as usize * 4) else {
            continue;
        };
        let dst_start = (cy as usize * canvas_w as usize + x as usize) * 4;
        let Some(dst_row) = canvas.get_mut(dst_start..dst_start + w as usize * 4) else {
            continue;
        };
        for (dp, sp) in dst_row.chunks_exact_mut(4).zip(src_row.chunks_exact(4)) {
            if let &[.., a] = sp
                && a != 0
            {
                dp.copy_from_slice(sp);
            }
        }
    }
}

/// Decode one GIF packet into every composited frame it carries.
///
/// Each frame is decoded already resolved to RGBA (`gif::ColorOutput::Rgba`)
/// and composited onto a shared canvas at its own declared position, per its
/// own disposal method — the reference decoder's pipeline does this
/// compositing itself rather than the `gif` crate, and this reproduces that
/// (plan 15 §4A.2's GIF risk note).
///
/// # Errors
///
/// [`Error::InvalidData`] for a malformed header or block sequence.
/// [`Error::UnexpectedEof`] for a truncated stream. [`Error::LimitExceeded`]
/// when the canvas exceeds `budget`.
#[allow(
    clippy::many_single_char_names,
    reason = "x/y/w/h read naturally for a pixel-rectangle composite"
)]
pub fn decode(bytes: &[u8], budget: &mut Budget) -> Result<Vec<Frame>> {
    let mut options = gif::DecodeOptions::new();
    options.set_color_output(gif::ColorOutput::RGBA);
    let mut decoder = options
        .read_info(bytes)
        .map_err(|_| Error::InvalidData("gif: header"))?;
    let canvas_w = u32::from(decoder.width());
    let canvas_h = u32::from(decoder.height());
    budget.check_frame(canvas_w, canvas_h, 4)?;

    let mut canvas = vec![0u8; canvas_w as usize * canvas_h as usize * 4];
    let mut previous: Option<Vec<u8>> = None;
    let mut out = Vec::new();

    loop {
        let frame = match decoder.read_next_frame() {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(_) if !out.is_empty() => break,
            Err(_) => return Err(Error::InvalidData("gif: frame data")),
        };
        let (x, y, w, h) = (
            u32::from(frame.left),
            u32::from(frame.top),
            u32::from(frame.width),
            u32::from(frame.height),
        );
        if frame.dispose == gif::DisposalMethod::Previous {
            previous = Some(canvas.clone());
        }
        composite(&mut canvas, canvas_w, x, y, w, h, &frame.buffer);

        let mut out_frame = Frame::alloc_video(budget, PixFmt::Rgba, canvas_w, canvas_h)?;
        for mut plane in out_frame.planes_mut() {
            for row in 0..plane.rows() {
                let src_start = row * canvas_w as usize * 4;
                if let (Some(dst), Some(src)) = (
                    plane.row_mut(row),
                    canvas.get(src_start..src_start + canvas_w as usize * 4),
                ) {
                    let n = dst.len().min(src.len());
                    if let (Some(d), Some(s)) = (dst.get_mut(..n), src.get(..n)) {
                        d.copy_from_slice(s);
                    }
                }
            }
        }
        // GIF delay is in hundredths of a second.
        out_frame.time_base = vaco_core::Rational::new(1, 100);
        out_frame.duration = vaco_core::Duration(i64::from(frame.delay));
        out_frame.flags = FrameFlags::KEY;
        out.push(out_frame);

        match frame.dispose {
            gif::DisposalMethod::Background => {
                for row in 0..h {
                    let start = ((y + row) as usize * canvas_w as usize + x as usize) * 4;
                    if let Some(slice) = canvas.get_mut(start..start + w as usize * 4) {
                        slice.fill(0);
                    }
                }
            }
            gif::DisposalMethod::Previous => {
                if let Some(p) = previous.take() {
                    canvas = p;
                }
            }
            gif::DisposalMethod::Any | gif::DisposalMethod::Keep => {}
        }
    }

    if out.is_empty() {
        return Err(Error::InvalidData("gif: no image data"));
    }
    Ok(out)
}

/// Encode one or more frames as a GIF (a single frame becomes a
/// non-animated, one-frame GIF).
///
/// Fidelity is D11 "Equivalent": every pixel this crate keeps opaque
/// round-trips exactly, but GIF's 256-colour palette and 1-bit alpha are
/// lossy relative to an arbitrary source frame, and the `NeuQuant`
/// quantisation the `gif` crate performs will not match the reference
/// encoder's palette choice byte-for-byte.
///
/// # Errors
///
/// [`Error::InvalidData`] for an empty frame list, dimensions the GIF
/// format cannot express (`u16::MAX` per axis), or an encoder failure.
/// [`Error::Unsupported`] for a non-video frame.
pub fn encode(frames: &[Frame]) -> Result<Vec<u8>> {
    let Some(first) = frames.first() else {
        return Err(Error::InvalidData("gif: no frames to encode"));
    };
    let FrameData::Video { width, height, .. } = &first.data else {
        return Err(Error::Unsupported("gif: audio frame"));
    };
    let (width, height) = (
        u16::try_from(*width).map_err(|_| Error::InvalidData("gif: width exceeds u16"))?,
        u16::try_from(*height).map_err(|_| Error::InvalidData("gif: height exceeds u16"))?,
    );

    let mut out: Vec<u8> = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut out, width, height, &[])
            .map_err(|_| Error::InvalidData("gif: header encode"))?;
        if frames.len() > 1 {
            let _ = encoder.set_repeat(gif::Repeat::Infinite);
        }
        for frame in frames {
            let mut rgba = to_rgba8(frame)?;
            let mut gif_frame = gif::Frame::from_rgba(width, height, &mut rgba);
            gif_frame.delay = delay_hundredths(frame);
            encoder
                .write_frame(&gif_frame)
                .map_err(|_| Error::InvalidData("gif: frame encode"))?;
        }
    }
    Ok(out)
}

#[allow(
    clippy::integer_division,
    reason = "den is checked non-zero on the line above; converting a duration to hundredths \
              of a second is inherently a ratio"
)]
fn delay_hundredths(frame: &Frame) -> u16 {
    if frame.time_base.den <= 0 {
        return 0;
    }
    let hundredths = frame.duration.0 * 100 / i64::from(frame.time_base.den);
    hundredths.clamp(0, i64::from(u16::MAX)) as u16
}

/// Convert one RGB(A)/gray frame to tightly packed RGBA8, the only shape
/// `gif::Frame::from_rgba` accepts.
fn to_rgba8(frame: &Frame) -> Result<Vec<u8>> {
    let FrameData::Video { format, width, height, .. } = &frame.data else {
        return Err(Error::Unsupported("gif: audio frame"));
    };
    let (width, height) = (*width as usize, *height as usize);
    let plane = frame.plane(0).ok_or(Error::InvalidData("gif: no plane"))?;
    let mut out = vec![0u8; width * height * 4];
    match format {
        PixFmt::Rgba => {
            for (row_idx, row) in plane.rows_iter().take(height).enumerate() {
                if let Some(dst) = out.get_mut(row_idx * width * 4..(row_idx + 1) * width * 4) {
                    let n = dst.len().min(row.len());
                    if let (Some(d), Some(s)) = (dst.get_mut(..n), row.get(..n)) {
                        d.copy_from_slice(s);
                    }
                }
            }
        }
        PixFmt::Rgb24 => {
            for (row_idx, row) in plane.rows_iter().take(height).enumerate() {
                let Some(dst) = out.get_mut(row_idx * width * 4..(row_idx + 1) * width * 4) else {
                    continue;
                };
                for (d, s) in dst.chunks_exact_mut(4).zip(row.chunks_exact(3)) {
                    if let &[r, g, b] = s {
                        d.copy_from_slice(&[r, g, b, 255]);
                    }
                }
            }
        }
        PixFmt::Gray8 => {
            for (row_idx, row) in plane.rows_iter().take(height).enumerate() {
                let Some(dst) = out.get_mut(row_idx * width * 4..(row_idx + 1) * width * 4) else {
                    continue;
                };
                for (d, &g) in dst.chunks_exact_mut(4).zip(row.iter()) {
                    d.copy_from_slice(&[g, g, g, 255]);
                }
            }
        }
        _ => return Err(Error::Unsupported("gif: encode pixel format")),
    }
    Ok(out)
}
