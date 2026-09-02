//! Byte-identical pixel copy shared by `rawvideo`, `bitpacked`, `wrapped_avframe`
//! and (via a fixed target format) `r210`.
//!
//! A raw-video packet carries no header of its own: its payload *is* the
//! frame's pixel planes, laid out exactly as
//! [`vaco_pixfmt::PixFmt::plane_layout`] describes them with `align = 1` —
//! byte-aligned, no row padding. That is the same convention
//! `vaco-demux-raw::rawvideo`'s `Packing::PixFmtPlanes` uses on the demux
//! side (measured there against `ffprobe`; see that crate's docs), so this
//! module's [`decode_raw`]/[`encode_raw`] are the codec-side mirror of it.
//!
//! The one subtlety is that a [`vaco_frame::Frame`]'s *own* planes are
//! allocated with each row padded to [`vaco_pool::ALIGN`]
//! (`vaco_frame::Frame::alloc_video`), while the wire representation has no
//! such padding — so this cannot be a single `memcpy` of a whole plane. Each
//! row is copied individually, at the wire's row length, leaving whatever
//! padding the frame's own allocator added untouched.

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

/// Decode `payload` as `format`'s tightly packed planes into a freshly
/// allocated `width`x`height` frame.
///
/// # Errors
/// [`Error::InvalidData`] for a `0x0` picture size or an unrepresentable
/// geometry, [`Error::UnexpectedEof`] if `payload` is shorter than the
/// declared geometry implies, and whatever [`Frame::alloc_video`] returns for
/// an over-budget or hardware-only format.
pub fn decode_raw(
    payload: &[u8],
    width: u32,
    height: u32,
    format: PixFmt,
    budget: &mut Budget,
) -> Result<Frame> {
    if width == 0 || height == 0 {
        // Matches `vaco-demux-raw::rawvideo`'s own wording for the identical
        // situation on the demux side.
        return Err(Error::InvalidData("rawvideo: picture size 0x0 is invalid"));
    }
    let wire = format
        .plane_layout(width, height, 1)
        .map_err(|_| Error::InvalidData("rawvideo: pixel format geometry overflowed"))?;
    if payload.len() < wire.total {
        return Err(Error::UnexpectedEof);
    }

    let mut frame = Frame::alloc_video(budget, format, width, height)?;
    let FrameData::Video { planes, .. } = &mut frame.data else {
        return Err(Error::InvalidData("rawvideo: expected a video frame"));
    };

    let mut src_off = 0usize;
    for i in 0..wire.planes {
        let plane_idx = u8::try_from(i).unwrap_or(u8::MAX);
        let rows = format.plane_height(height, plane_idx) as usize;
        let row_bytes = format.min_stride(width, plane_idx);
        let plane = planes
            .get_mut(i)
            .ok_or(Error::InvalidData("rawvideo: missing plane"))?;
        let dst_stride = plane.stride;
        let buf = plane.data.make_mut();
        for row in 0..rows {
            let src = payload
                .get(src_off..src_off.saturating_add(row_bytes))
                .ok_or(Error::UnexpectedEof)?;
            let dst_start = row.saturating_mul(dst_stride);
            let dst = buf
                .get_mut(dst_start..dst_start.saturating_add(row_bytes))
                .ok_or(Error::InvalidData("rawvideo: plane shorter than a row"))?;
            dst.copy_from_slice(src);
            src_off = src_off.saturating_add(row_bytes);
        }
    }
    Ok(frame)
}

/// Encode a video frame as its own pixel format's tightly packed planes —
/// the exact inverse of [`decode_raw`].
///
/// # Errors
/// [`Error::InvalidData`] for a non-video frame or a `0x0` picture size.
pub fn encode_raw(frame: &Frame) -> Result<Vec<u8>> {
    let FrameData::Video {
        format,
        width,
        height,
        planes,
    } = &frame.data
    else {
        return Err(Error::InvalidData("rawvideo: expected a video frame"));
    };
    let format = *format;
    let (width, height) = (*width, *height);
    if width == 0 || height == 0 {
        return Err(Error::InvalidData("rawvideo: picture size 0x0 is invalid"));
    }
    let wire = format
        .plane_layout(width, height, 1)
        .map_err(|_| Error::InvalidData("rawvideo: pixel format geometry overflowed"))?;

    let mut out = vec![0u8; wire.total];
    let mut dst_off = 0usize;
    for i in 0..wire.planes {
        let plane_idx = u8::try_from(i).unwrap_or(u8::MAX);
        let rows = format.plane_height(height, plane_idx) as usize;
        let row_bytes = format.min_stride(width, plane_idx);
        let plane = planes
            .get(i)
            .ok_or(Error::InvalidData("rawvideo: missing plane"))?;
        let src_stride = plane.stride;
        let src = plane.data.as_slice();
        for row in 0..rows {
            let src_start = row.saturating_mul(src_stride);
            let chunk = src
                .get(src_start..src_start.saturating_add(row_bytes))
                .ok_or(Error::InvalidData("rawvideo: plane shorter than a row"))?;
            let dst = out
                .get_mut(dst_off..dst_off.saturating_add(row_bytes))
                .ok_or(Error::InvalidData("rawvideo: encode buffer too short"))?;
            dst.copy_from_slice(chunk);
            dst_off = dst_off.saturating_add(row_bytes);
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
    reason = "test code exercising the codec, not the untrusted-input surface \
              the lint protects"
)]
mod tests {
    use super::*;
    use vaco_core::Error;
    use vaco_limits::Limits;

    #[test]
    fn round_trips_a_planar_format() {
        let mut budget = Budget::new(Limits::permissive());
        let width = 5u32;
        let height = 3u32;
        let layout = PixFmt::Yuv420p
            .plane_layout(width, height, 1)
            .expect("layout");
        let payload: Vec<u8> = (0..layout.total).map(|i| (i % 251) as u8).collect();
        let frame =
            decode_raw(&payload, width, height, PixFmt::Yuv420p, &mut budget).expect("decode");
        let encoded = encode_raw(&frame).expect("encode");
        assert_eq!(encoded, payload);
    }

    #[test]
    fn round_trips_a_packed_format() {
        let mut budget = Budget::new(Limits::permissive());
        let width = 7u32;
        let height = 2u32;
        let layout = PixFmt::Rgb24
            .plane_layout(width, height, 1)
            .expect("layout");
        let payload: Vec<u8> = (0..layout.total).map(|i| (i * 3 % 256) as u8).collect();
        let frame =
            decode_raw(&payload, width, height, PixFmt::Rgb24, &mut budget).expect("decode");
        let encoded = encode_raw(&frame).expect("encode");
        assert_eq!(encoded, payload);
    }

    #[test]
    fn zero_size_is_rejected() {
        let mut budget = Budget::new(Limits::permissive());
        let err = decode_raw(&[], 0, 0, PixFmt::Yuv420p, &mut budget).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn truncated_payload_is_rejected() {
        let mut budget = Budget::new(Limits::permissive());
        let err = decode_raw(&[0u8; 2], 4, 4, PixFmt::Yuv420p, &mut budget).unwrap_err();
        assert!(matches!(err, Error::UnexpectedEof));
    }
}
