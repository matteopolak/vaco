//! An 8-bit coverage mask, and compositing it into a real [`Frame`].
//!
//! [`crate::TextRenderer::rasterise`] produces one of these per drawn block;
//! `drawtext` and `vaco-ass` both composite it the same way, which is what
//! this module is for — one home for "tint a coverage buffer and
//! alpha-composite it", built on [`vaco_filter_draw`]'s already-measured
//! [`vaco_filter_draw::sample`]/[`vaco_filter_draw::solid`]/
//! [`vaco_filter_draw::rect`] primitives rather than a second copy of them.
//!
//! `vaco_filter_draw::blend` only blends a *uniform* alpha over a rectangle
//! ([`vaco_filter_draw::blend::blend`]'s own doc), which is not this
//! module's shape — text coverage varies per pixel — so the blend formula
//! (floor for a colour channel, Porter-Duff "over" for a destination alpha
//! channel) is reproduced here rather than reached through that crate: both
//! are one-line arithmetic, not logic worth sharing as a unit, and
//! `vaco-filter-text` does not own `vaco-filter-draw` to add a mask-shaped
//! entry point to it.

use vaco_color::ColorInfo;
use vaco_core::{Error, Result};
use vaco_filter_draw::Rgba;
use vaco_filter_draw::rect::Rect;
use vaco_filter_draw::sample;
use vaco_filter_draw::solid::Solid;
use vaco_frame::{Frame, FrameData};
use vaco_limits::Budget;
use vaco_pixfmt::PixFmtFlags;

/// An 8-bit coverage buffer positioned in frame space. `coverage[y*w+x]` is
/// `0` (nothing drawn) to `255` (fully covered).
#[derive(Debug, Clone)]
pub struct AlphaMask {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub coverage: Vec<u8>,
}

