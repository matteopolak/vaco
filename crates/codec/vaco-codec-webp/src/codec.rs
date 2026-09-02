//! The byte format: WebP.
//!
//! [`decode`] and [`encode`] are pure functions over bytes and [`Frame`]s;
//! [`vp8l`] is this crate's own native lossless codec, used directly here
//! for the common "bare `VP8L` chunk" case (a still lossless image, no
//! `VP8X` wrapper). Anything with a `VP8X` header — alpha via a separate
//! chunk, animation, ICCP/EXIF metadata — still goes through `image-webp`;
//! extending that to native code is future work, not required by C-19
//! (native lossless, lossy routed through `vaco-codec-vp8`).

use std::io::Cursor;

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData, FrameFlags};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

use crate::vp8l;

fn chunk_at(bytes: &[u8], want_fourcc: [u8; 4]) -> Option<&[u8]> {
    if bytes.get(0..4)? != b"RIFF" {
        return None;
    }
    if bytes.get(8..12)? != b"WEBP" {
        return None;
    }
    if bytes.get(12..16)? != want_fourcc {
        return None;
    }
    let size_bytes: [u8; 4] = bytes.get(16..20)?.try_into().ok()?;
    let size = u32::from_le_bytes(size_bytes) as usize;
    bytes.get(20..20usize.checked_add(size)?)
}

/// Decode one WebP packet into every frame it carries.
///
/// A bare `VP8L` file (no `VP8X` wrapper) is decoded natively via [`vp8l`].
/// Everything else — `VP8X`-wrapped (alpha, animation, metadata chunks) —
/// falls back to `image_webp::WebPDecoder`, which composites each `ANMF`
/// frame's dispose/blend onto the canvas internally (unlike GIF/APNG, which
/// this crate composites itself), hence [`crate::Caps::SUBFRAMES`] on
/// decode.
///
/// # Errors
///
/// [`Error::InvalidData`] for a malformed RIFF/VP8(L/X) stream.
/// [`Error::UnexpectedEof`] for a truncated stream. [`Error::LimitExceeded`]
/// when the canvas exceeds `budget`.
pub fn decode(bytes: &[u8], budget: &mut Budget) -> Result<Vec<Frame>> {
    if let Some(payload) = chunk_at(bytes, *b"VP8L") {
        let image = vp8l::decode(payload, budget)?;
        let frame = argb_to_frame(
            budget,
            &image.pixels,
            image.width,
            image.height,
            image.alpha_is_used,
        )?;
        return Ok(vec![frame]);
    }

    let mut decoder = image_webp::WebPDecoder::new(Cursor::new(bytes))
        .map_err(|_| Error::InvalidData("webp: header"))?;
    let (width, height) = decoder.dimensions();
    let has_alpha = decoder.has_alpha();
    let bpp = if has_alpha { 4 } else { 3 };
    budget.check_frame(width, height, bpp)?;
    let format = if has_alpha {
        PixFmt::Rgba
    } else {
        PixFmt::Rgb24
    };

    let Some(buf_len) = decoder.output_buffer_size() else {
        return Err(Error::Unsupported("webp: image too large"));
    };

    let mut out = Vec::new();
    if !decoder.is_animated() {
        let mut buf: Vec<u8> = budget.alloc(buf_len)?;
        decoder
            .read_image(&mut buf)
            .map_err(|_| Error::InvalidData("webp: image data"))?;
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

/// Unpack a video [`Frame`] into a row-major `0xAARRGGBB` buffer plus
/// whether the frame's own pixel format carries an alpha channel at all
/// (`Rgba`/`Ya8`), as opposed to whether any decoded alpha byte happens to
/// differ from opaque.
///
/// The two are not the same thing, and only the format-based answer is
/// correct here: a `VP8L` stream may legally set `alpha_is_used` and still
/// code every pixel fully opaque (nothing requires an encoder to omit a
/// redundant alpha plane), which decodes to `Rgba` with every alpha byte
/// `255`. Deriving `has_alpha` from *content* rather than *format* would
/// answer "false" for such a frame and make [`encode`] emit a plain `Rgb24`
/// stream — changing the frame's pixel format on every decode -> encode ->
/// decode round trip even though no pixel's value changed. Basing it on the
/// format instead makes the round trip preserve both.
///
/// # Errors
///
/// [`Error::Unsupported`] for a non-video frame or a pixel format this
/// crate does not map to ARGB (`Gray8`/`Ya8`/`Rgb24`/`Rgba`).
#[allow(
    clippy::many_single_char_names,
    reason = "a/r/g/b are the ARGB channel names, clearer here than any longer alternative"
)]
pub(crate) fn frame_to_argb(frame: &Frame) -> Result<(Vec<u32>, u32, u32, bool)> {
    let FrameData::Video {
        format,
        width,
        height,
        ..
    } = &frame.data
    else {
        return Err(Error::Unsupported("webp: audio frame"));
    };
    let (width, height) = (*width, *height);
    let plane = frame.plane(0).ok_or(Error::InvalidData("webp: no plane"))?;
    let mut pixels = vec![0u32; (width as usize).saturating_mul(height as usize)];
    let has_alpha_channel = matches!(format, PixFmt::Rgba | PixFmt::Ya8);
    let w = width as usize;
    for (y, row) in plane.rows_iter().take(height as usize).enumerate() {
        for x in 0..w {
            let (a, r, g, b) = match format {
                PixFmt::Gray8 => {
                    let g = row.get(x).copied().unwrap_or(0);
                    (255, g, g, g)
                }
                PixFmt::Ya8 => {
                    let base = x * 2;
                    let g = row.get(base).copied().unwrap_or(0);
                    let a = row.get(base + 1).copied().unwrap_or(255);
                    (a, g, g, g)
                }
                PixFmt::Rgb24 => {
                    let base = x * 3;
                    (
                        255,
                        row.get(base).copied().unwrap_or(0),
                        row.get(base + 1).copied().unwrap_or(0),
                        row.get(base + 2).copied().unwrap_or(0),
                    )
                }
                PixFmt::Rgba => {
                    let base = x * 4;
                    (
                        row.get(base + 3).copied().unwrap_or(255),
                        row.get(base).copied().unwrap_or(0),
                        row.get(base + 1).copied().unwrap_or(0),
                        row.get(base + 2).copied().unwrap_or(0),
                    )
                }
                _ => return Err(Error::Unsupported("webp: encode pixel format")),
            };
            if let Some(slot) = pixels.get_mut(y * w + x) {
                *slot = (u32::from(a) << 24)
                    | (u32::from(r) << 16)
                    | (u32::from(g) << 8)
                    | u32::from(b);
            }
        }
    }
    Ok((pixels, width, height, has_alpha_channel))
}

