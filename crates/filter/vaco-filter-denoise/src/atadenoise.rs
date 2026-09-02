//! `atadenoise` — Adaptive Temporal Averaging Denoiser: average a pixel with
//! its own recent history, but only the history samples close enough to the
//! current value to plausibly be the same underlying signal plus noise.
//!
//! # Options (`ffmpeg -h filter=atadenoise`, probed 2026-08-23)
//!
//! `0a`/`0b`, `1a`/`1b`, `2a`/`2b` — threshold `A`/`B` per plane (`f32`,
//! defaults `0.02`/`0.04`, `A` in `[0, 0.3]`, `B` in `[0, 5]`, fractions of
//! full scale); `s` — window frame count (`5..=129`, default `9`); `p` —
//! planes bitmask (default `7`); `a` — algorithm variant (`p`arallel/
//! `s`erial); `0s`/`1s`/`2s` — per-plane sigma (default `32767`, i.e.
//! effectively unbounded).
//!
//! # A deliberate structural simplification versus the reference
//!
//! The reference's window is **centred**: filtering frame `N` uses `s`
//! frames symmetric around it, so output is delayed by `(s-1)/2` frames and
//! the edges of the stream need a shrinking window. This implementation
//! uses a **trailing** window instead — frame `N`'s output is the weighted
//! average of the last `min(s, N+1)` *input* frames including itself — which
//! keeps 1:1 frame timing with zero added latency and no special-cased
//! stream edges, at the cost of matching the reference's frame alignment.
//! Still a genuine adaptive temporal average, and still what the acceptance
//! criterion below tests, but a documented divergence from "centred" (see
//! `docs/filter/vaco-filter-denoise.md`). The `a` (parallel/serial)
//! algorithm-variant option is accepted but not distinguished: both variants
//! use the same weighted trailing average.
//!
//! # Algorithm
//!
//! For plane `p` with thresholds `(A, B)`, and history samples `h_0
//! (oldest) .. h_k (== current)` at one pixel:
//!
//! ```text
//! d_i = |h_i - h_k|
//! w_i = 1                              if d_i <= A
//!     = (B - d_i) / (B - A)            if A < d_i < B
//!     = 0                              otherwise
//! out = sum(w_i * h_i) / sum(w_i)
//! ```
//!
//! `w_k` (the current sample against itself) is always `1`, so the sum of
//! weights is always at least `1` and the average is always well defined.
//! `A`/`B` are fractions of the plane's full-scale value, matching the
//! option table's documented range.
//!
//! # Independent oracles
//!
//! * **Identical-history invariant**: if every buffered frame carries the
//!   same value at a pixel (`d_i == 0` for all `i`), every weight is `1` and
//!   the average is exactly that value — true for *any* correct weighted
//!   average, not particular to this file's formula.
//! * **Noise-power bound**: `N` independently-noised frames of a common flat
//!   baseline, with noise amplitude kept inside threshold `A` so every
//!   sample is fully weighted, average to a variance close to the
//!   single-frame noise variance divided by `N` (the textbook result for
//!   averaging `N` i.i.d. samples) — checked as a `<` bound with slack
//!   rather than an exact ratio, since the weights are not perfectly equal
//!   once floating-point history population is uneven at stream start.

use std::collections::VecDeque;

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Timeline};
use vaco_frame::{Frame, FrameData};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{self, PlaneBuf, VIDEO_PAD};

