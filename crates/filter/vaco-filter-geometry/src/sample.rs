//! A generic nearest/bilinear single-plane sampler, shared by
//! [`crate::perspective`] (the only per-pixel warper left in this crate —
//! see this crate's `lib.rs` doc for why `rotate` itself is not registered
//! here: `vaco-filter-video-composite` already owns it).
//!
//! Works on raw plane bytes rather than a typed sample, so it is safe to
//! call for any pixel format's plane: nearest-neighbour is a byte-group
//! copy (correct for any bit depth or packing), and bilinear additionally
//! interpolates when `unit == 1` (an 8-bit, one-byte-per-sample plane) and
//! otherwise falls back to nearest rather than blending raw bytes of a
//! wider or packed sample, which would corrupt it.

/// Sample `plane` at continuous coordinate `(src_x, src_y)` (pixel-centre
/// convention: pixel `i` occupies `[i, i+1)`, `src_x`/`src_y` already in
/// that space) into `out`, a `unit`-byte destination slot. Falls back to
/// `fill` (a `unit`-byte pattern) when the sampled position is out of
/// bounds.
#[allow(
    clippy::too_many_arguments,
    reason = "a plain sampling kernel; grouping these into a struct would not \
              make any call site clearer, only add an extra type to thread through"
)]
pub(crate) fn sample_plane_pixel(
    plane: &vaco_frame::PlaneRef<'_>,
    unit: usize,
    plane_w: u32,
    plane_h: u32,
    src_x: f64,
    src_y: f64,
    bilinear: bool,
    fill: &[u8],
    out: &mut [u8],
) {
    let in_bounds =
        |x: i64, y: i64| x >= 0 && y >= 0 && (x as u32) < plane_w && (y as u32) < plane_h;
    let read = |x: i64, y: i64| -> Option<&[u8]> {
        if !in_bounds(x, y) {
            return None;
        }
        let row = plane.row(y as usize)?;
        let start = (x as usize).saturating_mul(unit);
        row.get(start..start.saturating_add(unit))
    };
    let x0 = src_x.floor();
    let y0 = src_y.floor();
    if !bilinear || unit != 1 {
        let (sx, sy) = (x0 as i64, y0 as i64);
        if let Some(px) = read(sx, sy) {
            let n = out.len().min(px.len());
            if let (Some(d), Some(s)) = (out.get_mut(..n), px.get(..n)) {
                d.copy_from_slice(s);
            }
        } else {
            let n = out.len().min(fill.len());
            if let (Some(d), Some(s)) = (out.get_mut(..n), fill.get(..n)) {
                d.copy_from_slice(s);
            }
        }
        return;
    }
    // Bilinear blends between pixel *centres* (pixel `i`'s value sits at
    // continuous position `i + 0.5`, the same convention `src_x`/`src_y`
    // are computed in). Shifting by `-0.5` before flooring is what makes a
    // query exactly at a pixel's own centre come back as 100% that pixel —
    // without it, `src_x = i + 0.5` incorrectly blends pixels `i` and
    // `i + 1` 50/50, a half-pixel offset. Nearest-neighbour (above) has no
    // such correction because it only ever picks one sample, so there is no
    // "which two pixels" question for an offset to get wrong.
    let bx = src_x - 0.5;
    let by = src_y - 0.5;
    let bx0 = bx.floor();
    let by0 = by.floor();
    let sx = bx0 as i64;
    let sy = by0 as i64;
    if !in_bounds(sx, sy) {
        let n = out.len().min(fill.len());
        if let (Some(d), Some(s)) = (out.get_mut(..n), fill.get(..n)) {
            d.copy_from_slice(s);
        }
        return;
    }
    let fx = bx - bx0;
    let fy = by - by0;
    let clamp_read = |x: i64, y: i64| -> u8 {
        let cx = x.clamp(0, i64::from(plane_w) - 1);
        let cy = y.clamp(0, i64::from(plane_h) - 1);
        read(cx, cy).and_then(|s| s.first()).copied().unwrap_or(0)
    };
    let p00 = f64::from(clamp_read(sx, sy));
    let p10 = f64::from(clamp_read(sx + 1, sy));
    let p01 = f64::from(clamp_read(sx, sy + 1));
    let p11 = f64::from(clamp_read(sx + 1, sy + 1));
    let top = p00 + (p10 - p00) * fx;
    let bot = p01 + (p11 - p01) * fx;
    let v = top + (bot - top) * fy;
    if let Some(d) = out.first_mut() {
        *d = v.round().clamp(0.0, 255.0) as u8;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_frame::FramePool;
    use vaco_pixfmt::PixFmt;

    #[test]
    fn nearest_reads_the_floored_pixel() {
        let pool = FramePool::default();
        let mut frame = pool.acquire_video(PixFmt::Gray8, 4, 1).unwrap();
        if let Some(mut plane) = frame.plane_mut(0)
            && let Some(row) = plane.row_mut(0)
        {
            row.copy_from_slice(&[10, 20, 30, 40]);
        }
        let plane = frame.plane(0).unwrap();
        let mut out = [0_u8; 1];
        sample_plane_pixel(&plane, 1, 4, 1, 2.5, 0.5, false, &[0], &mut out);
        assert_eq!(out, [30]);
    }

    #[test]
    fn out_of_bounds_uses_fill() {
        let pool = FramePool::default();
        let frame = pool.acquire_video(PixFmt::Gray8, 4, 1).unwrap();
        let plane = frame.plane(0).unwrap();
        let mut out = [0_u8; 1];
        sample_plane_pixel(&plane, 1, 4, 1, -1.0, 0.5, false, &[99], &mut out);
        assert_eq!(out, [99]);
    }

    #[test]
    fn bilinear_averages_two_neighbours() {
        let pool = FramePool::default();
        let mut frame = pool.acquire_video(PixFmt::Gray8, 2, 1).unwrap();
        if let Some(mut plane) = frame.plane_mut(0)
            && let Some(row) = plane.row_mut(0)
        {
            row.copy_from_slice(&[0, 100]);
        }
        let plane = frame.plane(0).unwrap();
        let mut out = [0_u8; 1];
        // Midpoint between pixel 0 (centre 0.5) and pixel 1 (centre 1.5).
        sample_plane_pixel(&plane, 1, 2, 1, 1.0, 0.5, true, &[0], &mut out);
        assert_eq!(out, [50]);
    }

    #[test]
    fn bilinear_at_a_pixel_centre_is_that_pixel_exactly() {
        // The half-pixel-offset bug this crate's tests caught: querying
        // exactly at pixel 1's own centre (1.5) must return pixel 1
        // untouched, not a 50/50 blend with pixel 2.
        let pool = FramePool::default();
        let mut frame = pool.acquire_video(PixFmt::Gray8, 3, 1).unwrap();
        if let Some(mut plane) = frame.plane_mut(0)
            && let Some(row) = plane.row_mut(0)
        {
            row.copy_from_slice(&[0, 100, 200]);
        }
        let plane = frame.plane(0).unwrap();
        let mut out = [0_u8; 1];
        sample_plane_pixel(&plane, 1, 3, 1, 1.5, 0.5, true, &[0], &mut out);
        assert_eq!(out, [100]);
    }
}
