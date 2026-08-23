//! `nlmeans` — Non-local means denoising (Buades, Coll & Morel, 2005): every
//! pixel is replaced by a weighted average of *every* pixel in a search
//! window whose surrounding patch looks similar, not just its immediate
//! spatial neighbours.
//!
//! # Options (`ffmpeg -h filter=nlmeans`, probed 2026-08-23)
//!
//! `s` — denoising strength `h` (`f64`, `1..=30`, default `1`); `p` — patch
//! size (`int`, `0..=99`, default `7`); `pc` — chroma patch size (default
//! `0`, meaning "same as `p`"); `r` — research window size (default `15`);
//! `rc` — chroma research window (default `0`, meaning "same as `r`").
//!
//! # Algorithm
//!
//! For pixel `i` with patch radius `pr = p / 2` and search radius `rr = r /
//! 2`, over every `j` within `rr` of `i`:
//!
//! ```text
//! d2(i, j)  = mean over the patch of (I(i + o) - I(j + o))^2
//! w(i, j)   = exp(-d2(i, j) / h^2)
//! out(i)    = sum_j w(i, j) * I(j) / sum_j w(i, j)
//! ```
//!
//! `h` is `s`, taken directly in the plane's own intensity units (Buades et
//! al.'s "filtering parameter", provenance/sources.toml's
//! `buades-coll-morel-2005-nlmeans` entry) — the reference's `s` range
//! (`1..=30`) is small relative to an 8-bit plane's `0..=255`, matching this
//! reading of it as a strength directly in sample units rather than a
//! normalised fraction.
//!
//! # Independent oracles
//!
//! * **Flat-field invariant**: on a constant plane every patch distance is
//!   `0`, so every weight is equal; the weighted average of identical
//!   values is that value, exactly, regardless of the weight formula's
//!   details.
//! * **Noise-power bound**: on a plane that is constant *plus* independent
//!   per-pixel noise, every patch is — up to the noise — identical to every
//!   other, so NLM degenerates toward an average over the whole search
//!   window and the output's sample variance must fall well below the
//!   input's. This is the paper's own claimed behaviour in the flat-region
//!   case, checked as a variance inequality rather than by re-deriving the
//!   pixel values NLM itself would produce.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Timeline};
use vaco_frame::{Frame, FrameData};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{self, PlaneBuf, VIDEO_PAD};

pub const DESC: FilterDesc = FilterDesc {
    name: "nlmeans",
    description: "Non-local means denoiser.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

fn f64_opt(req: &Instantiate<'_>, key: &str, default: f64) -> f64 {
    req.named(key)
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or(default)
}

fn usize_opt(req: &Instantiate<'_>, key: &str, default: usize) -> usize {
    req.named(key)
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

#[derive(Debug, Clone, Copy)]
struct Options {
    h: f32,
    patch: usize,
    patch_chroma: usize,
    research: usize,
    research_chroma: usize,
}

impl Options {
    fn parse(req: &Instantiate<'_>) -> Self {
        let patch = usize_opt(req, "p", 7);
        let research = usize_opt(req, "r", 15);
        let pc = usize_opt(req, "pc", 0);
        let rc = usize_opt(req, "rc", 0);
        Self {
            h: f64_opt(req, "s", 1.0).max(0.001) as f32,
            patch,
            patch_chroma: if pc == 0 { patch } else { pc },
            research,
            research_chroma: if rc == 0 { research } else { rc },
        }
    }

    fn radii_for(&self, plane: usize) -> (i64, i64) {
        let (p, r) = if plane == 0 {
            (self.patch, self.research)
        } else {
            (self.patch_chroma, self.research_chroma)
        };
        #[allow(
            clippy::integer_division,
            reason = "patch/research radius from a diameter option; truncation toward zero is the intended rounding"
        )]
        let (pr, rr) = ((p / 2).max(1), (r / 2).max(1));
        (
            i64::try_from(pr).unwrap_or(i64::MAX),
            i64::try_from(rr).unwrap_or(i64::MAX),
        )
    }
}

