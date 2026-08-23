//! Shared plane access for the denoise filters: pull one plane out to an
//! `f32` buffer, run the filter's own math on it, write the (clamped,
//! rounded) result back.
//!
//! # Scope
//!
//! Every filter in this crate needs real per-pixel arithmetic — averaging,
//! thresholding, transforms — which is awkward to do directly against the
//! packed byte layout [`vaco_frame::Plane`] stores. So [`PlaneBuf`] is the
//! one conversion point: [`PlaneBuf::read`] decodes a plane once per frame,
//! every filter computes over plain `f32`s, and [`PlaneBuf::write`] encodes
//! it back with a single rounding-and-clamping rule shared by all eight
//! filters.
//!
//! [`sample_layout`] restricts *which* pixel formats this crate can process:
//! exactly one component per plane (so a plane and a component coincide),
//! byte-aligned, host- little-endian, none of `BITSTREAM`/`HW_ACCEL`/
//! `PALETTE`/`FLOAT`/`BAYER`. That covers the whole `grayN`/`yuv4:4:4`/
//! `yuv4:2:2`/`yuv4:2:0` planar family at every depth up to 16 bits — the
//! formats every denoise filter is actually used on — and excludes
//! semi-planar (`nv12`) and packed (`rgb24`) layouts, which would need a
//! second component per plane and are a documented gap (see
//! `docs/filter/vaco-filter-denoise.md`). A filter asked to process a format
//! outside this set returns [`vaco_core::Error::Unsupported`] rather than
//! silently miscomputing.

use vaco_core::{Error, MediaType};
use vaco_filter_core::Pad;
use vaco_frame::{Frame, PlaneMut, PlaneRef};
use vaco_pixfmt::{PixFmt, PixFmtFlags};

/// Every filter in this crate is one video pad in, one video pad out.
pub(crate) const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

/// Copy everything but the pixel data from `src` onto `dst` — the metadata
/// every filter here leaves untouched (timestamps, colour signalling,
/// aspect ratio, frame flags).
pub(crate) fn copy_meta(dst: &mut Frame, src: &Frame) {
    dst.pts = src.pts;
    dst.time_base = src.time_base;
    dst.duration = src.duration;
    dst.color = src.color;
    dst.flags = src.flags;
    dst.sample_aspect_ratio = src.sample_aspect_ratio;
}

/// Bytes per sample and the maximum representable value for `plane` of
/// `format`, or `None` if this crate cannot address it (see the module doc).
pub(crate) fn sample_layout(format: PixFmt, plane: u8) -> Option<(usize, f32)> {
    if format.is_hw() || format.is_big_endian() {
        return None;
    }
    if format.plane_count() != format.component_count() {
        // Semi-planar (nv12) or a plane carrying more than one component:
        // out of scope, see the module doc.
        return None;
    }
    let d = format.descriptor();
    if d.flags.intersects(
        PixFmtFlags::BITSTREAM | PixFmtFlags::PALETTE | PixFmtFlags::FLOAT | PixFmtFlags::BAYER,
    ) {
        return None;
    }
    let comp = d.components.get(usize::from(plane))?;
    if comp.plane != plane || comp.offset != 0 || comp.depth == 0 || comp.depth > 16 {
        return None;
    }
    let bytes = if comp.depth <= 8 { 1usize } else { 2usize };
    if usize::from(comp.step) != bytes {
        return None;
    }
    let max = (1u32 << u32::from(comp.depth)).saturating_sub(1);
    Some((bytes, max as f32))
}

/// One plane, decoded to `f32` samples in `[0, max_val]`, row-major.
#[derive(Debug, Clone)]
pub(crate) struct PlaneBuf {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) max_val: f32,
    data: Vec<f32>,
}

