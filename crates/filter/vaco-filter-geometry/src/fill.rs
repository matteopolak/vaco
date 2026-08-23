//! Solid-colour raster generation for `perspective` and `fillborders`'
//! `fixed`/`color` fallback (and any future per-pixel warper this crate
//! grows).
//!
//! Same approach as `vaco-filter-video-geometry::fill` (see that crate's doc
//! for the measurement backing it): build a small RGB24 tile and run it
//! through [`vaco_scale::Scaler`] into the destination format, rather than
//! hand-deriving a colour matrix. That is what makes a `black` fill land on
//! limited-range `Y=16` for `yuv420p` rather than `0`.

use vaco_core::{Error, Result};
use vaco_frame::{Frame, FramePool};
use vaco_pixfmt::PixFmt;
use vaco_scale::{ImageSpec, ScaleOptions, Scaler};

fn aligned_tile_size(format: PixFmt) -> (u32, u32) {
    let (sw, sh) = format.log2_chroma();
    (1u32 << sw, 1u32 << sh)
}

/// Render a `width`x`height` frame of `format` filled with `rgb`.
///
/// # Errors
/// [`vaco_core::Error::Unsupported`] for a `format` `vaco-scale` cannot
/// target, or whatever allocating the frame reports.
pub(crate) fn solid_frame(
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

/// The single-pixel byte pattern for `rgb` in `format`'s plane 0, used by
/// per-pixel warpers ([`crate::perspective`]) to fill an
/// out-of-bounds destination sample without allocating a whole frame per
/// pixel. Built once per `configure` from a 1x1-tile-aligned [`solid_frame`]
/// and then copied byte-for-byte per plane.
pub(crate) struct FillPattern {
    /// One aligned tile per plane, at that plane's own subsampled size.
    pub(crate) planes: smallvec::SmallVec<[Vec<u8>; 4]>,
}

impl FillPattern {
    pub(crate) fn build(
        pool: &FramePool,
        format: PixFmt,
        rgb: (u8, u8, u8),
        color: vaco_color::ColorInfo,
    ) -> Result<Self> {
        let (tw, th) = aligned_tile_size(format);
        let frame = solid_frame(pool, format, tw, th, rgb, color)?;
        let mut planes = smallvec::SmallVec::new();
        for p in 0..format.plane_count() {
            let unit = crate::geom::plane_unit_bytes(format, p as u8)?;
            let pw = format.plane_width(tw, p as u8) as usize;
            let mut buf = vec![0_u8; unit];
            if let Some(plane) = frame.plane(p)
                && let Some(row) = plane.row(0)
                && let Some(px) = row.get(..unit.min(pw.saturating_mul(unit)))
            {
                let n = buf.len().min(px.len());
                if let (Some(d), Some(s)) = (buf.get_mut(..n), px.get(..n)) {
                    d.copy_from_slice(s);
                }
            }
            planes.push(buf);
        }
        Ok(Self { planes })
    }

    /// The fill byte-group for `plane`, one `unit`-sized group, tileable
    /// across the whole plane (a solid colour repeats identically).
    pub(crate) fn plane_pixel(&self, plane: usize) -> &[u8] {
        self.planes.get(plane).map_or(&[], Vec::as_slice)
    }
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
        assert_eq!(y.row(0).unwrap()[0], 16);
    }

    #[test]
    fn fill_pattern_plane0_matches_solid_frame() {
        let pool = FramePool::default();
        let pat = FillPattern::build(
            &pool,
            PixFmt::Gray8,
            (16, 16, 16),
            vaco_color::ColorInfo::default(),
        )
        .unwrap();
        assert_eq!(pat.plane_pixel(0), &[16]);
    }
}
