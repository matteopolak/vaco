//! Bitmap subtitle compositing (GitHub #486 / FT-5.1's bitmap half): DVB,
//! `VobSub`, PGS decode to a palette-index bitmap plus a per-index RGBA
//! palette (`vaco_frame::SubtitleContent::Bitmap`) — there is no
//! typesetting problem here, only a positioned alpha-composite, per plan 16
//! SS6.3's own framing.
//!
//! Not [`vaco_filter_text::mask::composite`]: that module tints **one**
//! colour by a coverage buffer, and a bitmap subtitle's pixels each carry
//! their **own** colour from the palette — a different shape, built here on
//! the same underlying [`vaco_filter_draw`] primitives (`sample`/`solid`)
//! rather than forcing one interface to cover both.

use vaco_color::ColorInfo;
use vaco_core::{Error, Result};
use vaco_filter_draw::rect::Rect;
use vaco_filter_draw::sample;
use vaco_filter_draw::solid::Solid;
use vaco_frame::{SubtitleContent, SubtitleRect};
use vaco_frame::{Frame, FrameData};
use vaco_pixfmt::PixFmtFlags;

/// Composite one bitmap [`SubtitleRect`] onto `frame` at its own `(x, y)`.
///
/// # Errors
/// [`Error::Unsupported`] if `frame` is not video, `rect` is not a bitmap,
/// or the destination pixel format is one [`Solid::resolve`] rejects.
pub fn composite_bitmap(frame: &mut Frame, rect: &SubtitleRect) -> Result<()> {
    let SubtitleContent::Bitmap { stride, data, palette } = &rect.content else {
        return Err(Error::Unsupported("vaco-filter-subtitle::bitmap: rect is not a bitmap"));
    };
    let FrameData::Video { format, width, height, .. } = frame.data else {
        return Err(Error::Unsupported("vaco-filter-subtitle::bitmap: not a video frame"));
    };
    let color_info: ColorInfo = frame.color;
    let resolved: Vec<Solid> = palette
        .iter()
        .map(|&[r, g, b, a]| Solid::resolve(vaco_core::Rgba { r, g, b, a }, format, color_info))
        .collect::<Result<_>>()?;
    // The palette's own alpha, separately from `Solid`: `Solid::resolve`
    // only fills a destination alpha *channel* when the format has one
    // (`vaco_filter_draw::solid`'s own doc), so a format with no alpha
    // plane (the common case: `yuv420p`, `gray8`, ...) would otherwise
    // read back `0` here and treat every pixel as fully transparent.
    let alphas: Vec<u8> = palette.iter().map(|&[_, _, _, a]| a).collect();

    let dst = Rect { x: rect.x, y: rect.y, w: rect.w, h: rect.h }.clip(width, height);
    if dst.w == 0 || dst.h == 0 {
        return Ok(());
    }
    frame.make_writable();

    let desc = format.descriptor();
    let big_endian = format.is_big_endian();
    let has_alpha_plane = desc.flags.contains(PixFmtFlags::ALPHA);
    let (log2_w, log2_h) = format.log2_chroma();

    for plane_idx in 0..desc.planes {
        let prect = dst.on_plane(format, plane_idx, width, height);
        if prect.w == 0 || prect.h == 0 {
            continue;
        }
        let is_chroma = desc
            .components
            .iter()
            .enumerate()
            .any(|(logical, c)| c.plane == plane_idx && (logical == 1 || logical == 2))
            && !desc.flags.contains(PixFmtFlags::RGB);
        let (sw, sh) = if is_chroma { (log2_w, log2_h) } else { (0, 0) };

        let Some(mut plane) = frame.plane_mut(usize::from(plane_idx)) else { continue };
        for (logical, comp) in desc.components.iter().enumerate() {
            if comp.plane != plane_idx {
                continue;
            }
            let is_alpha_channel = has_alpha_plane && logical == 3;
            for py in prect.y..prect.y.saturating_add(prect.h) {
                let Some(row) = plane.row_mut(py as usize) else { continue };
                for px in prect.x..prect.x.saturating_add(prect.w) {
                    // Nearest-sample the source bitmap for a chroma plane
                    // (simpler than the box-average this crate's text path
                    // uses; a documented, minor divergence for this format
                    // family, which is already palette-quantised).
                    let src_x = (px << sw).saturating_sub(rect.x);
                    let src_y = (py << sh).saturating_sub(rect.y);
                    if src_x >= rect.w || src_y >= rect.h {
                        continue;
                    }
                    let Some(&index) = data.as_slice().get(src_y as usize * stride + src_x as usize) else { continue };
                    let Some(solid) = resolved.get(index as usize) else { continue };
                    let src = solid.channel.get(logical).copied().unwrap_or(0);
                    let alpha8 = alphas.get(index as usize).copied().unwrap_or(0);
                    let a = f64::from(alpha8) / 255.0;
                    if a <= 0.0 {
                        continue;
                    }
                    let Some(dst_v) = sample::read(row, px as usize, comp, big_endian) else { continue };
                    let out = if is_alpha_channel {
                        let max = if comp.depth >= 32 { u32::MAX } else { (1u32 << comp.depth) - 1 };
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a is a 0..=1 fraction and max bounds the result")]
                        let src_a_scaled = (a * f64::from(max)).round() as u32;
                        composite_alpha(src_a_scaled, dst_v, comp.depth)
                    } else {
                        blend_channel(dst_v, src, a)
                    };
                    sample::write(row, px as usize, comp, out, big_endian);
                }
            }
        }
    }
    Ok(())
}

