//! `y41p`: uncompressed YUV 4:1:1, packed 8 pixels to 12 bytes (12
//! bits/pixel average).
//!
//! # The packing
//!
//! This is the widely mirrored "Y41P" byte order (documented by multiple
//! independent vendors of 4:1:1 packed formats, not the reference's own
//! expression of it — D7): a 12-byte group encodes 8 luma samples and two
//! chroma pairs, one pair per four luma samples:
//!
//! ```text
//! byte:  0   1   2   3   4   5   6   7   8   9  10  11
//!        U0  Y0  V0  Y1  U4  Y2  V4  Y3  Y4  Y5  Y6  Y7
//! ```
//!
//! `U0`/`V0` cover luma samples 0-3, `U4`/`V4` cover luma samples 4-7 — 4:1:1
//! horizontal-only chroma subsampling, matching
//! [`vaco_pixfmt::PixFmt::Yuv411p`]'s own `log2_chroma_w = 2`,
//! `log2_chroma_h = 0` exactly, which is why that format is the decode
//! target: nothing is widened, narrowed, or resampled, only unpacked.
//!
//! Best-effort, not independently measured against the reference (no `y41p`
//! fixture with known-good bytes was available in this pass) — the issue
//! brief calls this out explicitly as one of the formats where coverage
//! matters more than verified byte-exactness.

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData, Plane};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmt;

const GROUP_PIXELS: usize = 8;
const GROUP_BYTES: usize = 12;

fn row_bytes(width: u32) -> usize {
    (width as usize)
        .div_ceil(GROUP_PIXELS)
        .saturating_mul(GROUP_BYTES)
}

fn split_planes_mut(planes: &mut [Plane]) -> Result<(&mut Plane, &mut Plane, &mut Plane)> {
    if planes.len() != 3 {
        return Err(Error::InvalidData("y41p: expected exactly three planes"));
    }
    let (first, rest) = planes.split_at_mut(1);
    let (second, third) = rest.split_at_mut(1);
    let y = first
        .get_mut(0)
        .ok_or(Error::InvalidData("y41p: missing Y plane"))?;
    let u = second
        .get_mut(0)
        .ok_or(Error::InvalidData("y41p: missing U plane"))?;
    let v = third
        .get_mut(0)
        .ok_or(Error::InvalidData("y41p: missing V plane"))?;
    Ok((y, u, v))
}

/// Decode a `y41p` payload into a [`PixFmt::Yuv411p`] frame.
///
/// # Errors
/// [`Error::InvalidData`] for a `0x0` picture size, [`Error::UnexpectedEof`]
/// if `payload` is shorter than the padded geometry implies.
pub(crate) fn decode(
    payload: &[u8],
    width: u32,
    height: u32,
    budget: &mut Budget,
) -> Result<Frame> {
    if width == 0 || height == 0 {
        return Err(Error::InvalidData("y41p: picture size 0x0 is invalid"));
    }
    let stride = row_bytes(width);
    let total = stride.saturating_mul(height as usize);
    if payload.len() < total {
        return Err(Error::UnexpectedEof);
    }

    let mut frame = Frame::alloc_video(budget, PixFmt::Yuv411p, width, height)?;
    let FrameData::Video { planes, .. } = &mut frame.data else {
        return Err(Error::InvalidData("y41p: expected a video frame"));
    };
    let (y_plane, u_plane, v_plane) = split_planes_mut(planes.as_mut_slice())?;
    let (y_stride, u_stride, v_stride) = (y_plane.stride, u_plane.stride, v_plane.stride);
    let y_buf = y_plane.data.make_mut();
    let u_buf = u_plane.data.make_mut();
    let v_buf = v_plane.data.make_mut();

    let groups = (width as usize).div_ceil(GROUP_PIXELS);
    let width = width as usize;

    for row in 0..height as usize {
        let src_row = row.saturating_mul(stride);
        let y_row = row.saturating_mul(y_stride);
        let u_row = row.saturating_mul(u_stride);
        let v_row = row.saturating_mul(v_stride);
        for g in 0..groups {
            let base = src_row.saturating_add(g.saturating_mul(GROUP_BYTES));
            let chunk = payload
                .get(base..base.saturating_add(GROUP_BYTES))
                .ok_or(Error::UnexpectedEof)?;
            let &[u0, y0, v0, y1, u4, y2, v4, y3, y4, y5, y6, y7] = chunk else {
                return Err(Error::UnexpectedEof);
            };

            let pixel_base = g.saturating_mul(GROUP_PIXELS);
            for (k, y_val) in [y0, y1, y2, y3, y4, y5, y6, y7].into_iter().enumerate() {
                let x = pixel_base.saturating_add(k);
                if x >= width {
                    break;
                }
                let dst = y_buf
                    .get_mut(y_row.saturating_add(x))
                    .ok_or(Error::InvalidData("y41p: Y plane too short"))?;
                *dst = y_val;
            }
            for (p, (u_val, v_val)) in [(u0, v0), (u4, v4)].into_iter().enumerate() {
                let x = pixel_base.saturating_add(p.saturating_mul(4));
                if x >= width {
                    break;
                }
                let cx = x >> 2;
                if let Some(dst) = u_buf.get_mut(u_row.saturating_add(cx)) {
                    *dst = u_val;
                }
                if let Some(dst) = v_buf.get_mut(v_row.saturating_add(cx)) {
                    *dst = v_val;
                }
            }
        }
    }
    Ok(frame)
}

