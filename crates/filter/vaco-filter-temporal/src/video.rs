//! Shared plane access and option parsing for this crate's filters.
//!
//! [`PlaneBuf`]/[`sample_layout`] mirror `vaco-filter-denoise::video`'s
//! `PlaneBuf`/`sample_layout` exactly (decode one plane to `f32`, run plain
//! per-pixel arithmetic, encode back with one clamp-and-round rule).
//! Duplicated rather than shared: neither crate depends on the other, no
//! lower crate exports this today, and `vaco-filter-denoise` marks the
//! module `pub(crate)` in any case. If a third crate needs the same helper,
//! that is the point to hoist it — most plausibly into `vaco-filter-vdsp`,
//! which this crate already introduces — but doing that unilaterally here
//! would touch a crate this brief does not own.

use vaco_core::MediaType;
use vaco_filter_core::Pad;
use vaco_filter_graph::registry::Instantiate;
use vaco_frame::{Frame, PlaneMut, PlaneRef};
use vaco_pixfmt::{PixFmt, PixFmtFlags};

/// Every filter in this crate that is not `freezeframes` is one video pad
/// in, one video pad out.
pub(crate) const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

/// Copy everything but the pixel data: timestamps, colour signalling, aspect
/// ratio, frame flags. Every filter here that produces a frame derived from
/// one input frame starts from this.
pub(crate) fn copy_meta(dst: &mut Frame, src: &Frame) {
    dst.pts = src.pts;
    dst.time_base = src.time_base;
    dst.duration = src.duration;
    dst.color = src.color;
    dst.flags = src.flags;
    dst.sample_aspect_ratio = src.sample_aspect_ratio;
}

/// Bytes per sample and the maximum representable value for `plane` of
/// `format`, or `None` if this crate cannot address it: anything but exactly
/// one byte-aligned, host-little-endian, non-bitstream/palette/float/bayer
/// component per plane, at a depth up to 16 bits. That is the whole
/// `grayN`/planar-`yuv` family and excludes semi-planar (`nv12`) and packed
/// (`rgb24`) layouts — the same restriction `vaco-filter-denoise` documents,
/// for the same reason.
pub(crate) fn sample_layout(format: PixFmt, plane: u8) -> Option<(usize, f32)> {
    if format.is_hw() || format.is_big_endian() {
        return None;
    }
    if format.plane_count() != format.component_count() {
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
    #[allow(
        clippy::cast_precision_loss,
        reason = "max is at most 65535, exactly representable in f32"
    )]
    let max_f = max as f32;
    Some((bytes, max_f))
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

    pub(crate) fn get(&self, x: usize, y: usize) -> f32 {
        self.data
            .get(y.saturating_mul(self.width).saturating_add(x))
            .copied()
            .unwrap_or(0.0)
    }

    pub(crate) fn set(&mut self, x: usize, y: usize, v: f32) {
        let idx = y.saturating_mul(self.width).saturating_add(x);
        if let Some(dst) = self.data.get_mut(idx) {
            *dst = v;
        }
    }

    pub(crate) fn as_slice(&self) -> &[f32] {
        &self.data
    }

    pub(crate) fn write(&self, plane: &mut PlaneMut<'_>, bytes: usize) {
        for y in 0..self.height {
            let Some(row) = plane.row_mut(y) else {
                continue;
            };
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
}

// --- option parsing helpers, shared across this crate's `create` fns -----

pub(crate) fn str_opt(req: &Instantiate<'_>, key: &str) -> Option<String> {
    req.named(key)
}

pub(crate) fn f64_opt(req: &Instantiate<'_>, key: &str, default: f64) -> f64 {
    req.named(key)
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or(default)
}

pub(crate) fn i64_opt(req: &Instantiate<'_>, key: &str, default: i64) -> i64 {
    req.named(key)
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(default)
}

pub(crate) fn usize_opt(req: &Instantiate<'_>, key: &str, default: usize) -> usize {
    req.named(key)
        .and_then(|v| v.trim().parse::<i64>().ok())
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(default)
}

pub(crate) fn bool_opt(req: &Instantiate<'_>, key: &str, default: bool) -> bool {
    match req.named(key).as_deref() {
        Some("1" | "true" | "yes") => true,
        Some("0" | "false" | "no") => false,
        _ => default,
    }
}

/// Plane dimensions for a given format/plane index, via chroma subsampling
/// (`log2_chroma_w`/`log2_chroma_h`; plane 0 is always full resolution).
pub(crate) fn plane_dims(format: PixFmt, width: u32, height: u32, plane: usize) -> (usize, usize) {
    let d = format.descriptor();
    let (sw, sh) = if plane == 0 {
        (0u8, 0u8)
    } else {
        (d.log2_chroma_w, d.log2_chroma_h)
    };
    let pw = (width as usize).div_ceil(1usize << sw);
    let ph = (height as usize).div_ceil(1usize << sh);
    (pw, ph)
}

/// A `planes` bitmask option, accepted either as a bare integer (the
/// reference's own encoding) or left at `default` when absent/unparseable.
pub(crate) fn planes_mask_opt(req: &Instantiate<'_>, keys: &[&str], default: u8) -> u8 {
    for key in keys {
        if let Some(v) = req.named(key)
            && let Ok(n) = v.trim().parse::<i64>()
        {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "planes bitmasks are documented 0..=15"
            )]
            return (n.clamp(0, 0xFF)) as u8;
        }
    }
    default
}