impl AlphaMask {
    /// An all-zero mask of the given size, allocated through `budget` — the
    /// size comes from shaped text (attacker-controlled string length times
    /// font size), so the budget check must run before the allocation, not
    /// after.
    ///
    /// # Errors
    /// [`vaco_core::Error::LimitExceeded`] if `w * h` exceeds the budget.
    #[allow(
        clippy::many_single_char_names,
        reason = "x/y/w/h is this crate's and vaco-filter-draw::rect's established rectangle vocabulary"
    )]
    pub fn blank(budget: &mut Budget, x: i32, y: i32, w: u32, h: u32) -> Result<Self> {
        let n = usize::try_from(w)
            .unwrap_or(0)
            .saturating_mul(usize::try_from(h).unwrap_or(0));
        let coverage = budget.alloc::<u8>(n)?;
        Ok(Self {
            x,
            y,
            w,
            h,
            coverage,
        })
    }

    #[must_use]
    pub fn coverage_at(&self, px: i32, py: i32) -> u8 {
        if px < self.x || py < self.y {
            return 0;
        }
        let (dx, dy) = (px - self.x, py - self.y);
        let (Ok(dx), Ok(dy)) = (usize::try_from(dx), usize::try_from(dy)) else {
            return 0;
        };
        if dx >= self.w as usize || dy >= self.h as usize {
            return 0;
        }
        self.coverage
            .get(dy * self.w as usize + dx)
            .copied()
            .unwrap_or(0)
    }

    /// Blit `src` onto this mask at `(dst_x, dst_y)`, taking the brighter of
    /// the two coverages where they overlap (how two glyphs, or a glyph and
    /// its own already-drawn border, combine — never darker than either
    /// alone).
    pub fn blit_max(&mut self, src: &AlphaMask, dst_x: i32, dst_y: i32) {
        for sy in 0..src.h {
            for sx in 0..src.w {
                let Some(&v) = src.coverage.get((sy * src.w + sx) as usize) else {
                    continue;
                };
                if v == 0 {
                    continue;
                }
                let px = dst_x + i32::try_from(sx).unwrap_or(0);
                let py = dst_y + i32::try_from(sy).unwrap_or(0);
                if px < self.x || py < self.y {
                    continue;
                }
                let (dx, dy) = (px - self.x, py - self.y);
                let (Ok(dx), Ok(dy)) = (usize::try_from(dx), usize::try_from(dy)) else {
                    continue;
                };
                if dx >= self.w as usize || dy >= self.h as usize {
                    continue;
                }
                if let Some(slot) = self.coverage.get_mut(dy * self.w as usize + dx) {
                    *slot = (*slot).max(v);
                }
            }
        }
    }

    /// A box-blurred copy — libass's own `\blur`/`\be` both work on the
    /// rasterised alpha bitmap, and a box blur run a few times approximates
    /// a Gaussian well enough for this purpose (three passes is the
    /// standard trick; `passes` lets a caller pick).
    ///
    /// # Errors
    /// [`vaco_core::Error::LimitExceeded`] if the (unchanged) size exceeds
    /// the budget.
    pub fn box_blur(&self, budget: &mut Budget, radius: u32, passes: u32) -> Result<Self> {
        if radius == 0 || self.w == 0 || self.h == 0 {
            return Self::blank_from(budget, self).map(|mut m| {
                m.coverage.copy_from_slice(&self.coverage);
                m
            });
        }
        let mut cur = self.coverage.clone();
        for _ in 0..passes.max(1) {
            cur = box_blur_pass(&cur, self.w, self.h, radius);
        }
        let mut out = Self::blank_from(budget, self)?;
        out.coverage.copy_from_slice(&cur);
        Ok(out)
    }

    fn blank_from(budget: &mut Budget, like: &Self) -> Result<Self> {
        Self::blank(budget, like.x, like.y, like.w, like.h)
    }

    /// A copy grown by `radius` pixels on every side and max-filtered over a
    /// disc of that radius — the approximation this crate uses for a glyph
    /// outline (`drawtext`'s `borderw`, ASS's `\bord`), rather than a true
    /// stroke of the glyph's own vector outline: visually correct (the
    /// border wraps the visible glyph shape and grows with `radius`) but not
    /// libass's shape at small radii on sharp corners, which is the
    /// documented divergence for this feature.
    ///
    /// # Errors
    /// [`vaco_core::Error::LimitExceeded`] if the grown size exceeds budget.
    pub fn dilate(&self, budget: &mut Budget, radius: u32) -> Result<Self> {
        if radius == 0 {
            let mut out = Self::blank_from(budget, self)?;
            out.coverage.copy_from_slice(&self.coverage);
            return Ok(out);
        }
        let new_w = self.w.saturating_add(2 * radius);
        let new_h = self.h.saturating_add(2 * radius);
        let r = i32::try_from(radius).unwrap_or(i32::MAX);
        let mut out = Self::blank(
            budget,
            self.x.saturating_sub(r),
            self.y.saturating_sub(r),
            new_w,
            new_h,
        )?;
        let (new_wi, new_hi, self_wi, self_hi) = (
            i32::try_from(new_w).unwrap_or(0),
            i32::try_from(new_h).unwrap_or(0),
            i32::try_from(self.w).unwrap_or(0),
            i32::try_from(self.h).unwrap_or(0),
        );
        let r2 = r * r;
        for oy in 0..new_hi {
            for ox in 0..new_wi {
                let mut best: u8 = 0;
                'search: for dy in -r..=r {
                    for dx in -r..=r {
                        if dx * dx + dy * dy > r2 {
                            continue;
                        }
                        let sx = ox - r + dx;
                        let sy = oy - r + dy;
                        if sx < 0 || sy < 0 || sx >= self_wi || sy >= self_hi {
                            continue;
                        }
                        let Some(&v) = self
                            .coverage
                            .get(sy as usize * self.w as usize + sx as usize)
                        else {
                            continue;
                        };
                        if v > best {
                            best = v;
                            if best == 255 {
                                break 'search;
                            }
                        }
                    }
                }
                if let Some(slot) = out
                    .coverage
                    .get_mut(oy as usize * new_w as usize + ox as usize)
                {
                    *slot = best;
                }
            }
        }
        Ok(out)
    }

    /// Translate the mask's own placement without touching its content —
    /// how a shadow (`shadowx`/`shadowy`, `\shad`) reuses an already-
    /// rasterised mask instead of rasterising twice.
    #[must_use]
    pub fn translated(&self, dx: i32, dy: i32) -> Self {
        Self {
            x: self.x + dx,
            y: self.y + dy,
            w: self.w,
            h: self.h,
            coverage: self.coverage.clone(),
        }
    }
}

/// `sum / count`, rounding down, `0` when `count` is `0` — the one division
/// this module needs, named once so `clippy::integer_division` (denied
/// workspace-wide) has a single, obviously-correct place to point at.
fn average(sum: u32, count: u32) -> u8 {
    u8::try_from(sum.checked_div(count).unwrap_or(0)).unwrap_or(255)
}

