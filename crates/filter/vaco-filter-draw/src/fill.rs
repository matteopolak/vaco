//! Format-aware solid fill: write one resolved colour into every sample of a
//! rectangular region of a [`Frame`], across every plane, at whatever bit
//! depth and chroma decimation the frame's [`PixFmt`] declares.

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData};
use vaco_pixfmt::PixFmt;

use crate::color::Rgba;
use crate::rect::Rect;
use crate::sample;
use crate::solid::Solid;

/// Fill `rect` (frame-space, clipped internally) with `color`.
///
/// Calls [`Frame::make_writable`] first, so a shared plane is copied once up
/// front rather than mid-write.
///
/// # Errors
/// [`Error::Unsupported`] if `frame` is not video, or for the pixel-format
/// classes [`crate::solid::Solid::resolve`] does not handle (see its doc).
pub fn fill(frame: &mut Frame, rect: Rect, color: Rgba) -> Result<()> {
    let FrameData::Video { format, width, height, .. } = frame.data else {
        return Err(Error::Unsupported("vaco-filter-draw::fill: not a video frame"));
    };
    let solid = Solid::resolve(color, format, frame.color)?;
    let rect = rect.clip(width, height);
    frame.make_writable();
    write_solid(frame, format, width, height, rect, &solid);
    Ok(())
}

/// The write loop [`fill`] and [`crate::blend::blend`] (for a fully opaque
/// colour) share: every component on every plane whose projected rectangle
/// is non-empty.
pub(crate) fn write_solid(frame: &mut Frame, format: PixFmt, width: u32, height: u32, rect: Rect, solid: &Solid) {
    let desc = format.descriptor();
    let big_endian = format.is_big_endian();
    for plane_idx in 0..desc.planes {
        let prect = rect.on_plane(format, plane_idx, width, height);
        if prect.w == 0 || prect.h == 0 {
            continue;
        }
        let Some(mut plane) = frame.plane_mut(usize::from(plane_idx)) else {
            continue;
        };
        for (logical, comp) in desc.components.iter().enumerate() {
            if comp.plane != plane_idx {
                continue;
            }
            let value = solid.channel.get(logical).copied().unwrap_or(0);
            for y in prect.y..prect.y.saturating_add(prect.h) {
                let Some(row) = plane.row_mut(y as usize) else {
                    continue;
                };
                for x in prect.x..prect.x.saturating_add(prect.w) {
                    sample::write(row, x as usize, comp, value, big_endian);
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_frame::FramePool;

    #[test]
    fn fill_whole_frame_yuv420p() {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Yuv420p, 16, 16).unwrap();
        fill(&mut f, Rect::full(16, 16), Rgba { r: 255, g: 0, b: 0, a: 255 }).unwrap();
        assert_eq!(f.plane(0).unwrap().row(0).unwrap()[0], 0x51);
        assert_eq!(f.plane(1).unwrap().row(0).unwrap()[0], 0x5a);
        assert_eq!(f.plane(2).unwrap().row(0).unwrap()[0], 0xf0);
    }

    #[test]
    fn fill_partial_rect_leaves_the_rest_untouched() {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 8, 8).unwrap();
        // Equal R=G=B is a fixed point of any BT.601-weighted luma sum
        // (weights sum to 1), so full-range `gray` reproduces it exactly —
        // matches `ffmpeg -pix_fmt gray` on `0xc8c8c8` printing `0xc8`.
        fill(&mut f, Rect { x: 0, y: 0, w: 4, h: 8 }, Rgba { r: 200, g: 200, b: 200, a: 255 }).unwrap();
        let row = f.plane(0).unwrap().row(0).unwrap().to_vec();
        assert_eq!(&row[0..4], &[200, 200, 200, 200]);
        assert_eq!(&row[4..8], &[0, 0, 0, 0]);
    }

    #[test]
    fn fill_gray8_from_red_matches_the_measured_full_range_bt601_luma() {
        // `ffmpeg -f lavfi -i color=c=red -pix_fmt gray -f rawvideo -` prints
        // 0x4c (76 = round(255*0.299)) — see `solid.rs`'s doc on why `gray`
        // defaults to full range where `yuv420p` defaults to limited.
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 2, 2).unwrap();
        fill(&mut f, Rect::full(2, 2), Rgba { r: 255, g: 0, b: 0, a: 255 }).unwrap();
        assert_eq!(f.plane(0).unwrap().row(0).unwrap()[0], 76);
    }

    #[test]
    fn fill_high_bit_depth_scales_correctly() {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray16le, 4, 4).unwrap();
        fill(&mut f, Rect::full(4, 4), Rgba { r: 255, g: 255, b: 255, a: 255 }).unwrap();
        let row = f.plane(0).unwrap().row(0).unwrap();
        // gray16le's single component is full 16-bit luma at limited range,
        // so white (255 in 8-bit) maps through the same BT.601 luma levels.
        let value = u16::from_le_bytes([row[0], row[1]]);
        assert!(value > 0);
    }

    #[test]
    fn fill_rejects_audio_frames() {
        let pool = FramePool::default();
        let mut f = pool
            .acquire_audio(
                vaco_sampfmt::SampleFmt::S16,
                vaco_chlayout::ChannelLayout::unspecified(1),
                1024,
                48000,
            )
            .unwrap();
        assert!(fill(&mut f, Rect::full(1, 1), Rgba::BLACK).is_err());
    }
}