impl PlaneBuf {
    /// Decode `plane` into an `f32` buffer of exactly `width * height`
    /// samples, given the byte width and maximum value a prior
    /// [`sample_layout`] call reported.
    pub(crate) fn read(
        plane: PlaneRef<'_>,
        width: usize,
        height: usize,
        bytes: usize,
        max_val: f32,
    ) -> Self {
        let mut data = vec![0.0f32; width.saturating_mul(height)];
        for y in 0..height {
            let Some(row) = plane.row(y) else { continue };
            for x in 0..width {
                let start = x.saturating_mul(bytes);
                let sample = match bytes {
                    2 => row
                        .get(start..start.saturating_add(2))
                        .and_then(|b| <[u8; 2]>::try_from(b).ok())
                        .map_or(0, u16::from_le_bytes),
                    _ => row.get(start).copied().map_or(0, u16::from),
                };
                let idx = y.saturating_mul(width).saturating_add(x);
                if let Some(dst) = data.get_mut(idx) {
                    *dst = f32::from(sample);
                }
            }
        }
        Self {
            width,
            height,
            max_val,
            data,
        }
    }

    /// Encode back into `plane`, rounding to nearest and clamping to
    /// `[0, self.max_val]`.
    pub(crate) fn write(&self, plane: &mut PlaneMut<'_>, bytes: usize) {
        for y in 0..self.height {
            let Some(row) = plane.row_mut(y) else { continue };
            for x in 0..self.width {
                let idx = y.saturating_mul(self.width).saturating_add(x);
                let v = self.data.get(idx).copied().unwrap_or(0.0);
                let v = v.clamp(0.0, self.max_val).round();
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "v is clamped to [0, max_val] and max_val <= 65535"
                )]
                let sample = v as u16;
                let start = x.saturating_mul(bytes);
                match bytes {
                    2 => {
                        if let Some(dst) = row.get_mut(start..start.saturating_add(2)) {
                            dst.copy_from_slice(&sample.to_le_bytes());
                        }
                    }
                    _ => {
                        if let Some(dst) = row.get_mut(start) {
                            #[allow(
                                clippy::cast_possible_truncation,
                                reason = "8-bit path: sample <= 255"
                            )]
                            {
                                *dst = sample as u8;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Sample at `(x, y)`, clamped to the plane edges ("replicate" boundary
    /// handling — the conventional choice for spatial kernels).
    #[must_use]
    pub(crate) fn get_clamped(&self, x: i64, y: i64) -> f32 {
        let max_x = i64::try_from(self.width.saturating_sub(1)).unwrap_or(i64::MAX);
        let max_y = i64::try_from(self.height.saturating_sub(1)).unwrap_or(i64::MAX);
        let cx = usize::try_from(x.clamp(0, max_x)).unwrap_or(0);
        let cy = usize::try_from(y.clamp(0, max_y)).unwrap_or(0);
        let idx = cy.saturating_mul(self.width).saturating_add(cx);
        self.data.get(idx).copied().unwrap_or(0.0)
    }

    /// In-bounds sample at `(x, y)`, or `None`.
    #[must_use]
    pub(crate) fn get(&self, x: usize, y: usize) -> Option<f32> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let idx = y.saturating_mul(self.width).saturating_add(x);
        self.data.get(idx).copied()
    }

    pub(crate) fn set(&mut self, x: usize, y: usize, v: f32) {
        if x >= self.width || y >= self.height {
            return;
        }
        let idx = y.saturating_mul(self.width).saturating_add(x);
        if let Some(dst) = self.data.get_mut(idx) {
            *dst = v;
        }
    }

    /// All samples, row-major.
    #[must_use]
    pub(crate) fn as_slice(&self) -> &[f32] {
        &self.data
    }

    /// A `width * height` buffer of zeroes: the identity element several
    /// filters build a result into before setting every sample, and what
    /// tests use to build synthetic input directly rather than decoding a
    /// real [`vaco_frame::Plane`].
    pub(crate) fn zeroed(width: usize, height: usize, max_val: f32) -> Self {
        Self {
            width,
            height,
            max_val,
            data: vec![0.0; width.saturating_mul(height)],
        }
    }

    /// Mean of every sample. Used by more than one filter's tests as an
    /// independent invariant (block-mean preservation).
    #[must_use]
    #[cfg(test)]
    pub(crate) fn mean(&self) -> f32 {
        if self.data.is_empty() {
            return 0.0;
        }
        #[allow(
            clippy::cast_precision_loss,
            reason = "plane sample counts are far below f32's exact-integer range"
        )]
        let n = self.data.len() as f32;
        self.data.iter().sum::<f32>() / n
    }

    /// Population variance of every sample. Used as the noise-power oracle:
    /// a denoiser must not *increase* the variance of a noisy-but-flat input.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn variance(&self) -> f32 {
        if self.data.is_empty() {
            return 0.0;
        }
        #[allow(
            clippy::cast_precision_loss,
            reason = "plane sample counts are far below f32's exact-integer range"
        )]
        let n = self.data.len() as f32;
        let mean = self.data.iter().sum::<f32>() / n;
        self.data.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / n
    }
}