fn box_blur_pass(src: &[u8], w: u32, h: u32, radius: u32) -> Vec<u8> {
    let (wi, hi) = (i32::try_from(w).unwrap_or(0), i32::try_from(h).unwrap_or(0));
    let wu = w as usize;
    let r = i32::try_from(radius).unwrap_or(0);
    let mut horiz = vec![0u8; src.len()];
    for y in 0..hi {
        for x in 0..wi {
            let mut sum: u32 = 0;
            let mut count: u32 = 0;
            for k in -r..=r {
                let sx = x + k;
                if sx < 0 || sx >= wi {
                    continue;
                }
                if let Some(&v) = src.get(y as usize * wu + sx as usize) {
                    sum += u32::from(v);
                    count += 1;
                }
            }
            if let Some(slot) = horiz.get_mut(y as usize * wu + x as usize) {
                *slot = average(sum, count);
            }
        }
    }
    let mut out = vec![0u8; src.len()];
    for x in 0..wi {
        for y in 0..hi {
            let mut sum: u32 = 0;
            let mut count: u32 = 0;
            for k in -r..=r {
                let sy = y + k;
                if sy < 0 || sy >= hi {
                    continue;
                }
                if let Some(&v) = horiz.get(sy as usize * wu + x as usize) {
                    sum += u32::from(v);
                    count += 1;
                }
            }
            if let Some(slot) = out.get_mut(y as usize * wu + x as usize) {
                *slot = average(sum, count);
            }
        }
    }
    out
}

/// Tint `mask` with `color` and alpha-composite it into `frame`, format- and
/// subsampling-aware. Chroma planes sample the mask by box-averaging the
/// full-resolution coverage under each decimated pixel, so an antialiased
/// glyph edge does not alias on 4:2:0 chroma.
///
/// # Errors
/// [`Error::Unsupported`] for a non-video frame or an unsupported pixel
/// format (see [`Solid::resolve`]).
pub fn composite(
    frame: &mut Frame,
    mask: &AlphaMask,
    color: Rgba,
    color_info: ColorInfo,
) -> Result<()> {
    let FrameData::Video {
        format,
        width,
        height,
        ..
    } = frame.data
    else {
        return Err(Error::Unsupported(
            "vaco-filter-text::mask: not a video frame",
        ));
    };
    if mask.w == 0 || mask.h == 0 || color.a == 0 {
        return Ok(());
    }
    let solid = Solid::resolve(color, format, color_info)?;
    let rect = Rect {
        x: mask.x.max(0) as u32,
        y: mask.y.max(0) as u32,
        w: mask.w,
        h: mask.h,
    }
    .clip(width, height);
    if rect.w == 0 || rect.h == 0 {
        return Ok(());
    }
    frame.make_writable();

    let desc = format.descriptor();
    let big_endian = format.is_big_endian();
    let has_alpha_plane = desc.flags.contains(PixFmtFlags::ALPHA);
    let af = f64::from(color.a) / 255.0;
    let (log2_w, log2_h) = format.log2_chroma();

    for plane_idx in 0..desc.planes {
        let prect = rect.on_plane(format, plane_idx, width, height);
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

        let Some(mut plane) = frame.plane_mut(usize::from(plane_idx)) else {
            continue;
        };
        for (logical, comp) in desc.components.iter().enumerate() {
            if comp.plane != plane_idx {
                continue;
            }
            let src = solid.channel.get(logical).copied().unwrap_or(0);
            let is_alpha_channel = has_alpha_plane && logical == 3;
            for py in prect.y..prect.y.saturating_add(prect.h) {
                let Some(row) = plane.row_mut(py as usize) else {
                    continue;
                };
                for px in prect.x..prect.x.saturating_add(prect.w) {
                    let full_x0 = px << sw;
                    let full_y0 = py << sh;
                    let full_x1 = full_x0 + (1u32 << sw) - 1;
                    let full_y1 = full_y0 + (1u32 << sh) - 1;
                    let mut sum: u32 = 0;
                    let mut n: u32 = 0;
                    for fy in full_y0..=full_y1 {
                        for fx in full_x0..=full_x1 {
                            sum += u32::from(mask.coverage_at(
                                i32::try_from(fx).unwrap_or(i32::MAX),
                                i32::try_from(fy).unwrap_or(i32::MAX),
                            ));
                            n += 1;
                        }
                    }
                    let coverage = average(sum, n);
                    if coverage == 0 {
                        continue;
                    }
                    let a = af * (f64::from(coverage) / 255.0);
                    let Some(dst) = sample::read(row, px as usize, comp, big_endian) else {
                        continue;
                    };
                    let out = if is_alpha_channel {
                        composite_alpha(src, dst, comp.depth, a)
                    } else {
                        blend_channel(dst, src, a)
                    };
                    sample::write(row, px as usize, comp, out, big_endian);
                }
            }
        }
    }
    Ok(())
}

