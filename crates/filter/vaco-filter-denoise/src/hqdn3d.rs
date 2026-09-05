//! `hqdn3d` — spatial and temporal denoising blended by a difference-based
//! weight, independently per plane.
//!
//! # Options (`ffmpeg -h filter=hqdn3d`, probed 2026-08-23)
//!
//! `luma_spatial`/`chroma_spatial`/`luma_tmp`/`chroma_tmp`, each an `f64`
//! from `0` to `DBL_MAX`, each printed with default `0`.
//!
//! # A measured divergence from the option table's own printed default
//!
//! `0` is not actually "no filtering". Probed directly:
//!
//! ```text
//! ffmpeg -f lavfi -i testsrc2=size=64x64:rate=1:duration=1 -vf hqdn3d           -f framecrc  -> 0x1c315432
//! ffmpeg -f lavfi -i testsrc2=size=64x64:rate=1:duration=1 -vf hqdn3d=0:0:0:0   -f framecrc  -> 0x1c315432
//! ffmpeg -f lavfi -i testsrc2=size=64x64:rate=1:duration=1                     -f framecrc  -> 0x42205435
//! ```
//!
//! `hqdn3d` with no options and `hqdn3d=0:0:0:0` produce the identical
//! non-trivial output, both different from doing nothing at all. So the
//! reference substitutes its own built-in defaults whenever an argument is
//! `0` (or omitted) — the printed `AVOption` default of `0` is a sentinel
//! meaning "unset", not a literal strength of zero.
//!
//! Recovering the reference's exact substituted constants would mean either
//! reading its source (closed by D7) or a much larger black-box bisection
//! than this work package's budget allows. **This implementation takes `0`
//! literally**: `luma_spatial=0` genuinely disables spatial filtering on
//! luma, etc. That is a documented, deliberate divergence, not an oversight
//! — see `docs/filter/vaco-filter-denoise.md`.
//!
//! # Algorithm
//!
//! Independently per plane (luma strength for plane 0, chroma strength for
//! planes 1 and 2, higher-index planes left untouched):
//!
//! 1. **Spatial**: a horizontal then a vertical pass, each pixel blended
//!    with its two neighbours along that axis using an edge-preserving
//!    weight `w(d) = s / (s + d^2)` (`s` the plane's spatial strength, `d`
//!    the neighbour/centre difference) — the further apart two samples are,
//!    the less they mix, which is what keeps an edge from being blurred
//!    across. `s <= 0` skips the pass (identity).
//! 2. **Temporal**: blended against this filter instance's own previous
//!    *output* for that plane (not the previous input) with the same
//!    weight shape, so noise that survived one frame does not accumulate
//!    across many. The first frame after construction or after
//!    [`FrameFilter::flush_state`] has no previous output, so temporal
//!    filtering is skipped for it.
//!
//! # Independent oracles
//!
//! Not a byte-identity target (see the module's divergence note above), so
//! tests check properties no re-derivation of the same formula could fake:
//!
//! * **Flat-field invariant**: every neighbour difference is `0`, so every
//!   weight is `1` (spatial) or the blend has nothing to move toward
//!   (temporal); a constant frame stream comes back exactly constant,
//!   whatever the strengths are.
//! * **Noise-power bound**: an independently-seeded synthetic noisy plane's
//!   sample variance strictly decreases after spatial filtering, and a
//!   sequence of independently-noised frames' variance decreases further
//!   after temporal filtering — a property of *any* correct weighted
//!   average of correlated-signal/independent-noise samples, not of this
//!   file's specific weight formula.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Timeline};
use vaco_frame::{Frame, FrameData};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{self, PlaneBuf, VIDEO_PAD};

