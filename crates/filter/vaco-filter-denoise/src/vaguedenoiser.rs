//! `vaguedenoiser` — à trous wavelet decomposition ([`crate::wavelet`]),
//! coefficient shrinkage by a user-chosen method and threshold rule,
//! reconstruction blended with the original by `percent`.
//!
//! # Options (`ffmpeg -h filter=vaguedenoiser`, probed 2026-08-23)
//!
//! `threshold` (`f32`, default `2`), `method` (`hard`/`soft`/`garrote`,
//! default `garrote`), `nsteps` (`int`, `1..=32`, default `6`), `percent`
//! (`f32`, `0..=100`, default `85`), `planes` (bitmask, default `15`),
//! `type` (`universal`/`bayes`, default `universal`).
//!
//! # Algorithm
//!
//! `nsteps` decomposition levels (capped by plane size — see
//! [`levels_for`]). Per level, the threshold is:
//!
//! * **`universal`**: `threshold * sigma * sqrt(2 * ln(N))` — `VisuShrink`
//!   (Donoho & Johnstone 1994), `sigma` the finest band's robust
//!   MAD-based noise estimate ([`crate::wavelet::Decomposition::
//!   finest_band_sigma`]) and `N` that level's sample count. The user's
//!   `threshold` option scales the textbook formula rather than replacing
//!   it, since a fixed threshold with no data-dependence at all would make
//!   the option pointless on a plane whose noise level it cannot see.
//! * **`bayes`**: `threshold * `[`crate::wavelet::bayes_threshold`] per
//!   band — `BayesShrink` (Chang, Yu & Vetterli 2000), which adapts to each
//!   band's own signal-to-noise ratio rather than using one global formula.
//!
//! `method` picks hard/soft/garrote shrinkage on top of whichever threshold
//! was selected. `percent` blends the shrunk reconstruction back toward the
//! original: `out = original * (1 - percent/100) + denoised * (percent/100)`
//! — the reference's own documented meaning for the option ("percent of
//! full denoising").
//!
//! # Independent oracles
//!
//! Same shape as [`crate::owdenoise`], since both are built on
//! [`crate::wavelet`]: a flat plane has zero detail at every level, so
//! shrinkage is a no-op and `percent < 100` cannot introduce any drift
//! either (blending an unchanged value with itself is that value); a
//! synthetic noisy plane's variance must fall after filtering with
//! `percent = 100` and a non-trivial threshold.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Timeline};
use vaco_frame::{Frame, FrameData};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{self, PlaneBuf, VIDEO_PAD};
use crate::wavelet::{self, Decomposition, ThresholdMethod};

pub const DESC: FilterDesc = FilterDesc {
    name: "vaguedenoiser",
    description: "Apply a Wavelet based Denoiser.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThresholdType {
    Universal,
    Bayes,
}

#[derive(Debug, Clone, Copy)]
struct Options {
    threshold: f32,
    method: ThresholdMethod,
    nsteps: usize,
    percent: f32,
    planes: u8,
    kind: ThresholdType,
}

impl Options {
    fn parse(req: &Instantiate<'_>) -> Self {
        let threshold = req
            .named("threshold")
            .and_then(|v| v.trim().parse::<f32>().ok())
            .unwrap_or(2.0)
            .max(0.0);
        let method = match req.named("method").as_deref() {
            Some("0" | "hard") => ThresholdMethod::Hard,
            Some("1" | "soft") => ThresholdMethod::Soft,
            _ => ThresholdMethod::Garrote,
        };
        let nsteps = req
            .named("nsteps")
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(6)
            .clamp(1, 32);
        let percent = req
            .named("percent")
            .and_then(|v| v.trim().parse::<f32>().ok())
            .unwrap_or(85.0)
            .clamp(0.0, 100.0);
        let kind = match req.named("type").as_deref() {
            Some("1" | "bayes") => ThresholdType::Bayes,
            _ => ThresholdType::Universal,
        };
        Self {
            threshold,
            method,
            nsteps,
            percent,
            planes: video::planes_mask_opt(req, &["planes"], 15),
            kind,
        }
    }
}

/// Decomposition levels for a `min(width, height)`-sized plane: `nsteps`
/// capped so the coarsest level's à trous kernel (`2^level` sample holes)
/// stays smaller than the plane itself.
fn levels_for(nsteps: usize, min_dim: usize) -> usize {
    let cap = min_dim.max(2).ilog2().max(1) as usize;
    nsteps.min(cap).max(1)
}

