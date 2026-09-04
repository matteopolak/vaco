//! Simple-text subtitle rendering (GitHub #486 / FT-5.1's text half): SRT,
//! `WebVTT`, `MicroDVD`, SAMI and plain `SubStation` with no override tags are
//! "a layout-and-draw job the SS6.1 stack handles directly" (plan 16
//! SS6.3) — bottom-centred, word-wrapped, white-on-black-outline, the
//! universal simple-subtitle convention no format-specific styling
//! overrides.

use vaco_core::{Error, Result};
use vaco_filter_text::{Anchor, TextRenderer, TextStyle, Wrap, mask};
use vaco_frame::{Frame, FrameData};

/// Default styling for a subtitle format that carries no styling of its
/// own: white fill, a thin black outline for legibility over any
/// background, bottom-centred with a margin proportional to frame height.
#[derive(Debug)]
pub struct SimpleTextStyle {
    pub size_px: f32,
    pub outline_px: u32,
    pub margin_bottom: u32,
}

impl SimpleTextStyle {
    /// A reasonable default for a frame of the given height: ~5% of the
    /// height for font size, matching the visual weight most authored
    /// simple-text subtitles are timed against.
    #[must_use]
    pub fn for_frame_height(height: u32) -> Self {
        let size_px = (f64::from(height) * 0.05).max(12.0) as f32;
        Self {
            size_px,
            outline_px: 2,
            margin_bottom: (f64::from(height) * 0.04) as u32,
        }
    }
}

/// Composite `text` (already newline-split where the source format marks a
/// line break) bottom-centred onto `frame`.
///
/// # Errors
/// [`Error::Unsupported`] for a non-video frame or an unsupported pixel
/// format; [`vaco_core::Error::LimitExceeded`] if rasterisation exceeds
/// `renderer`'s own budget.
pub fn composite_simple_text(
    renderer: &mut TextRenderer,
    frame: &mut Frame,
    text: &str,
    style: &SimpleTextStyle,
) -> Result<()> {
    let FrameData::Video { width, height, .. } = frame.data else {
        return Err(Error::Unsupported(
            "vaco-filter-subtitle::text: not a video frame",
        ));
    };
    if text.trim().is_empty() {
        return Ok(());
    }
    let text_style = TextStyle {
        size_px: style.size_px,
        color: vaco_core::Rgba {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        },
        ..TextStyle::default()
    };
    let wrap = Wrap::Word(f32::from(u16::try_from(width).unwrap_or(u16::MAX)) * 0.9);
    let layout = renderer.layout(text, &text_style, wrap);
    if layout.is_empty() {
        return Ok(());
    }
    let target_x = f64::from(width) / 2.0;
    let target_y = f64::from(height) - f64::from(style.margin_bottom);
    let (ox, oy) = Anchor::BottomCenter.place(
        target_x as f32,
        target_y as f32,
        layout.width as f32,
        layout.height as f32,
    );
    let origin = (ox.round() as i32, oy.round() as i32);

    let base_mask = renderer.rasterise(&layout, origin)?;
    let color_info = frame.color;
    if style.outline_px > 0 {
        let dilated = base_mask.dilate(renderer.budget_mut(), style.outline_px)?;
        mask::composite(
            frame,
            &dilated,
            vaco_core::Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 255,
            },
            color_info,
        )?;
    }
    mask::composite(frame, &base_mask, text_style.color, color_info)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_frame::FramePool;
    use vaco_pixfmt::PixFmt;

    #[test]
    fn renders_visible_coverage_without_panicking() {
        let mut renderer = TextRenderer::new();
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Yuv420p, 320, 240).unwrap();
        vaco_filter_draw::fill::fill(
            &mut f,
            vaco_filter_draw::rect::Rect::full(320, 240),
            vaco_core::Rgba::BLACK,
        )
        .unwrap();
        let style = SimpleTextStyle::for_frame_height(240);
        composite_simple_text(&mut renderer, &mut f, "Hello, world!", &style).unwrap();
        let plane = f.plane(0).unwrap();
        assert!(
            (0..plane.rows()).any(|y| plane.row(y).is_some_and(|row| row.iter().any(|&v| v > 16))),
            "simple subtitle text must change at least one black luma sample"
        );
    }

    #[test]
    fn empty_text_is_a_no_op() {
        let mut renderer = TextRenderer::new();
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Yuv420p, 320, 240).unwrap();
        let style = SimpleTextStyle::for_frame_height(240);
        composite_simple_text(&mut renderer, &mut f, "   ", &style).unwrap();
    }

    #[test]
    fn frame_height_scales_the_default_font_size() {
        let small = SimpleTextStyle::for_frame_height(240);
        let large = SimpleTextStyle::for_frame_height(1080);
        assert!(large.size_px > small.size_px);
    }
}