/// Encode a [`PixFmt::Yuv411p`] frame as `y41p`.
///
/// # Errors
/// [`Error::Unsupported`] for any other pixel format, [`Error::InvalidData`]
/// for a `0x0` picture size.
pub(crate) fn encode(frame: &Frame) -> Result<Vec<u8>> {
    let FrameData::Video {
        format,
        width,
        height,
        planes,
    } = &frame.data
    else {
        return Err(Error::InvalidData("y41p: expected a video frame"));
    };
    if *format != PixFmt::Yuv411p {
        return Err(Error::Unsupported("y41p: encoder needs yuv411p input"));
    }
    let (width, height) = (*width, *height);
    if width == 0 || height == 0 {
        return Err(Error::InvalidData("y41p: picture size 0x0 is invalid"));
    }
    if planes.len() != 3 {
        return Err(Error::InvalidData("y41p: expected exactly three planes"));
    }
    let y_plane = planes
        .first()
        .ok_or(Error::InvalidData("y41p: missing Y plane"))?;
    let u_plane = planes
        .get(1)
        .ok_or(Error::InvalidData("y41p: missing U plane"))?;
    let v_plane = planes
        .get(2)
        .ok_or(Error::InvalidData("y41p: missing V plane"))?;
    let (y_stride, u_stride, v_stride) = (y_plane.stride, u_plane.stride, v_plane.stride);
    let (y_buf, u_buf, v_buf) = (
        y_plane.data.as_slice(),
        u_plane.data.as_slice(),
        v_plane.data.as_slice(),
    );

    let stride = row_bytes(width);
    let mut out = vec![0u8; stride.saturating_mul(height as usize)];
    let groups = (width as usize).div_ceil(GROUP_PIXELS);
    let width = width as usize;

    for row in 0..height as usize {
        let dst_row = row.saturating_mul(stride);
        let y_row = row.saturating_mul(y_stride);
        let u_row = row.saturating_mul(u_stride);
        let v_row = row.saturating_mul(v_stride);
        for g in 0..groups {
            let pixel_base = g.saturating_mul(GROUP_PIXELS);
            let mut y = [0u8; GROUP_PIXELS];
            for (k, slot) in y.iter_mut().enumerate() {
                let x = pixel_base.saturating_add(k);
                if x < width {
                    *slot = *y_buf.get(y_row.saturating_add(x)).unwrap_or(&0);
                }
            }
            let mut chroma = [(0u8, 0u8); 2];
            for (p, slot) in chroma.iter_mut().enumerate() {
                let x = pixel_base.saturating_add(p.saturating_mul(4));
                if x < width {
                    let cx = x >> 2;
                    let u_val = *u_buf.get(u_row.saturating_add(cx)).unwrap_or(&0);
                    let v_val = *v_buf.get(v_row.saturating_add(cx)).unwrap_or(&0);
                    *slot = (u_val, v_val);
                }
            }

            let [y0, y1, y2, y3, y4, y5, y6, y7] = y;
            let [(u0, v0), (u4, v4)] = chroma;
            let group = [u0, y0, v0, y1, u4, y2, v4, y3, y4, y5, y6, y7];
            let base = dst_row.saturating_add(g.saturating_mul(GROUP_BYTES));
            let dst = out
                .get_mut(base..base.saturating_add(GROUP_BYTES))
                .ok_or(Error::InvalidData("y41p: encode buffer too short"))?;
            dst.copy_from_slice(&group);
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
    fn a_single_group_decodes_pixel_order_correctly() {
        let group = [10u8, 20, 30, 21, 40, 22, 41, 23, 24, 25, 26, 27];
        let mut budget = Budget::new(Limits::permissive());
        let frame = decode(&group, 8, 1, &mut budget).expect("decode");
        let FrameData::Video { planes, .. } = &frame.data else {
            panic!("video frame")
        };
        let y = planes[0].data.as_slice();
        let u = planes[1].data.as_slice();
        let v = planes[2].data.as_slice();
        assert_eq!(&y[..8], &[20, 21, 22, 23, 24, 25, 26, 27]);
        assert_eq!(&u[..2], &[10, 40]);
        assert_eq!(&v[..2], &[30, 41]);
    }

    #[test]
    fn round_trips_an_exact_group_width() {
        let mut budget = Budget::new(Limits::permissive());
        let width = 8u32;
        let height = 2u32;
        let mut payload = vec![0u8; row_bytes(width) * height as usize];
        for (i, b) in payload.iter_mut().enumerate() {
            *b = (i * 7 % 256) as u8;
        }
        let frame = decode(&payload, width, height, &mut budget).expect("decode");
        let re = encode(&frame).expect("encode");
        assert_eq!(re, payload);
    }

    #[test]
    fn round_trips_a_width_that_is_not_a_multiple_of_eight() {
        let mut budget = Budget::new(Limits::permissive());
        let width = 10u32;
        let height = 1u32;
        let payload = vec![0u8; row_bytes(width)];
        let frame = decode(&payload, width, height, &mut budget).expect("decode");
        let re = encode(&frame).expect("encode");
        assert_eq!(re.len(), payload.len());
    }

    #[test]
    fn zero_size_is_rejected() {
        let mut budget = Budget::new(Limits::permissive());
        assert!(matches!(
            decode(&[], 0, 0, &mut budget).unwrap_err(),
            Error::InvalidData(_)
        ));
    }

    #[test]
    fn encoder_rejects_the_wrong_pixel_format() {
        let mut budget = Budget::new(Limits::permissive());
        let frame = Frame::alloc_video(&mut budget, PixFmt::Yuv420p, 8, 8).expect("alloc");
        assert!(matches!(encode(&frame).unwrap_err(), Error::Unsupported(_)));
    }
}