fn blend_channel(dst: u32, src: u32, a: f64) -> u32 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a convex combination of two bounded code values")]
    {
        (f64::from(dst) * (1.0 - a) + f64::from(src) * a).floor() as u32
    }
}

fn composite_alpha(src_a: u32, dst_a: u32, depth: u8) -> u32 {
    let max = if depth >= 32 { u32::MAX } else { (1u32 << depth) - 1 };
    if max == 0 {
        return 0;
    }
    let sa = f64::from(src_a) / f64::from(max);
    let da = f64::from(dst_a) / f64::from(max);
    let out = sa + da * (1.0 - sa);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "out is a convex combination of two 0..=1 fractions")]
    {
        (out * f64::from(max)).round() as u32
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_frame::FramePool;
    use vaco_limits::{Budget, Limits};
    use vaco_pixfmt::PixFmt;

    #[test]
    fn opaque_bitmap_paints_its_own_colour() {
        let mut budget = Budget::new(Limits::default());
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 8, 8).unwrap();
        // rect at (2,2), 2x2: row 0 = index 0 (transparent black), row 1 =
        // index 1 (opaque white) -> only frame row 3 should light up.
        let pixels = [0u8, 0, 1, 1];
        let palette = vec![[0, 0, 0, 0], [255, 255, 255, 255]];
        let rect = SubtitleRect::bitmap(&mut budget, 2, 2, 2, 2, false, 2, &pixels, palette).unwrap();
        composite_bitmap(&mut f, &rect).unwrap();
        let row2 = f.plane(0).unwrap().row(2).unwrap();
        let row3 = f.plane(0).unwrap().row(3).unwrap();
        assert_eq!(row2[2], 0, "the transparent row must not paint");
        assert_eq!(row3[2], 255, "the opaque row must paint white");
    }

    #[test]
    fn transparent_index_leaves_the_frame_untouched() {
        let mut budget = Budget::new(Limits::default());
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 4, 4).unwrap();
        vaco_filter_draw::fill::fill(&mut f, Rect::full(4, 4), vaco_core::Rgba { r: 7, g: 0, b: 0, a: 255 }).unwrap();
        let before = f.plane(0).unwrap().row(0).unwrap()[0];
        let pixels = [0u8];
        let palette = vec![[255, 255, 255, 0]];
        let rect = SubtitleRect::bitmap(&mut budget, 0, 0, 1, 1, false, 1, &pixels, palette).unwrap();
        composite_bitmap(&mut f, &rect).unwrap();
        assert_eq!(f.plane(0).unwrap().row(0).unwrap()[0], before);
    }

    #[test]
    fn out_of_frame_rect_clips_cleanly() {
        let mut budget = Budget::new(Limits::default());
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 4, 4).unwrap();
        let pixels = [1u8];
        let palette = vec![[0, 0, 0, 0], [255, 255, 255, 255]];
        let rect = SubtitleRect::bitmap(&mut budget, 100, 100, 1, 1, false, 1, &pixels, palette).unwrap();
        composite_bitmap(&mut f, &rect).unwrap();
    }

    #[test]
    fn non_bitmap_content_is_rejected() {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 4, 4).unwrap();
        let rect = SubtitleRect::text(0, 0, 0, 0, false, "hi");
        assert!(composite_bitmap(&mut f, &rect).is_err());
    }
}