#[allow(
    clippy::many_single_char_names,
    reason = "a/r/g/b are the ARGB channel names, clearer here than any longer alternative"
)]
fn argb_to_frame(
    budget: &mut Budget,
    pixels: &[u32],
    width: u32,
    height: u32,
    has_alpha: bool,
) -> Result<Frame> {
    let format = if has_alpha {
        PixFmt::Rgba
    } else {
        PixFmt::Rgb24
    };
    let mut frame = Frame::alloc_video(budget, format, width, height)?;
    let bpp = if has_alpha { 4 } else { 3 };
    let w = width as usize;
    for mut plane in frame.planes_mut() {
        for y in 0..plane.rows() {
            if y >= height as usize {
                break;
            }
            let Some(row) = plane.row_mut(y) else {
                continue;
            };
            for x in 0..w {
                let px = pixels.get(y * w + x).copied().unwrap_or(0);
                let a = ((px >> 24) & 0xff) as u8;
                let r = ((px >> 16) & 0xff) as u8;
                let g = ((px >> 8) & 0xff) as u8;
                let b = (px & 0xff) as u8;
                let base = x * bpp;
                let src: [u8; 4] = [r, g, b, a];
                if let Some(dst) = row.get_mut(base..base + bpp) {
                    let n = bpp.min(4);
                    if let Some(s) = src.get(..n) {
                        dst.copy_from_slice(s);
                    }
                }
            }
        }
    }
    frame.flags = FrameFlags::KEY;
    Ok(frame)
}

/// Encode a single frame as a lossless (`VP8L`) WebP image, using this
/// crate's own native codec (see [`vp8l`]'s module doc for exactly what it
/// emits). Fidelity is D11 "Exact": lossless WebP is an integer-exact
/// transform, so every pixel this crate supports round-trips exactly.
///
/// # Errors
///
/// [`Error::Unsupported`] for a non-video frame or an unmapped pixel
/// format. [`Error::InvalidData`]/[`Error::LimitExceeded`] from the
/// underlying [`vp8l::encode`].
pub fn encode(frame: &Frame) -> Result<Vec<u8>> {
    let (pixels, width, height, has_alpha) = frame_to_argb(frame)?;
    let mut budget = Budget::new(vaco_limits::Limits::permissive());
    let stream = vp8l::encode(&pixels, width, height, has_alpha, &mut budget)?;
    Ok(build_riff(*b"VP8L", &stream))
}

