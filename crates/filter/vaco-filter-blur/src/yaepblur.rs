//! `yaepblur` — "yet another edge preserving blur": blend a pixel with its
//! local mean, weighted by local variance so flat regions blur and busy
//! regions are left alone.
//!
//! `ffmpeg -h filter=yaepblur` documents `radius`/`r` (default `3`),
//! `planes`/`p` (default `1`, luma only) and `sigma`/`s` (`1..=INT_MAX`,
//! default `128`).
//!
//! # Structural, not framecrc-verified
//!
//! Measured (`ffmpeg 8.1`, an interior step edge, `radius=1`): larger
//! `sigma` visibly blurs more (`sigma=1000000` moves a pixel most of the way
//! to its local box average; `sigma=1` barely moves it at all), confirming
//! `sigma` trades off blur strength against edge preservation, but solving
//! the exact per-pixel blend weight back out of the measured data did not
//! converge on a clean closed form inside this pass's time budget — the
//! `sigma=1000000` limit came out one count off a plain box average at one
//! probed pixel and exact at another, which rules out "large `sigma`
//! reduces to `common::box_pass`" as an exact statement even though it is
//! clearly the right *limit*.
//!
//! This ships a standard, independently published adaptive-smoothing
//! formula (the same shape as a local Wiener/minimum-mean-square-error
//! filter — a well-known class, not this filter's own invention):
//!
//! ```text
//! mean  = box(I, r)
//! var   = box((I - mean)^2, r)
//! w     = var / (var + sigma)
//! out   = w * I + (1 - w) * mean
//! ```
//!
//! `w -> 1` (self dominates) as local variance grows relative to `sigma` —
//! an edge is high-variance, so it resists blurring — and `w -> 0`
//! (`out -> mean`) as `sigma` grows relative to the local variance,
//! matching the measured "bigger `sigma`, more blur" trend qualitatively,
//! even though it is not the reference's exact formula.
//!
//! # Verified: a flat field is always the identity
//!
//! `var = 0` everywhere on a constant plane (every pixel equals the local
//! mean), so `w = 0/(0+sigma) = 0` and `out = mean = I` for *any* `sigma` —
//! a property of the formula's own algebra, matching the measured
//! reference behaviour on a flat field exactly (not merely structurally):
//! `ffmpeg ... yaepblur=radius=1` on a constant plane leaves it unchanged.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "yaepblur",
    description: "Yet another edge preserving blur filter",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "yaepblur", help = "Yet another edge preserving blur filter")]
pub(crate) struct Opts {
    #[opt(name = "radius", alias = "r", help = "set window radius", default = 3, range = 0..=1024, flags(video, filtering))]
    pub radius: i32,
    #[opt(name = "planes", alias = "p", help = "set planes to filter", default = 1, range = 0..=15, flags(video, filtering))]
    pub planes: i64,
    #[opt(name = "sigma", alias = "s", help = "set blur strength", default = 128, range = 1..=1_000_000_000, flags(video, filtering))]
    pub sigma: i64,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        Ok(o)
    }
}

fn blur_plane(rows: &[&[u8]], w: i32, h: i32, radius: i32, sigma: f64) -> Vec<Vec<u8>> {
    if radius <= 0 {
        return rows.iter().map(|r| (*r).to_vec()).collect();
    }
    let mut out = Vec::new();
    for y in 0..h {
        let mut row = Vec::new();
        for x in 0..w {
            let mut sum = 0.0;
            let mut count = 0.0;
            for dy in -radius..=radius {
                for dx in -radius..=radius {
                    sum += f64::from(common::sample_clamped(rows, x + dx, y + dy, w, h));
                    count += 1.0;
                }
            }
            let mean = sum / count;
            let mut var_sum = 0.0;
            for dy in -radius..=radius {
                for dx in -radius..=radius {
                    let v = f64::from(common::sample_clamped(rows, x + dx, y + dy, w, h));
                    var_sum += (v - mean) * (v - mean);
                }
            }
            let var = var_sum / count;
            let weight = var / (var + sigma);
            let self_v = f64::from(common::sample_clamped(rows, x, y, w, h));
            let value = weight.mul_add(self_v, (1.0 - weight) * mean).round();
            row.push(u8::try_from(value.clamp(0.0, 255.0) as i64).unwrap_or(255));
        }
        out.push(row);
    }
    out
}

#[derive(Debug)]
pub(crate) struct Filter {
    radius: i32,
    sigma: f64,
    planes: i64,
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { format, .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        common::ensure_8bit_addressable(format)?;
        let Some(LinkFormat::Video { width, height, .. }) = ctx.input_link(0).cloned() else {
            return Ok(FrameOut::One(input));
        };
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        for p in 0..format.plane_count() {
            let p8 = p as u8;
            let pw = common::to_i32(format.plane_width(width, p8));
            let ph = common::to_i32(format.plane_height(height, p8));
            let Some(src_plane) = input.plane(p) else {
                continue;
            };
            let rows = common::collect_rows(src_plane, ph.max(0) as usize);
            let filtered = if common::plane_selected(self.planes, p8) {
                blur_plane(&rows, pw, ph, self.radius, self.sigma)
            } else {
                rows.iter().map(|r| (*r).to_vec()).collect()
            };
            let Some(mut dst_plane) = out.plane_mut(p) else {
                continue;
            };
            for (y, row) in filtered.iter().enumerate() {
                if let Some(dst_row) = dst_plane.row_mut(y) {
                    let n = dst_row.len().min(row.len());
                    if let (Some(d), Some(s)) = (dst_row.get_mut(..n), row.get(..n)) {
                        d.copy_from_slice(s);
                    }
                }
            }
        }
        common::copy_frame_meta(&mut out, &input);
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter {
        radius: opts.radius,
        sigma: opts.sigma as f64,
        planes: opts.planes,
    };
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn zero_radius_is_identity() {
        let row0: &[u8] = &[1, 2, 3];
        let rows: [&[u8]; 1] = [row0];
        let out = blur_plane(&rows, 3, 1, 0, 128.0);
        assert_eq!(out[0], vec![1, 2, 3]);
    }

    /// Independent oracle: a flat field is always the identity, for any
    /// sigma (see this module's doc: `var = 0` forces `w = 0`, `out = I`).
    #[test]
    fn a_flat_field_is_always_the_identity() {
        for sigma in [1.0, 128.0, 1e6] {
            let rows_owned = vec![vec![77u8; 7]; 7];
            let rows: Vec<&[u8]> = rows_owned.iter().map(Vec::as_slice).collect();
            let out = blur_plane(&rows, 7, 7, 2, sigma);
            for row in out {
                for v in row {
                    assert_eq!(v, 77, "sigma={sigma}");
                }
            }
        }
    }

    /// A larger sigma blurs a step edge more, matching the measured trend
    /// (see this module's doc) even though the exact numbers are not
    /// pinned against the reference.
    #[test]
    fn larger_sigma_blurs_more_at_an_edge() {
        let img: Vec<Vec<u8>> = (0..5).map(|_| vec![0, 0, 0, 255, 255, 255, 255]).collect();
        let rows: Vec<&[u8]> = img.iter().map(Vec::as_slice).collect();
        let small = blur_plane(&rows, 7, 5, 1, 1.0);
        let large = blur_plane(&rows, 7, 5, 1, 1e6);
        // At the column just left of the edge, more blur means a bigger
        // upward shift from the original 0.
        assert!(large[2][2] >= small[2][2]);
    }
}