fn denoise_plane(buf: &PlaneBuf, opts: &Options) -> PlaneBuf {
    if opts.threshold <= 0.0 && opts.percent <= 0.0 {
        return buf.clone();
    }
    if buf.width < 4 || buf.height < 4 {
        return buf.clone();
    }
    let levels = levels_for(opts.nsteps, buf.width.min(buf.height));
    let mut decomp = Decomposition::decompose(buf.as_slice(), buf.width, buf.height, levels);
    let user_threshold = opts.threshold;
    let kind = opts.kind;
    match kind {
        ThresholdType::Universal => {
            let sigma = decomp.finest_band_sigma();
            decomp.shrink(opts.method, move |_level, s| {
                let sigma = if s > 0.0 { s } else { sigma };
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "band sample counts are far below f32's exact-integer range"
                )]
                let n = buf.width.saturating_mul(buf.height) as f32;
                user_threshold * sigma * (2.0 * n.max(2.0).ln()).sqrt()
            });
        }
        ThresholdType::Bayes => {
            let bands: Vec<f32> = {
                let sigma = decomp.finest_band_sigma();
                decomp
                    .details
                    .iter()
                    .map(|band| user_threshold * wavelet::bayes_threshold(band, sigma))
                    .collect()
            };
            let mut idx = 0usize;
            decomp.shrink(opts.method, move |_level, _sigma| {
                let t = bands.get(idx).copied().unwrap_or(0.0);
                idx = idx.saturating_add(1);
                t
            });
        }
    }
    let denoised = decomp.reconstruct();
    let mix = opts.percent / 100.0;
    let mut out = PlaneBuf::zeroed(buf.width, buf.height, buf.max_val);
    for y in 0..buf.height {
        for x in 0..buf.width {
            let Some(orig) = buf.get(x, y) else { continue };
            let idx = y.saturating_mul(buf.width).saturating_add(x);
            let d = denoised.get(idx).copied().unwrap_or(orig);
            out.set(x, y, orig * (1.0 - mix) + d * mix);
        }
    }
    out
}

#[derive(Debug)]
struct VagueDenoiser {
    opts: Options,
}

impl FrameFilter for VagueDenoiser {
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
            let result = if video::plane_selected(self.opts.planes, p) {
                denoise_plane(&read, &self.opts)
            } else {
                read
            };
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
        filter: Box::new(Simple::new(VagueDenoiser { opts }).with_timeline(Timeline::always())),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn default_opts() -> Options {
        Options {
            threshold: 2.0,
            method: ThresholdMethod::Garrote,
            nsteps: 4,
            percent: 100.0,
            planes: 15,
            kind: ThresholdType::Universal,
        }
    }

    #[test]
    fn a_flat_field_is_unchanged() {
        let mut buf = PlaneBuf::zeroed(16, 16, 255.0);
        for y in 0..16 {
            for x in 0..16 {
                buf.set(x, y, 33.0);
            }
        }
        let out = denoise_plane(&buf, &default_opts());
        for v in out.as_slice() {
            assert!((v - 33.0).abs() < 1e-2, "{v}");
        }
    }

    fn lcg(seed: &mut u32) -> f32 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let n = ((*seed >> 16) & 0xff) as f32;
        n - 127.5
    }

    #[test]
    fn noise_variance_drops_for_both_threshold_types() {
        let (w, h) = (16, 16);
        for kind in [ThresholdType::Universal, ThresholdType::Bayes] {
            let mut buf = PlaneBuf::zeroed(w, h, 255.0);
            let mut seed = 21u32;
            for y in 0..h {
                for x in 0..w {
                    buf.set(x, y, 128.0 + lcg(&mut seed) * 0.6);
                }
            }
            let noisy_var = buf.variance();
            let mut opts = default_opts();
            opts.kind = kind;
            opts.threshold = 3.0;
            let out = denoise_plane(&buf, &opts);
            assert!(
                out.variance() < noisy_var,
                "{kind:?}: expected reduced variance: {} vs {}",
                out.variance(),
                noisy_var
            );
        }
    }

    #[test]
    fn zero_percent_is_the_identity_regardless_of_threshold() {
        let mut buf = PlaneBuf::zeroed(16, 16, 255.0);
        let mut seed = 3u32;
        for y in 0..16 {
            for x in 0..16 {
                buf.set(x, y, 128.0 + lcg(&mut seed) * 0.6);
            }
        }
        let mut opts = default_opts();
        opts.percent = 0.0;
        opts.threshold = 5.0;
        let out = denoise_plane(&buf, &opts);
        for (a, b) in out.as_slice().iter().zip(buf.as_slice().iter()) {
            assert!((a - b).abs() < 1e-3);
        }
    }
}