/// Encode a single frame as a lossy (`VP8`) WebP image, wrapping
/// `vaco-codec-vp8`'s real encoder (C-19: "route lossy through C-16"). The
/// input is converted to `Yuv420p` via `vaco-scale` first, since VP8 only
/// ever codes that format; WebP carries no non-keyframe VP8, so this always
/// runs a single, self-contained keyframe encode.
///
/// # Errors
///
/// [`Error::Unsupported`] for a non-video frame. Whatever `vaco-scale` or
/// `vaco-codec-vp8` returns for a conversion or encode failure.
pub(crate) fn encode_lossy(frame: &Frame, limits: &vaco_limits::Limits) -> Result<Vec<u8>> {
    use vaco_codec_core::Encoder as _;

    let FrameData::Video {
        format,
        width,
        height,
        ..
    } = &frame.data
    else {
        return Err(Error::Unsupported("webp: audio frame"));
    };
    let (width, height) = (*width, *height);

    let yuv_frame = if *format == PixFmt::Yuv420p {
        frame.clone()
    } else {
        let src_spec = vaco_scale::ImageSpec::new(*format, width, height);
        let dst_spec = vaco_scale::ImageSpec::new(PixFmt::Yuv420p, width, height);
        let mut scaler =
            vaco_scale::Scaler::new(&src_spec, &dst_spec, &vaco_scale::ScaleOptions::default())?;
        let mut budget = Budget::new(limits.clone());
        let mut dst = Frame::alloc_video(&mut budget, PixFmt::Yuv420p, width, height)?;
        scaler.scale_frame(frame, &mut dst)?;
        dst
    };

    let mut enc = vaco_codec_vp8::encode::Vp8Encoder::new(limits.clone());
    enc.send_frame(Some(&yuv_frame))?;
    enc.send_frame(None)?;
    let packet = enc.receive_packet()?;
    Ok(build_riff(*b"VP8 ", packet.payload()))
}

fn build_riff(fourcc: [u8; 4], chunk_data: &[u8]) -> Vec<u8> {
    let padded_len = chunk_data.len() + (chunk_data.len() & 1);
    let riff_size = 4 /* "WEBP" */ + 8 /* chunk header */ + padded_len;
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(riff_size as u32).to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend_from_slice(&fourcc);
    out.extend_from_slice(&(chunk_data.len() as u32).to_le_bytes());
    out.extend_from_slice(chunk_data);
    if chunk_data.len() & 1 == 1 {
        out.push(0);
    }
    out
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code exercising the decoder, not the untrusted-input surface \
              the lint protects"
)]
mod tests {
    use super::*;

    fn checker_frame(w: u32, h: u32, format: PixFmt) -> Frame {
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let mut frame = Frame::alloc_video(&mut budget, format, w, h).expect("alloc");
        let bpp = match format {
            PixFmt::Rgb24 => 3,
            PixFmt::Rgba => 4,
            PixFmt::Gray8 => 1,
            _ => panic!("unsupported test format"),
        };
        for mut plane in frame.planes_mut() {
            for y in 0..plane.rows() {
                let row_bytes = plane.row_bytes();
                if let Some(row) = plane.row_mut(y) {
                    for x in 0..(row_bytes / bpp) {
                        let base = x * bpp;
                        for c in 0..bpp {
                            row[base + c] = ((x * 37 + y * 91 + c * 53) % 256) as u8;
                        }
                    }
                }
            }
        }
        frame
    }

    fn frame_bytes(frame: &Frame) -> Vec<u8> {
        let plane = frame.plane(0).expect("plane 0");
        let mut out = Vec::new();
        for row in plane.rows_iter() {
            out.extend_from_slice(row);
        }
        out
    }

    #[test]
    fn round_trips_lossless() {
        for format in [PixFmt::Rgb24, PixFmt::Rgba] {
            let frame = checker_frame(9, 5, format);
            let encoded = encode(&frame).expect("encode");
            let mut budget = Budget::new(vaco_limits::Limits::permissive());
            let decoded = decode(&encoded, &mut budget).expect("decode");
            assert_eq!(decoded.len(), 1);
            assert_eq!(frame_bytes(&frame), frame_bytes(&decoded[0]), "{format:?}");
        }
    }