/// `floor(dst*(1-a) + src*a)` — matches
/// `vaco_filter_draw::blend`'s measured, pinned formula.
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

/// Porter-Duff "over" on a destination alpha channel: `sa + da*(1-sa)`.
fn composite_alpha(src_a: u32, dst_a: u32, depth: u8, coverage_a: f64) -> u32 {
    let max = if depth >= 32 {
        u32::MAX
    } else {
        (1u32 << depth) - 1
    };
    if max == 0 {
        return 0;
    }
    let sa = (f64::from(src_a) / f64::from(max)) * coverage_a;
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
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_frame::FramePool;
    use vaco_limits::Limits;
    use vaco_pixfmt::PixFmt;

    fn full_mask(budget: &mut Budget, w: u32, h: u32) -> AlphaMask {
        let mut m = AlphaMask::blank(budget, 0, 0, w, h).unwrap();
        m.coverage.iter_mut().for_each(|c| *c = 255);
        m
    }

    #[test]
    fn fully_covered_opaque_mask_matches_a_plain_fill() {
        let mut budget = Budget::new(Limits::default());
        let pool = FramePool::default();
        let mut a = pool.acquire_video(PixFmt::Gray8, 4, 4).unwrap();
        let mut b = pool.acquire_video(PixFmt::Gray8, 4, 4).unwrap();
        vaco_filter_draw::fill::fill(
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
        let mask = full_mask(&mut budget, 4, 4);
        composite(
            &mut b,
            &mask,
            Rgba {
                r: 100,
                g: 0,
                b: 0,
                a: 255,
            },
            ColorInfo::default(),
        )
        .unwrap();
        assert_eq!(a.plane(0).unwrap().row(0), b.plane(0).unwrap().row(0));
    }

    #[test]
    fn zero_coverage_leaves_the_frame_untouched() {
        let mut budget = Budget::new(Limits::default());
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 4, 4).unwrap();
        vaco_filter_draw::fill::fill(
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
        let mask = AlphaMask::blank(&mut budget, 0, 0, 4, 4).unwrap();
        composite(
            &mut f,
            &mask,
            Rgba {
                r: 250,
                g: 0,
                b: 0,
                a: 255,
            },
            ColorInfo::default(),
        )
        .unwrap();
        assert_eq!(f.plane(0).unwrap().row(0).unwrap()[0], before);
    }

    #[test]
    fn out_of_frame_mask_clips_cleanly() {
        let mut budget = Budget::new(Limits::default());
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 4, 4).unwrap();
        let mask = full_mask(&mut budget, 4, 4);
        // Entirely off frame: no panic, no effect.
        let mut off = mask.clone();
        off.x = 100;
        off.y = 100;
        composite(&mut f, &off, Rgba::BLACK, ColorInfo::default()).unwrap();
    }

    #[test]
    fn blur_spreads_coverage_into_neighbouring_pixels() {
        let mut budget = Budget::new(Limits::default());
        let mut m = AlphaMask::blank(&mut budget, 0, 0, 5, 5).unwrap();
        if let Some(c) = m.coverage.get_mut(2 * 5 + 2) {
            *c = 255;
        }
        let blurred = m.box_blur(&mut budget, 1, 3).unwrap();
        assert!(
            blurred.coverage_at(1, 2) > 0,
            "blur should spread coverage to a neighbour"
        );
        assert!(
            blurred.coverage_at(2, 2) < 255,
            "blur should reduce the peak"
        );
    }

    #[test]
    fn blit_max_never_darkens_existing_coverage() {
        let mut budget = Budget::new(Limits::default());
        let mut dst = AlphaMask::blank(&mut budget, 0, 0, 4, 4).unwrap();
        if let Some(c) = dst.coverage.get_mut(0) {
            *c = 200;
        }
        let src = full_mask(&mut budget, 1, 1);
        dst.blit_max(&src, 0, 0);
        assert_eq!(dst.coverage_at(0, 0), 255);
    }
}