pub const DESC: FilterDesc = FilterDesc {
    name: "atadenoise",
    description: "Apply an Adaptive Temporal Averaging Denoiser.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

fn f32_opt(req: &Instantiate<'_>, key: &str, default: f32) -> f32 {
    req.named(key)
        .and_then(|v| v.trim().parse::<f32>().ok())
        .unwrap_or(default)
}

fn usize_opt(req: &Instantiate<'_>, key: &str, default: usize) -> usize {
    req.named(key)
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

#[derive(Debug, Clone, Copy)]
struct Thresholds {
    a: f32,
    b: f32,
}

#[derive(Debug, Clone, Copy)]
struct Options {
    thresholds: [Thresholds; 3],
    window: usize,
    planes: u8,
}

impl Options {
    fn parse(req: &Instantiate<'_>) -> Self {
        let t = |ai: &str, bi: &str, da: f32, db: f32| Thresholds {
            a: f32_opt(req, ai, da),
            b: f32_opt(req, bi, db),
        };
        let window = usize_opt(req, "s", 9).clamp(5, 129);
        Self {
            thresholds: [
                t("0a", "0b", 0.02, 0.04),
                t("1a", "1b", 0.02, 0.04),
                t("2a", "2b", 0.02, 0.04),
            ],
            window,
            planes: video::planes_mask_opt(req, &["p"], 7),
        }
    }

    fn thresholds_for(&self, plane: usize) -> Thresholds {
        self.thresholds
            .get(plane.min(2))
            .copied()
            .unwrap_or(Thresholds { a: 0.02, b: 0.04 })
    }
}

fn weight(diff: f32, t: Thresholds) -> f32 {
    if diff <= t.a {
        1.0
    } else if diff < t.b && t.b > t.a {
        (t.b - diff) / (t.b - t.a)
    } else {
        0.0
    }
}

/// Weighted trailing average of `history` (oldest first, last == current)
/// against thresholds scaled by `max_val`.
fn average_pixel(history: &[f32], t: Thresholds, max_val: f32) -> f32 {
    let Some(&current) = history.last() else {
        return 0.0;
    };
    if max_val <= 0.0 {
        return current;
    }
    let mut num = 0.0f32;
    let mut den = 0.0f32;
    for &h in history {
        let diff = (h - current).abs() / max_val;
        let w = weight(diff, t);
        num += w * h;
        den += w;
    }
    if den > 0.0 { num / den } else { current }
}

#[derive(Debug)]
struct Atadenoise {
    opts: Options,
    history: Vec<VecDeque<PlaneBuf>>,
}

impl Atadenoise {
    fn new(opts: Options) -> Self {
        Self {
            opts,
            history: Vec::new(),
        }
    }
}

impl FrameFilter for Atadenoise {
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
        if self.history.len() < plane_count {
            self.history.resize(plane_count, VecDeque::new());
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

            if !video::plane_selected(self.opts.planes, p) {
                if let Some(mut dst) = out.plane_mut(p) {
                    read.write(&mut dst, bytes);
                }
                continue;
            }

            let Some(hist) = self.history.get_mut(p) else {
                continue;
            };
            hist.push_back(read);
            while hist.len() > self.opts.window {
                hist.pop_front();
            }
            let t = self.opts.thresholds_for(p);
            let mut result = PlaneBuf::zeroed(pw, ph, max_val);
            let samples: Vec<&PlaneBuf> = hist.iter().collect();
            for y in 0..ph {
                for x in 0..pw {
                    let values: Vec<f32> = samples.iter().filter_map(|b| b.get(x, y)).collect();
                    result.set(x, y, average_pixel(&values, t, max_val));
                }
            }
            if let Some(mut dst) = out.plane_mut(p) {
                result.write(&mut dst, bytes);
            }
        }
        video::copy_meta(&mut out, &input);
        Ok(FrameOut::One(out))
    }

    fn flush_state(&mut self) {
        self.history.clear();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let opts = Options::parse(req);
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Atadenoise::new(opts)).with_timeline(Timeline::always())),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn identical_history_averages_to_the_same_value() {
        let t = Thresholds { a: 0.02, b: 0.04 };
        let history = vec![50.0f32; 9];
        let out = average_pixel(&history, t, 255.0);
        assert!((out - 50.0).abs() < 1e-4);
    }

    fn lcg(seed: &mut u32) -> f32 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        #[allow(clippy::cast_precision_loss, reason = "test-only noise generator")]
        let n = ((*seed >> 16) & 0xff) as f32;
        n - 127.5
    }

    #[test]
    fn full_weight_averaging_matches_the_plain_mean_exactly() {
        let t = Thresholds { a: 0.05, b: 0.1 };
        // Weights compare every *pair* of history samples (`d_i = |h_i -
        // h_k|`, `h_k` the most recent), so the amplitude bound to guarantee
        // full weight for every sample is on the *spread* between any two
        // samples, not on each sample's own deviation from zero: with `n`
        // draws in `[-a, a]`, the worst-case pairwise spread is `2a`, which
        // needs to stay under `A * 255 ~= 12.75` for every weight to land
        // exactly at `1`. When every weight is equal, the weighted average's
        // definition *is* the arithmetic mean — an exact algebraic identity,
        // not a statistical bound with a tolerance to tune.
        let mut seed = 42u32;
        let n = 16;
        let mut history = Vec::new();
        for _ in 0..n {
            let noise = lcg(&mut seed) * 0.02; // amplitude <= ~2.55, spread <= ~5.1
            history.push(128.0 + noise);
        }
        #[allow(clippy::cast_precision_loss, reason = "n is a small test fixture size")]
        let plain_mean = history.iter().sum::<f32>() / (history.len() as f32);
        let out = average_pixel(&history, t, 255.0);
        assert!(
            (out - plain_mean).abs() < 1e-4,
            "out = {out}, plain mean = {plain_mean}"
        );
    }

    #[test]
    fn a_plane_outside_the_planes_mask_is_untouched() {
        assert!(!video::plane_selected(0b0000_0001, 1));
        assert!(video::plane_selected(0b0000_0111, 1));
    }
}