/// Sum of squared differences between the `(2*pr+1)^2` patches centred at
/// `(x1, y1)` and `(x2, y2)`, normalised by patch pixel count.
fn patch_distance(buf: &PlaneBuf, x1: i64, y1: i64, x2: i64, y2: i64, pr: i64) -> f32 {
    let mut acc = 0.0f32;
    let mut n = 0.0f32;
    for dy in -pr..=pr {
        for dx in -pr..=pr {
            let a = buf.get_clamped(x1 + dx, y1 + dy);
            let b = buf.get_clamped(x2 + dx, y2 + dy);
            acc += (a - b) * (a - b);
            n += 1.0;
        }
    }
    if n > 0.0 { acc / n } else { 0.0 }
}

fn nlmeans_plane(buf: &PlaneBuf, h: f32, pr: i64, rr: i64) -> PlaneBuf {
    let mut out = PlaneBuf::zeroed(buf.width, buf.height, buf.max_val);
    let h2 = (h * h).max(1e-6);
    for y in 0..buf.height {
        for x in 0..buf.width {
            let xi = i64::try_from(x).unwrap_or(i64::MAX);
            let yi = i64::try_from(y).unwrap_or(i64::MAX);
            let mut num = 0.0f32;
            let mut den = 0.0f32;
            for dy in -rr..=rr {
                for dx in -rr..=rr {
                    let (jx, jy) = (xi + dx, yi + dy);
                    let d2 = patch_distance(buf, xi, yi, jx, jy, pr);
                    let w = (-d2 / h2).exp();
                    num += w * buf.get_clamped(jx, jy);
                    den += w;
                }
            }
            out.set(x, y, if den > 0.0 { num / den } else { buf.get_clamped(xi, yi) });
        }
    }
    out
}

#[derive(Debug)]
struct NlMeans {
    opts: Options,
}

impl FrameFilter for NlMeans {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video {
            format,
            width,
            height,
            ..
        } = input.data
        else {
            return Ok(FrameOut::One(input));
        };
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        for p in 0..format.plane_count() {
            let plane_idx = p as u8;
            let Some((bytes, max_val)) = video::sample_layout(format, plane_idx) else {
                return Err(video::unsupported_format());
            };
            let (pw, ph) = video::plane_dims(format, width, height, plane_idx);
            let Some(src) = input.plane(p) else { continue };
            let read = PlaneBuf::read(src, pw, ph, bytes, max_val);
            let (pr, rr) = self.opts.radii_for(p);
            let result = nlmeans_plane(&read, self.opts.h, pr, rr);
            if let Some(mut dst) = out.plane_mut(p) {
                result.write(&mut dst, bytes);
            }
        }
        video::copy_meta(&mut out, &input);
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let opts = Options::parse(req);
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(NlMeans { opts }).with_timeline(Timeline::always())),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn a_flat_field_is_unchanged() {
        let mut buf = PlaneBuf::zeroed(10, 10, 255.0);
        for y in 0..10 {
            for x in 0..10 {
                buf.set(x, y, 90.0);
            }
        }
        let out = nlmeans_plane(&buf, 5.0, 1, 2);
        for v in out.as_slice() {
            assert!((v - 90.0).abs() < 1e-2, "{v}");
        }
    }

    fn lcg(seed: &mut u32) -> f32 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let n = ((*seed >> 16) & 0xff) as f32;
        n - 127.5
    }

    #[test]
    fn noise_on_a_flat_field_is_reduced() {
        let (w, h) = (12, 12);
        let mut buf = PlaneBuf::zeroed(w, h, 255.0);
        let mut seed = 99u32;
        for y in 0..h {
            for x in 0..w {
                buf.set(x, y, 128.0 + lcg(&mut seed) * 0.3);
            }
        }
        let noisy_var = buf.variance();
        // `h` well above the noise's own scale so patch-distance
        // differences (which are pure noise here, the underlying signal
        // being flat) barely affect the weight — pushing every pixel in the
        // research window toward equal weighting, i.e. toward a plain
        // spatial average over the window.
        let out = nlmeans_plane(&buf, 60.0, 2, 5);
        assert!(
            out.variance() < noisy_var * 0.5,
            "expected substantial variance reduction: {} vs {}",
            out.variance(),
            noisy_var
        );
    }
}