pub const DESC: FilterDesc = FilterDesc {
    name: "hqdn3d",
    description: "Apply a High Quality 3D Denoiser.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

/// The four options are also accepted positionally, in declaration order —
/// `hqdn3d=4:3:6:4` — the reference's own convention for this filter.
fn f64_opt(req: &Instantiate<'_>, keys: &[&str], position: usize, default: f64) -> f64 {
    for k in keys {
        if let Some(v) = req.named(k)
            && let Ok(f) = v.trim().parse::<f64>()
        {
            return f;
        }
    }
    if let Some(v) = req.positional(position)
        && let Ok(f) = v.trim().parse::<f64>()
    {
        return f;
    }
    default
}

#[derive(Debug, Clone, Copy)]
struct Options {
    luma_spatial: f32,
    chroma_spatial: f32,
    luma_tmp: f32,
    chroma_tmp: f32,
}

impl Options {
    fn parse(req: &Instantiate<'_>) -> Self {
        Self {
            #[allow(clippy::cast_possible_truncation, reason = "strength values are small")]
            luma_spatial: f64_opt(req, &["luma_spatial", "ls"], 0, 0.0) as f32,
            #[allow(clippy::cast_possible_truncation, reason = "strength values are small")]
            chroma_spatial: f64_opt(req, &["chroma_spatial", "cs"], 1, 0.0) as f32,
            #[allow(clippy::cast_possible_truncation, reason = "strength values are small")]
            luma_tmp: f64_opt(req, &["luma_tmp", "lt"], 2, 0.0) as f32,
            #[allow(clippy::cast_possible_truncation, reason = "strength values are small")]
            chroma_tmp: f64_opt(req, &["chroma_tmp", "ct"], 3, 0.0) as f32,
        }
    }

    fn strengths_for(&self, plane: usize) -> (f32, f32) {
        if plane == 0 {
            (self.luma_spatial, self.luma_tmp)
        } else {
            (self.chroma_spatial, self.chroma_tmp)
        }
    }
}

fn weight(strength: f32, diff: f32) -> f32 {
    if strength <= 0.0 {
        return 0.0;
    }
    if diff == 0.0 {
        return 1.0;
    }
    strength / (strength + diff * diff)
}

fn spatial_pass(buf: &PlaneBuf, spatial: f32) -> PlaneBuf {
    if spatial <= 0.0 {
        return buf.clone();
    }
    let (width, height) = (buf.width, buf.height);
    let mut horiz = buf.clone();
    for y in 0..height {
        for x in 0..width {
            let Some(center) = buf.get(x, y) else {
                continue;
            };
            #[allow(
                clippy::cast_possible_wrap,
                reason = "x/y are plane coordinates, far below i64 overflow"
            )]
            let (xi, yi) = (x as i64, y as i64);
            let left = buf.get_clamped(xi - 1, yi);
            let right = buf.get_clamped(xi + 1, yi);
            let wl = weight(spatial, left - center);
            let wr = weight(spatial, right - center);
            let denom = 1.0 + wl + wr;
            let blended = (center + wl * left + wr * right) / denom;
            horiz.set(x, y, blended);
        }
    }
    let mut out = horiz.clone();
    for y in 0..height {
        for x in 0..width {
            let Some(center) = horiz.get(x, y) else {
                continue;
            };
            #[allow(
                clippy::cast_possible_wrap,
                reason = "x/y are plane coordinates, far below i64 overflow"
            )]
            let (xi, yi) = (x as i64, y as i64);
            let up = horiz.get_clamped(xi, yi - 1);
            let down = horiz.get_clamped(xi, yi + 1);
            let wu = weight(spatial, up - center);
            let wd = weight(spatial, down - center);
            let denom = 1.0 + wu + wd;
            let blended = (center + wu * up + wd * down) / denom;
            out.set(x, y, blended);
        }
    }
    out
}

fn temporal_pass(cur: &PlaneBuf, prev: Option<&PlaneBuf>, strength: f32) -> PlaneBuf {
    let Some(prev) = prev.filter(|p| p.width == cur.width && p.height == cur.height) else {
        return cur.clone();
    };
    if strength <= 0.0 {
        return cur.clone();
    }
    let mut out = cur.clone();
    for y in 0..cur.height {
        for x in 0..cur.width {
            let (Some(c), Some(p)) = (cur.get(x, y), prev.get(x, y)) else {
                continue;
            };
            let w = weight(strength, c - p);
            out.set(x, y, p + w * (c - p));
        }
    }
    out
}

#[derive(Debug)]
struct Hqdn3d {
    opts: Options,
    prev: Vec<Option<PlaneBuf>>,
}

impl Hqdn3d {
    fn new(opts: Options) -> Self {
        Self {
            opts,
            prev: Vec::new(),
        }
    }
}

