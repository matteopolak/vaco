//! Solid-colour raster generation, shared by `pad` (and reusable by any
//! future filter that needs a uniform-colour plane in an arbitrary pixel
//! format).
//!
//! # Why this goes through `vaco-scale` instead of hand-written colour maths
//!
//! This crate has no colour-matrix code of its own, and hand-deriving one
//! just for a border fill risks silently disagreeing with `vaco-scale`'s
//! already-measured RGB↔`YCbCr` behaviour. Instead: build a small RGB24 tile
//! of the requested colour and run it through [`vaco_scale::Scaler`] into the
//! destination format and colour signalling. A uniform image has no spatial
//! frequency content, so every resampling kernel `vaco-scale` implements
//! reproduces the same solid colour in the destination — the resize step is
//! exact for a constant field, not approximate.
//!
//! # Measured: `pad`'s default fill is *limited-range* black
//!
//! ```text
//! ffmpeg -f lavfi -i color=red:s=8x8 -vf format=yuv420p,pad=16:16:4:4 \
//!     -frames:v 1 -f rawvideo -pix_fmt yuv420p - | xxd
//! ```
//!
//! prints `Y=0x10 (16)`, `Cb=Cr=0x80 (128)` for the padded border — not
//! `Y=0`. That is exactly what `ImageSpec::effective_range` computes for
//! `yuv420p` with unspecified signalling (`Limited`, per `vaco-scale`'s own
//! docs), so routing the fill through `Scaler` reproduces it for free. RGB
//! destinations get literal `(0, 0, 0)`, also reproduced for free because RGB
//! defaults to full range.

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FramePool};
use vaco_pixfmt::PixFmt;
use vaco_scale::{ImageSpec, ScaleOptions, Scaler};

/// The smallest tile whose dimensions are valid for every plane of `format` —
/// a multiple of its chroma subsampling factor on both axes, and never zero.
fn aligned_tile_size(format: PixFmt) -> (u32, u32) {
    let (sw, sh) = format.log2_chroma();
    (1u32 << sw, 1u32 << sh)
}

/// Render a `width`×`height` frame of `format` filled with `rgb`
/// (`(r, g, b)`, 8-bit), using `color` for the output's colour signalling.
///
/// # Errors
/// Whatever allocating the frame or building the scaler reports —
/// [`vaco_core::Error::Unsupported`] for a `format` `vaco-scale` cannot
/// target, or [`vaco_core::Error::LimitExceeded`] if the requested size does
/// not fit a `usize`.
pub fn solid_frame(
    pool: &FramePool,
    format: PixFmt,
    width: u32,
    height: u32,
    rgb: (u8, u8, u8),
    color: vaco_color::ColorInfo,
) -> Result<Frame> {
    if width == 0 || height == 0 {
        return Err(Error::InvalidData("solid_frame: zero-sized frame"));
    }
    let (tw, th) = aligned_tile_size(format);
    let mut tile = pool.acquire_video(PixFmt::Rgb24, tw, th)?;
    if let Some(mut plane) = tile.plane_mut(0) {
        for y in 0..plane.rows() {
            if let Some(row) = plane.row_mut(y) {
                for px in row.chunks_exact_mut(3) {
                    if let Some(dst) = px.get_mut(..3) {
                        dst.copy_from_slice(&[rgb.0, rgb.1, rgb.2]);
                    }
                }
            }
        }
    }
    let mut out = pool.acquire_video(format, width, height)?;
    let src_spec = ImageSpec::new(PixFmt::Rgb24, tw, th);
    let dst_spec = ImageSpec::new(format, width, height).with_color(color);
    let mut scaler = Scaler::new(&src_spec, &dst_spec, &ScaleOptions::default())?;
    scaler.scale_frame(&tile, &mut out)?;
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn black_on_yuv420p_is_limited_range() {
        let pool = FramePool::default();
        let frame = solid_frame(
            &pool,
            PixFmt::Yuv420p,
            8,
            8,
            (0, 0, 0),
            vaco_color::ColorInfo::default(),
        )
        .unwrap();
        let y = frame.plane(0).unwrap();
        assert_eq!(y.row(0).unwrap()[0], 16, "measured: yuv420p black is Y=16");
        let u = frame.plane(1).unwrap();
        assert_eq!(u.row(0).unwrap()[0], 128);
        let v = frame.plane(2).unwrap();
        assert_eq!(v.row(0).unwrap()[0], 128);
    }

    #[test]
    fn black_on_rgb24_is_zero() {
        let pool = FramePool::default();
        let frame = solid_frame(
            &pool,
            PixFmt::Rgb24,
            8,
            8,
            (0, 0, 0),
            vaco_color::ColorInfo::default(),
        )
        .unwrap();
        let p = frame.plane(0).unwrap();
        assert_eq!(&p.row(0).unwrap()[0..3], &[0, 0, 0]);
    }

    #[test]
    fn every_pixel_is_uniform() {
        let pool = FramePool::default();
        let frame = solid_frame(
            &pool,
            PixFmt::Yuv420p,
            16,
            10,
            (200, 50, 10),
            vaco_color::ColorInfo::default(),
        )
        .unwrap();
        let y = frame.plane(0).unwrap();
        let first = y.row(0).unwrap()[0];
        for row in 0..y.rows() {
            for &b in y.row(row).unwrap() {
                assert_eq!(b, first);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod recycle_tests {
    use super::*;

    #[test]
    fn solid_frame_is_uniform_even_when_the_pool_recycles_a_dirty_buffer() {
        let pool = FramePool::default();
        // Acquire and dirty an 8x8 gray8 buffer, then drop it so its buffer
        // returns to the pool's free list.
        {
            let mut dirty = pool.acquire_video(PixFmt::Gray8, 8, 8).unwrap();
            if let Some(mut p) = dirty.plane_mut(0) {
                for y in 0..p.rows() {
                    if let Some(row) = p.row_mut(y) {
                        for (x, b) in row.iter_mut().enumerate() {
                            *b = (y * 8 + x) as u8;
                        }
                    }
                }
            }
        }
        let frame = solid_frame(
            &pool,
            PixFmt::Gray8,
            8,
            8,
            (0, 0, 0),
            vaco_color::ColorInfo::default(),
        )
        .unwrap();
        let y = frame.plane(0).unwrap();
        for row in 0..y.rows() {
            for &b in y.row(row).unwrap() {
                assert_eq!(b, 0, "recycled buffer must be fully overwritten");
            }
        }
    }
}
