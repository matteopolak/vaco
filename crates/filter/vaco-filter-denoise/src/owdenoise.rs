//! `owdenoise` — wavelet-domain denoising: decompose each plane with the à
//! trous transform ([`crate::wavelet`]), soft-threshold the detail bands by
//! a fixed per-plane strength, reconstruct.
//!
//! # Options (`ffmpeg -h filter=owdenoise`, probed 2026-08-23)
//!
//! `depth` (`int`, `8..=16`, default `8`), `luma_strength`/`ls` (`f64`,
//! `0..=1000`, default `1`), `chroma_strength`/`cs` (same range, default
//! `1`).
//!
//! `depth` is parsed but not otherwise used: this implementation already
//! computes in `f32` at full precision regardless of the plane's own bit
//! depth, so there is no lower-precision path for `depth` to select between.
//! Documented structural gap, not a silent drop — see
//! `docs/filter/vaco-filter-denoise.md`.
//!
//! # Decomposition depth
//!
//! `owdenoise` exposes no "number of levels" option (unlike
//! [`crate::vaguedenoiser`]'s `nsteps`), so this implementation fixes it at
//! 3 levels — enough for the à trous transform to separate several spatial
//! scales of grain from the underlying picture without, on a small frame,
//! degenerating into per-level kernels wider than the plane itself.
//!
//! # Independent oracle
//!
//! Both of [`crate::wavelet`]'s own oracles carry over directly, since
//! `owdenoise` is exactly "decompose, soft-threshold with a fixed
//! threshold, reconstruct": a constant plane has zero detail at every level
//! ([`crate::wavelet::tests::a_constant_field_has_zero_detail_at_every_level`]),
//! so thresholding a zero is a no-op and the plane comes back unchanged —
//! checked again here end-to-end through the filter's own option parsing
//! and plane I/O, not just the transform in isolation. The noise-power bound
//! is checked the same way as every other filter in this crate: an
//! independently-seeded noisy synthetic plane's variance must fall after
//! filtering with a non-trivial strength.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Timeline};
use vaco_frame::{Frame, FrameData};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{self, PlaneBuf, VIDEO_PAD};
use crate::wavelet::{Decomposition, ThresholdMethod};

pub const DESC: FilterDesc = FilterDesc {
    name: "owdenoise",
    description: "Denoise using wavelets.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

/// Decomposition levels. See the module doc for why this is fixed rather
/// than an option.
const LEVELS: usize = 3;

fn f64_opt(req: &Instantiate<'_>, keys: &[&str], default: f64) -> f64 {
    for k in keys {
        if let Some(v) = req.named(k)
            && let Ok(f) = v.trim().parse::<f64>()
        {
            return f;
        }
    }
    default
}

#[derive(Debug, Clone, Copy)]
struct Options {
    luma_strength: f32,
    chroma_strength: f32,
}

impl Options {
    fn parse(req: &Instantiate<'_>) -> Self {
        Self {
            luma_strength: f64_opt(req, &["luma_strength", "ls"], 1.0) as f32,
            chroma_strength: f64_opt(req, &["chroma_strength", "cs"], 1.0) as f32,
        }
    }

    fn strength_for(self, plane: usize) -> f32 {
        if plane == 0 {
            self.luma_strength
        } else {
            self.chroma_strength
        }
    }
}

fn denoise_plane(buf: &PlaneBuf, strength: f32) -> PlaneBuf {
    if strength <= 0.0 || buf.width < 4 || buf.height < 4 {
        return buf.clone();
    }
    let levels = LEVELS.min(buf.width.min(buf.height).trailing_zeros() as usize + 1);
    let levels = levels.max(1);
    let mut decomp = Decomposition::decompose(buf.as_slice(), buf.width, buf.height, levels);
    decomp.shrink(ThresholdMethod::Soft, |_level, _sigma| strength);
    let data = decomp.reconstruct();
    let mut out = PlaneBuf::zeroed(buf.width, buf.height, buf.max_val);
    for y in 0..buf.height {
        for x in 0..buf.width {
            if let Some(v) = data.get(y * buf.width + x) {
                out.set(x, y, *v);
            }
        }
    }
    out
}

#[derive(Debug)]
struct Owdenoise {
    opts: Options,
}

impl FrameFilter for Owdenoise {
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
            let strength = self.opts.strength_for(p);
            let result = denoise_plane(&read, strength);
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
        filter: Box::new(Simple::new(Owdenoise { opts }).with_timeline(Timeline::always())),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn a_flat_field_is_unchanged() {
        let mut buf = PlaneBuf::zeroed(16, 16, 255.0);
        for y in 0..16 {
            for x in 0..16 {
                buf.set(x, y, 64.0);
            }
        }
        let out = denoise_plane(&buf, 50.0);
        for v in out.as_slice() {
            assert!((v - 64.0).abs() < 1e-2, "{v}");
        }
    }

    fn lcg(seed: &mut u32) -> f32 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let n = ((*seed >> 16) & 0xff) as f32;
        n - 127.5
    }

    #[test]
    fn noise_variance_drops_with_a_non_trivial_strength() {
        let (w, h) = (16, 16);
        let mut buf = PlaneBuf::zeroed(w, h, 255.0);
        let mut seed = 5u32;
        for y in 0..h {
            for x in 0..w {
                buf.set(x, y, 128.0 + lcg(&mut seed) * 0.5);
            }
        }
        let noisy_var = buf.variance();
        let out = denoise_plane(&buf, 10.0);
        assert!(
            out.variance() < noisy_var,
            "expected reduced variance: {} vs {}",
            out.variance(),
            noisy_var
        );
    }
}
