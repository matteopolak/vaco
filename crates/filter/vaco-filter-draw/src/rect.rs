//! A frame-space rectangle, and its per-plane, chroma-decimated projection.
//!
//! `boxblur`'s and `drawbox`'s own kind of geometry: an `(x, y, w, h)` given
//! in luma/RGB pixel coordinates has to become a *different* rectangle on a
//! subsampled chroma plane — half the size on each decimated axis, rounded so
//! adjacent boxes tile without a gap or a double-counted column.

use vaco_pixfmt::PixFmt;

/// An axis-aligned rectangle in frame-space (luma/RGB) pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    /// The whole frame.
    #[must_use]
    pub const fn full(width: u32, height: u32) -> Self {
        Self { x: 0, y: 0, w: width, h: height }
    }

    /// Clip to `0..frame_w` × `0..frame_h`, shrinking `w`/`h` rather than
    /// moving `x`/`y`. An input entirely outside the frame clips to a
    /// zero-sized rectangle at the frame's own corner, not an error — a
    /// filter option is free to describe a box off-frame and drawing nothing
    /// is the right degradation.
    #[must_use]
    pub fn clip(self, frame_w: u32, frame_h: u32) -> Self {
        let x = self.x.min(frame_w);
        let y = self.y.min(frame_h);
        let w = self.w.min(frame_w.saturating_sub(x));
        let h = self.h.min(frame_h.saturating_sub(y));
        Self { x, y, w, h }
    }

    /// This rectangle's projection onto plane `plane` of `fmt`, at frame
    /// size `frame_w × frame_h`.
    ///
    /// Chroma planes decimate by `2^log2_chroma_{w,h}`; both the origin and
    /// the far edge are shifted independently (`x >> shift`,
    /// `(x + w) >> shift`, then subtracted) rather than `w >> shift`, so a
    /// rectangle whose edges do not fall on a chroma sample boundary still
    /// tiles exactly with a neighbouring rectangle that shares that edge —
    /// `w >> shift` alone would round both independently and could leave a
    /// one-sample gap or overlap between them.
    #[must_use]
    pub fn on_plane(self, fmt: PixFmt, plane: u8, frame_w: u32, frame_h: u32) -> Self {
        let (log2_w, log2_h) = fmt.log2_chroma();
        let is_chroma = fmt
            .descriptor()
            .components
            .iter()
            .any(|c| c.plane == plane)
            && plane_is_chroma(fmt, plane);
        let (sw, sh) = if is_chroma { (log2_w, log2_h) } else { (0, 0) };
        let x0 = self.x >> sw;
        let y0 = self.y >> sh;
        let x1 = self.x.saturating_add(self.w) >> sw;
        let y1 = self.y.saturating_add(self.h) >> sh;
        let plane_w = fmt.plane_width(frame_w, plane);
        let plane_h = fmt.plane_height(frame_h, plane);
        Self {
            x: x0,
            y: y0,
            w: x1.saturating_sub(x0),
            h: y1.saturating_sub(y0),
        }
        .clip(plane_w, plane_h)
    }

    /// The border-only ring of thickness `t` inside this rectangle (for
    /// `drawbox`'s `thickness` option), as up to four sub-rectangles: top,
    /// bottom, left, right. `t >= min(w, h) / 2` degrades to a single filled
    /// rectangle (the whole box), matching the reference's own
    /// `thickness=fill`/oversized-thickness behaviour of drawing a solid
    /// box rather than an empty or negative-area ring.
    #[must_use]
    pub fn border_ring(self, t: u32) -> Vec<Self> {
        if t == 0 || self.w == 0 || self.h == 0 {
            return Vec::new();
        }
        if t.saturating_mul(2) >= self.w || t.saturating_mul(2) >= self.h {
            return vec![self];
        }
        let top = Self { x: self.x, y: self.y, w: self.w, h: t };
        let bottom = Self {
            x: self.x,
            y: self.y + self.h - t,
            w: self.w,
            h: t,
        };
        let left = Self {
            x: self.x,
            y: self.y + t,
            w: t,
            h: self.h - 2 * t,
        };
        let right = Self {
            x: self.x + self.w - t,
            y: self.y + t,
            w: t,
            h: self.h - 2 * t,
        };
        vec![top, bottom, left, right]
    }
}

fn plane_is_chroma(fmt: PixFmt, plane: u8) -> bool {
    // A plane is chroma exactly when its first component is logical channel
    // 1 or 2 (U/Cb or V/Cr) of a non-RGB format — RGB has no chroma planes
    // even though `gbrp`'s green plane is physically plane 0's near-neighbour
    // in some formats, so the RGB check comes first.
    if fmt.descriptor().flags.contains(vaco_pixfmt::PixFmtFlags::RGB) {
        return false;
    }
    fmt.descriptor()
        .components
        .iter()
        .enumerate()
        .any(|(logical, c)| c.plane == plane && (logical == 1 || logical == 2))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::expect_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn clip_shrinks_rather_than_moves() {
        let r = Rect { x: 5, y: 5, w: 100, h: 100 }.clip(10, 10);
        assert_eq!(r, Rect { x: 5, y: 5, w: 5, h: 5 });
    }

    #[test]
    fn fully_outside_clips_to_zero_area() {
        let r = Rect { x: 20, y: 20, w: 10, h: 10 }.clip(10, 10);
        assert_eq!(r.w, 0);
        assert_eq!(r.h, 0);
    }

    #[test]
    fn luma_plane_projection_is_the_identity() {
        let r = Rect { x: 3, y: 4, w: 10, h: 8 };
        let p = r.on_plane(PixFmt::Yuv420p, 0, 64, 64);
        assert_eq!(p, r);
    }

    #[test]
    fn chroma_plane_projection_halves_both_axes_for_420() {
        let r = Rect { x: 4, y: 4, w: 8, h: 8 };
        let p = r.on_plane(PixFmt::Yuv420p, 1, 64, 64);
        assert_eq!(p, Rect { x: 2, y: 2, w: 4, h: 4 });
    }

    #[test]
    fn adjacent_odd_edged_rectangles_tile_without_a_gap_on_chroma() {
        // Two rectangles sharing the edge x=5 (odd, not chroma-aligned).
        let left = Rect { x: 0, y: 0, w: 5, h: 8 };
        let right = Rect { x: 5, y: 0, w: 5, h: 8 };
        let lp = left.on_plane(PixFmt::Yuv420p, 1, 64, 64);
        let rp = right.on_plane(PixFmt::Yuv420p, 1, 64, 64);
        assert_eq!(lp.x + lp.w, rp.x, "no gap and no overlap at the shared edge");
    }

    #[test]
    fn rgb_formats_never_decimate() {
        let r = Rect { x: 1, y: 1, w: 5, h: 5 };
        let p = r.on_plane(PixFmt::Gbrp, 0, 64, 64);
        assert_eq!(p, r);
    }

    #[test]
    fn oversized_thickness_degrades_to_a_filled_box() {
        let r = Rect { x: 0, y: 0, w: 10, h: 10 };
        let ring = r.border_ring(10);
        assert_eq!(ring, vec![r]);
    }

    #[test]
    fn normal_thickness_yields_four_border_strips() {
        let r = Rect { x: 0, y: 0, w: 10, h: 10 };
        let ring = r.border_ring(2);
        assert_eq!(ring.len(), 4);
        // Total border area = outer area - inner area.
        let area: u32 = ring.iter().map(|s| s.w * s.h).sum();
        assert_eq!(area, 10 * 10 - 6 * 6);
    }
}
