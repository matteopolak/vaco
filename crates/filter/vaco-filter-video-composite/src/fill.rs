//! Solid-colour raster generation for [`crate::rotate`]'s corners.
//!
//! Independently written for this crate: `vaco-filter-video-geometry::fill`
//! solves the identical problem for `pad`'s border, but it is
//! `pub(crate)` there and this crate does not own that crate. Same approach
//! as that module (documented there, and re-derived here rather than
//! guessed): build a small RGB24 tile of the requested colour and run it
//! through [`vaco_scale::Scaler`] into the destination format and colour
//! signalling, because a uniform image has no spatial frequency content, so
//! every resampling kernel reproduces the same solid colour exactly.
//!
//! # Measured: `rotate`'s default `fillcolor=black` is limited-range black
//!
//! ```sh
//! ffmpeg -f lavfi -i "color=white:100x50,format=yuv420p" \
//!   -vf "rotate=PI/4:ow=rotw(PI/4):oh=roth(PI/4)" -f rawvideo -pix_fmt yuv420p - | xxd
//! ```
//!
//! prints `Y=0x10` (16) for a corner pixel the source never reaches, and
//! `Y=235` for the rotated white source — both exactly what `pad`'s fill
//! measurement found for the same `(0, 0, 0)`/`(253, 253, 253)`-style colour
//! pair, which is what going through the same colour-signalling path
//! predicts.

use vaco_core::Result;
use vaco_frame::{Frame, FramePool};
use vaco_pixfmt::PixFmt;
use vaco_scale::{ImageSpec, ScaleOptions, Scaler};

/// The smallest tile whose dimensions are valid for every plane of `format`.
fn aligned_tile_size(format: PixFmt) -> (u32, u32) {
    let (sw, sh) = format.log2_chroma();
    (1u32 << sw, 1u32 << sh)
}

/// Render a `width`x`height` frame of `format` filled with `rgb`, using
/// `color` for the output's colour signalling.
///
/// # Errors
/// Whatever allocating the frame or building the scaler reports.
pub fn solid_frame(
    pool: &FramePool,
    format: PixFmt,
    width: u32,
    height: u32,
    rgb: (u8, u8, u8),
    color: vaco_color::ColorInfo,
) -> Result<Frame> {
    if width == 0 || height == 0 {
        return Err(vaco_core::Error::InvalidData(
            "solid_frame: zero-sized frame",
        ));
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
}