    /// Regression test for the fuzz finding recorded at
    /// `fuzz/artifacts/webp_decode/crash-e121fd4b0a6180455021737f2feff13af210d606`
    /// (an 89-byte hand-shaped `VP8L` stream whose header sets
    /// `alpha_is_used = 1` while every coded pixel is fully opaque — legal
    /// per the WebP Lossless Bitstream Specification, which does not require
    /// an encoder to omit a redundant alpha plane).
    ///
    /// `encode` used to decide whether to emit an alpha channel by scanning
    /// the unpacked pixels for any alpha byte `!= 255` (`frame_to_argb`'s old
    /// `any_alpha` accumulator), instead of trusting the input [`Frame`]'s
    /// own [`PixFmt`]. An all-opaque `Rgba` frame therefore silently
    /// re-encoded as `Rgb24`, so a decode -> encode -> decode round trip
    /// changed the frame's pixel format (and so its `plane(0)` byte layout)
    /// even though no pixel's colour or alpha *value* changed. The fix
    /// derives `has_alpha` from `format` (`Rgba`/`Ya8` carry alpha;
    /// `Gray8`/`Rgb24` do not), so the format itself round-trips.
    #[test]
    fn rgba_round_trips_when_fully_opaque() {
        let (w, h) = (9, 5);
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgba, w, h).expect("alloc");
        for mut plane in frame.planes_mut() {
            for y in 0..plane.rows() {
                if let Some(row) = plane.row_mut(y) {
                    for x in 0..(row.len() / 4) {
                        let base = x * 4;
                        row[base] = ((x * 37 + y * 91) % 256) as u8;
                        row[base + 1] = ((x * 53 + y * 17) % 256) as u8;
                        row[base + 2] = ((x * 71 + y * 5) % 256) as u8;
                        row[base + 3] = 255; // fully opaque everywhere
                    }
                }
            }
        }

        let encoded = encode(&frame).expect("encode");
        let mut decode_budget = Budget::new(vaco_limits::Limits::permissive());
        let decoded = decode(&encoded, &mut decode_budget).expect("decode");
        assert_eq!(decoded.len(), 1);

        let FrameData::Video { format, .. } = &decoded[0].data else {
            panic!("expected a video frame");
        };
        assert_eq!(
            *format,
            PixFmt::Rgba,
            "an all-opaque Rgba input must still decode back as Rgba"
        );
        assert_eq!(frame_bytes(&frame), frame_bytes(&decoded[0]));
    }

    #[test]
    fn gray_round_trips_as_rgb() {
        let frame = checker_frame(6, 4, PixFmt::Gray8);
        let encoded = encode(&frame).expect("encode");
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let decoded = decode(&encoded, &mut budget).expect("decode");
        assert_eq!(decoded.len(), 1);
        let gray = frame_bytes(&frame);
        let rgb = frame_bytes(&decoded[0]);
        assert_eq!(rgb.len(), gray.len() * 3);
        for (g, rgb_px) in gray.iter().zip(rgb.chunks_exact(3)) {
            assert_eq!(rgb_px, [*g, *g, *g]);
        }
    }

    #[test]
    fn rejects_bad_magic() {
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let err = decode(b"not a webp", &mut budget).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_) | Error::UnexpectedEof));
    }

    #[test]
    fn lossy_round_trips_through_this_crates_own_decode_fallback() {
        // The bare-VP8L fast path in `decode` does not match a "VP8 " chunk,
        // so this exercises the `image_webp` fallback on our own
        // native-VP8-via-vaco-codec-vp8 output — the same file shape a
        // real lossy `cwebp` output has.
        let frame = checker_frame(16, 16, PixFmt::Rgb24);
        let bytes = encode_lossy(&frame, &vaco_limits::Limits::permissive()).expect("lossy encode");
        assert_eq!(bytes.get(0..4), Some(b"RIFF".as_slice()));
        assert_eq!(bytes.get(8..12), Some(b"WEBP".as_slice()));
        assert_eq!(bytes.get(12..16), Some(b"VP8 ".as_slice()));
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let decoded = decode(&bytes, &mut budget).expect("decode lossy webp");
        assert_eq!(decoded.len(), 1);
    }
}
