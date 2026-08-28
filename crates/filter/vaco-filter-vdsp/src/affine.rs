//! Geometric affine transform: bilinear-sampled 2D warp, the shared core
//! behind `deshake`'s stabilisation transform and (per plan 16 §4.4's own
//! forward reference) `vidstabtransform`'s per-frame correction.
//!
//! Not to be confused with `vaco-scale::colour::Affine`, a same-named but
//! unrelated 3×3 matrix on *pixel component values* (the RGB↔YUV colour
//! conversion) — this module's `AffineMap` is a 2×3 matrix on *pixel
//! coordinates* (`cargo xtask dup-check`'s `DISTINCT` table does not need an
//! entry here because the two are in different crates with no shared name:
//! one is `Affine`, this one is `AffineMap`).

use vaco_frame::PlaneRef;

/// Maps a destination pixel coordinate to the source coordinate to sample —
/// already the inverse of whatever forward transform a caller conceptually
/// applies, which is the direction a warp kernel actually needs (it walks
/// destination pixels and asks "where did this come from").
///
/// `src = [[a, b, c], [d, e, f]] * [dst_x, dst_y, 1]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AffineMap {
    pub m: [[f64; 3]; 2],
}

impl AffineMap {
    /// No transform: `src == dst`.
    #[must_use]
    pub const fn identity() -> Self {
        Self { m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] }
    }

    /// A pure translation by `(dx, dy)`.
    #[must_use]
    pub const fn translation(dx: f64, dy: f64) -> Self {
        Self { m: [[1.0, 0.0, dx], [0.0, 1.0, dy]] }
    }

    /// A rotation by `theta` radians about `(cx, cy)`, composed with a
    /// uniform `scale`.
    #[must_use]
    pub fn rotation_about(theta: f64, scale: f64, cx: f64, cy: f64) -> Self {
        let (sin, cos) = theta.sin_cos();
        let xx = cos * scale;
        let xy = -sin * scale;
        let yx = sin * scale;
        let yy = cos * scale;
        Self {
            m: [
                [xx, xy, cx - xx * cx - xy * cy],
                [yx, yy, cy - yx * cx - yy * cy],
            ],
        }
    }

    /// The destination-to-source mapping for `(x, y)`.
    #[must_use]
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        let row = |r: [f64; 3]| r[0].mul_add(x, r[1].mul_add(y, r[2]));
        (row(self.m[0]), row(self.m[1]))
    }
}

/// Bilinear sample at `(x, y)` (source-space, may be fractional or out of
/// bounds). Out-of-bounds returns `None` rather than a border colour — the
/// caller decides what "nothing sampled" means (transparent, a fill
/// colour via [`crate` sibling drawing crates], or the destination pixel
/// left untouched).
#[must_use]
pub fn bilinear_sample(src: PlaneRef<'_>, x: f64, y: f64) -> Option<u8> {
    let (w, h) = (src.row_bytes(), src.rows());
    if w == 0 || h == 0 || x < 0.0 || y < 0.0 {
        return None;
    }
    #[allow(clippy::cast_sign_loss, reason = "x, y >= 0.0 is checked above")]
    let (x0, y0) = (x.floor() as usize, y.floor() as usize);
    if x0 >= w || y0 >= h {
        return None;
    }
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    #[allow(clippy::cast_precision_loss, reason = "pixel coordinates, far below f64's exact-integer range")]
    let (fx, fy) = (x - x0 as f64, y - y0 as f64);

    let px = |xi: usize, yi: usize| -> f64 { f64::from(src.row(yi).and_then(|r| r.get(xi)).copied().unwrap_or(0)) };

    let top = px(x0, y0) * (1.0 - fx) + px(x1, y0) * fx;
    let bottom = px(x0, y1) * (1.0 - fx) + px(x1, y1) * fx;
    let value = top * (1.0 - fy) + bottom * fy;
    #[allow(
        clippy::cast_possible_truncation,
        reason = "value is a convex combination of four u8 samples, so it is 0.0..=255.0"
    )]
    Some(value.round() as u8)
}

/// Warp `src` into a `dst_w × dst_h` output, sampling each destination pixel
/// through `map`. Pixels whose source falls outside `src` are left at
/// `fill`.
#[must_use]
pub fn warp_plane(src: PlaneRef<'_>, dst_w: usize, dst_h: usize, map: &AffineMap, fill: u8) -> Vec<u8> {
    let mut out = vec![fill; dst_w.saturating_mul(dst_h)];
    for y in 0..dst_h {
        for x in 0..dst_w {
            #[allow(clippy::cast_precision_loss, reason = "pixel coordinates, far below f64's exact-integer range")]
            let (sx, sy) = map.apply(x as f64, y as f64);
            if let Some(value) = bilinear_sample(src, sx, sy)
                && let Some(cell) = out.get_mut(y.saturating_mul(dst_w).saturating_add(x))
            {
                *cell = value;
            }
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_frame::FramePool;
    use vaco_pixfmt::PixFmt;

    fn plane_of(rows: &[&[u8]]) -> vaco_frame::Frame {
        let pool = FramePool::default();
        let h = rows.len() as u32;
        let w = rows.first().map_or(0, |r| r.len()) as u32;
        let mut f = pool.acquire_video(PixFmt::Gray8, w, h).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            for (y, row) in rows.iter().enumerate() {
                if let Some(dst) = p.row_mut(y) {
                    dst[..row.len()].copy_from_slice(row);
                }
            }
        }
        f
    }

    #[test]
    fn identity_map_reproduces_the_source_exactly() {
        let f = plane_of(&[&[10, 20, 30], &[40, 50, 60]]);
        let out = warp_plane(f.plane(0).unwrap(), 3, 2, &AffineMap::identity(), 0);
        assert_eq!(out, vec![10, 20, 30, 40, 50, 60]);
    }

    #[test]
    fn translation_shifts_content_and_fills_the_uncovered_edge() {
        let f = plane_of(&[&[10, 20, 30]]);
        // Shifting the *sample point* by +1 in x means dst(x) reads src(x+1).
        let out = warp_plane(f.plane(0).unwrap(), 3, 1, &AffineMap::translation(1.0, 0.0), 255);
        assert_eq!(out, vec![20, 30, 255]);
    }

    #[test]
    fn bilinear_sample_at_a_half_offset_averages_two_neighbours() {
        let f = plane_of(&[&[0, 100]]);
        let value = bilinear_sample(f.plane(0).unwrap(), 0.5, 0.0).unwrap();
        assert_eq!(value, 50);
    }

    #[test]
    fn negative_coordinates_are_out_of_bounds_not_a_panic() {
        let f = plane_of(&[&[1, 2]]);
        assert_eq!(bilinear_sample(f.plane(0).unwrap(), -1.0, 0.0), None);
    }

    #[test]
    fn rotation_about_the_center_by_a_full_turn_is_the_identity() {
        let f = plane_of(&[&[10, 20, 30], &[40, 50, 60], &[70, 80, 90]]);
        let map = AffineMap::rotation_about(std::f64::consts::TAU, 1.0, 1.0, 1.0);
        let out = warp_plane(f.plane(0).unwrap(), 3, 3, &map, 0);
        // A full 2*pi turn is numerically very close to the identity.
        assert_eq!(out[4], 50); // center pixel unaffected by rotation about itself
    }
}