impl FrameFilter for Hqdn3d {
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
        let plane_count = format.plane_count();
        if self.prev.len() < plane_count {
            self.prev.resize(plane_count, None);
        }
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        for p in 0..plane_count {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "plane_count() is at most 4"
            )]
            let plane_idx = p as u8;
            let Some((bytes, max_val)) = video::sample_layout(format, plane_idx) else {
                return Err(video::unsupported_format());
            };
            let (pw, ph) = video::plane_dims(format, width, height, plane_idx);
            let Some(src) = input.plane(p) else { continue };
            let read = PlaneBuf::read(src, pw, ph, bytes, max_val);
            let (spatial, temporal) = self.opts.strengths_for(p);
            let spatially = spatial_pass(&read, spatial);
            let prev = self.prev.get(p).and_then(Option::as_ref);
            let filtered = temporal_pass(&spatially, prev, temporal);
            if let Some(mut dst) = out.plane_mut(p) {
                filtered.write(&mut dst, bytes);
            }
            if let Some(slot) = self.prev.get_mut(p) {
                *slot = Some(filtered);
            }
        }
        video::copy_meta(&mut out, &input);
        Ok(FrameOut::One(out))
    }

    fn flush_state(&mut self) {
        self.prev.clear();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let opts = Options::parse(req);
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Hqdn3d::new(opts)).with_timeline(Timeline::always())),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn flat(w: usize, h: usize, v: f32) -> PlaneBuf {
        let mut buf = PlaneBuf::zeroed(w, h, 255.0);
        for y in 0..h {
            for x in 0..w {
                buf.set(x, y, v);
            }
        }
        buf
    }

    #[test]
    fn spatial_pass_leaves_a_flat_field_unchanged() {
        let buf = flat(8, 8, 100.0);
        let out = spatial_pass(&buf, 4.0);
        for y in 0..8 {
            for x in 0..8 {
                assert!((out.get(x, y).unwrap() - 100.0).abs() < 1e-3);
            }
        }
    }

    #[test]
    fn zero_strength_spatial_pass_is_identity() {
        let buf = flat(4, 4, 30.0);
        let out = spatial_pass(&buf, 0.0);
        assert_eq!(out.as_slice(), buf.as_slice());
    }

    #[test]
    fn temporal_pass_with_no_history_is_identity() {
        let buf = flat(4, 4, 30.0);
        let out = temporal_pass(&buf, None, 5.0);
        assert_eq!(out.as_slice(), buf.as_slice());
    }

    fn lcg_noise(seed: &mut u32) -> f32 {
        *seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        #[allow(clippy::cast_precision_loss, reason = "test-only noise generator")]
        let n = ((*seed >> 16) & 0xff) as f32;
        n - 127.5
    }

    #[test]
    fn spatial_pass_reduces_noise_variance() {
        let (w, h) = (32, 32);
        let mut buf = PlaneBuf::zeroed(w, h, 255.0);
        let mut seed = 1u32;
        for y in 0..h {
            for x in 0..w {
                buf.set(x, y, 128.0 + lcg_noise(&mut seed));
            }
        }
        let noisy_var = buf.variance();
        let out = spatial_pass(&buf, 20.0);
        assert!(
            out.variance() < noisy_var,
            "expected reduced variance: {} vs {}",
            out.variance(),
            noisy_var
        );
    }

    #[test]
    fn temporal_pass_reduces_variance_across_independent_noise() {
        // `weight(strength, diff) = strength / (strength + diff^2)` blends
        // *less* as `diff` grows, so the strength has to be on the same
        // order as the noise's own variance for the blend to do anything —
        // strength far below the noise variance gives a weight near zero
        // every step (no smoothing at all), which is what an earlier,
        // untuned version of this test got wrong. Noise amplitude here is
        // scaled down accordingly, and the pass/fail bound is measured
        // against this run's own single-frame variance rather than a
        // hand-picked constant.
        let (w, h) = (16, 16);
        let mut seed = 7u32;
        let mut prev: Option<PlaneBuf> = None;
        let mut last_var = f32::MAX;
        let mut last_single_frame_var = 0.0f32;
        for _ in 0..24 {
            let mut cur = PlaneBuf::zeroed(w, h, 255.0);
            for y in 0..h {
                for x in 0..w {
                    cur.set(x, y, 128.0 + lcg_noise(&mut seed) * 0.15);
                }
            }
            last_single_frame_var = cur.variance();
            let filtered = temporal_pass(&cur, prev.as_ref(), 100.0);
            last_var = filtered.variance();
            prev = Some(filtered);
        }
        assert!(
            last_var < last_single_frame_var * 0.6,
            "expected the temporally-averaged variance ({last_var}) well below \
             a single frame's own noise variance ({last_single_frame_var})"
        );
    }
}
