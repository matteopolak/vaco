//! Format-aware alpha compositing: blend one solid colour over the existing
//! content of a rectangular region, rather than overwriting it — what
//! `overlay`'s colour-fill path and `drawbox`'s translucent boxes both need.
//!
//! # The blend formula
//!
//! For every non-alpha channel: `floor(dst*(1-a) + src*a)`, matching the
//! convention `vaco-filter-draw-vf::drawbox` already measured and pinned
//! (`ffmpeg -h filter=drawbox`, three alpha values, floored not rounded —
//! see that crate's own doc). For a destination alpha channel, the
//! standard Porter-Duff "over" operator applies instead:
//! `dst_a' = src_a + dst_a*(1 - src_a)` — compositing two coverages is not
//! the same operation as interpolating two colour values, so it gets its
//! own formula rather than reusing the channel loop's.

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FrameData};
use vaco_pixfmt::PixFmtFlags;

use crate::color::Rgba;
use crate::rect::Rect;
use crate::sample;
use crate::solid::Solid;

/// Alpha-composite `color` over `rect` (frame-space, clipped internally) of
/// `frame`.
///
/// `color.a == 255` degrades to an overwrite (no read-blend-write per
/// sample); `color.a == 0` is a no-op after the same validation
/// [`crate::fill::fill`] does, so a caller does not need to special-case
/// either extreme itself.
///
/// # Errors
/// Same as [`crate::fill::fill`].
pub fn blend(frame: &mut Frame, rect: Rect, color: Rgba) -> Result<()> {
    let FrameData::Video {
        format,
        width,
        height,
        ..
    } = frame.data
    else {
        return Err(Error::Unsupported(
            "vaco-filter-draw::blend: not a video frame",
        ));
    };
    let solid = Solid::resolve(color, format, frame.color)?;
    let rect = rect.clip(width, height);
    frame.make_writable();

    if color.a == 255 {
        crate::fill::write_solid(frame, format, width, height, rect, &solid);
        return Ok(());
    }
    if color.a == 0 || rect.w == 0 || rect.h == 0 {
        return Ok(());
    }

    let af = crate::color::alpha_fraction(color);
    let desc = format.descriptor();
    let big_endian = format.is_big_endian();
    let has_alpha_plane = desc.flags.contains(PixFmtFlags::ALPHA);

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
            let src = solid.channel.get(logical).copied().unwrap_or(0);
            let is_alpha_channel = has_alpha_plane && logical == 3;
            for y in prect.y..prect.y.saturating_add(prect.h) {
                let Some(row) = plane.row_mut(y as usize) else {
                    continue;
                };
                for x in prect.x..prect.x.saturating_add(prect.w) {
                    let Some(dst) = sample::read(row, x as usize, comp, big_endian) else {
                        continue;
                    };
                    let out = if is_alpha_channel {
                        composite_alpha(src, dst, comp.depth)
                    } else {
                        blend_channel(dst, src, af)
                    };
                    sample::write(row, x as usize, comp, out, big_endian);
                }
            }
        }
    }
    Ok(())
}

/// `floor(dst*(1-a) + src*a)`.
fn blend_channel(dst: u32, src: u32, a: f64) -> u32 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a convex combination of two non-negative code values is itself non-negative and no larger than their max"
    )]
    {
        (f64::from(dst) * (1.0 - a) + f64::from(src) * a).floor() as u32
    }
}

/// `src_a + dst_a*(1 - src_a)`, all normalised to `0.0..=1.0` by `depth` and
/// re-quantised.
fn composite_alpha(src_a: u32, dst_a: u32, depth: u8) -> u32 {
    let max = if depth >= 32 {
        u32::MAX
    } else {
        (1u32 << depth) - 1
    };
    if max == 0 {
        return 0;
    }
    let sa = f64::from(src_a) / f64::from(max);
    let da = f64::from(dst_a) / f64::from(max);
    let out = sa + da * (1.0 - sa);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "out is a convex combination of two 0..=1 fractions, so it is itself 0..=1"
    )]
    {
        (out * f64::from(max)).round() as u32
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_frame::FramePool;
    use vaco_pixfmt::PixFmt;

    #[test]
    fn opaque_blend_matches_a_plain_fill() {
        let pool = FramePool::default();
        let mut a = pool.acquire_video(PixFmt::Gray8, 4, 4).unwrap();
        let mut b = pool.acquire_video(PixFmt::Gray8, 4, 4).unwrap();
        crate::fill::fill(
            &mut a,
            Rect::full(4, 4),
            Rgba {
                r: 100,
                g: 0,
                b: 0,
                a: 255,
            },
        )
        .unwrap();
        blend(
            &mut b,
            Rect::full(4, 4),
            Rgba {
                r: 100,
                g: 0,
                b: 0,
                a: 255,
            },
        )
        .unwrap();
        assert_eq!(a.plane(0).unwrap().row(0), b.plane(0).unwrap().row(0));
    }

    #[test]
    fn fully_transparent_blend_leaves_the_frame_untouched() {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 4, 4).unwrap();
        crate::fill::fill(
            &mut f,
            Rect::full(4, 4),
            Rgba {
                r: 7,
                g: 0,
                b: 0,
                a: 255,
            },
        )
        .unwrap();
        let before = f.plane(0).unwrap().row(0).unwrap()[0];
        blend(
            &mut f,
            Rect::full(4, 4),
            Rgba {
                r: 250,
                g: 0,
                b: 0,
                a: 0,
            },
        )
        .unwrap();
        assert_eq!(f.plane(0).unwrap().row(0).unwrap()[0], before);
    }

    #[test]
    fn half_alpha_floors_the_interpolation() {
        assert_eq!(blend_channel(0, 255, 0.5), 127);
        assert_eq!(blend_channel(10, 20, 0.5), 15);
    }

    #[test]
    fn destination_alpha_composites_with_the_over_operator() {
        // Fully opaque destination stays fully opaque no matter what is
        // drawn over it, at any source alpha.
        assert_eq!(composite_alpha(128, 255, 8), 255);
        // Nothing over nothing is nothing.
        assert_eq!(composite_alpha(0, 0, 8), 0);
    }

    #[test]
    fn blending_over_yuva_composites_the_alpha_plane() {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Yuva420p, 4, 4).unwrap();
        // Start fully transparent.
        crate::fill::fill(
            &mut f,
            Rect::full(4, 4),
            Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 0,
            },
        )
        .unwrap();
        blend(
            &mut f,
            Rect::full(4, 4),
            Rgba {
                r: 255,
                g: 0,
                b: 0,
                a: 128,
            },
        )
        .unwrap();
        let a = f.plane(3).unwrap().row(0).unwrap()[0];
        assert!(
            a > 0,
            "compositing a translucent colour over transparent must raise alpha"
        );
    }
}