/// Parse a `planes` bitmask option: which plane indices (0..4) a filter
/// should touch, others passed through unmodified. Several filters in this
/// crate (`atadenoise`'s `p`, `fftdnoiz`'s and `vaguedenoiser`'s `planes`)
/// share exactly this convention.
pub(crate) fn planes_mask_opt(
    req: &vaco_filter_graph::registry::Instantiate<'_>,
    keys: &[&str],
    default: u8,
) -> u8 {
    for k in keys {
        if let Some(v) = req.named(k)
            && let Ok(n) = v.trim().parse::<u32>()
        {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a plane bitmask only ever needs the low few bits"
            )]
            return n as u8;
        }
    }
    default
}

/// Whether `planes_mask` selects plane `index`.
pub(crate) fn plane_selected(planes_mask: u8, index: usize) -> bool {
    u32::try_from(index)
        .ok()
        .and_then(|i| 1u8.checked_shl(i))
        .is_none_or(|bit| planes_mask & bit != 0)
}

/// Plane geometry helper: width/height of `plane` for a frame of `(width,
/// height)`, following `format`'s chroma subsampling.
pub(crate) fn plane_dims(format: PixFmt, width: u32, height: u32, plane: u8) -> (usize, usize) {
    (
        format.plane_width(width, plane) as usize,
        format.plane_height(height, plane) as usize,
    )
}

/// Common error for a format this crate's plane access cannot handle.
///
/// `format` is not embedded in the message: [`Error::Unsupported`] carries a
/// `&'static str`, and the point of [`sample_layout`] returning `None` rather
/// than an error itself is exactly that callers can name their own static
/// message rather than formatting one per call site.
pub(crate) const fn unsupported_format() -> Error {
    Error::Unsupported(
        "denoise filter: unsupported pixel format layout (see video::sample_layout)",
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn gray8_and_gray16_are_addressable() {
        assert_eq!(sample_layout(PixFmt::Gray8, 0), Some((1, 255.0)));
        assert!(sample_layout(PixFmt::Gray16le, 0).is_some());
    }

    #[test]
    fn nv12_chroma_plane_is_not_addressable() {
        // NV12's plane 1 interleaves U and V: two components share it, so
        // `plane_count() != component_count()`.
        if let Ok(nv12) = PixFmt::from_name("nv12") {
            assert_eq!(sample_layout(nv12, 1), None);
        }
    }

    #[test]
    fn read_write_round_trips_exactly() {
        use vaco_frame::Frame;
        use vaco_limits::{Budget, Limits};

        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Gray8, 4, 3).unwrap();
        {
            let mut plane = frame.plane_mut(0).unwrap();
            for y in 0..3 {
                let row = plane.row_mut(y).unwrap();
                for (x, b) in row.iter_mut().enumerate() {
                    *b = ((y * 4 + x) * 17) as u8;
                }
            }
        }
        let buf = PlaneBuf::read(frame.plane(0).unwrap(), 4, 3, 1, 255.0);
        let mut out = Frame::alloc_video(&mut budget, PixFmt::Gray8, 4, 3).unwrap();
        {
            let mut plane = out.plane_mut(0).unwrap();
            buf.write(&mut plane, 1);
        }
        assert_eq!(
            frame.plane(0).unwrap().row(0),
            out.plane(0).unwrap().row(0)
        );
        assert_eq!(
            frame.plane(0).unwrap().row(2),
            out.plane(0).unwrap().row(2)
        );
    }
}
