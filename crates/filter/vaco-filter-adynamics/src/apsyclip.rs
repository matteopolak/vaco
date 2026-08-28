//! `apsyclip` — audio psychoacoustic clipper.
//!
//! `ffmpeg -h filter=apsyclip` (2026-08-27): `level_in`/`level_out`
//! (0.015625 to 64, default 1), `clip` (0.015625 to 1, default 1), `diff`
//! (bool, default false), `adaptive` (0 to 1, default 0.5), `iterations` (1
//! to 20, default 10), `level` (bool, default false — auto level).
//!
//! # What this is not
//!
//! A true psychoacoustic clipper predicts, per frequency band, how much
//! clipping distortion a masking model says is inaudible there, and clips
//! harder in bands where the ear will not notice — a real signal-processing
//! project (an auditory masking model plus an STFT analysis/synthesis
//! chain), not something this pass reconstructs from an options list. `-h`
//! gives the parameter names, not the masking model or the STFT parameters,
//! and inventing either would be exactly the "plausible invention" this
//! project's standing rule warns against.
//!
//! # What is built instead
//!
//! An **iterative corrective clipper**, per sample rather than across
//! samples: each of `iterations` passes computes how far the current
//! estimate overshoots `clip`, low-pass-smooths that overshoot amount with a
//! one-pole coefficient set by `adaptive` (`0` = no smoothing, the full
//! overshoot is corrected on the first pass; `1` = the correction barely
//! moves per pass, so more `iterations` converges gradually rather than
//! jumping straight to the ceiling), and subtracts the smoothed overshoot
//! from the *original* sample before re-checking against `clip`. This
//! converges the *pre-clamp* estimate towards the ceiling as `iterations`
//! grows for high `adaptive`; a final hard clamp then guarantees the output
//! never exceeds the ceiling even if the iterations have not fully
//! converged, which is the one behaviour this implementation commits to.
//! This is deliberately **not** the reference's cross-sample distortion
//! shaping (the real technique the "psychoacoustic" name implies spreads
//! clipping energy into neighbouring samples and frequency bands it
//! predicts are masked) — see the section above for why that was not
//! attempted. `diff` (output the correction alone) and `level` (auto output
//! normalisation) are accepted but not implemented — both change *what* is
//! reported, not the core clipping decision, and are the honestly-labelled
//! gap here rather than a guess.
//!
//! # What is verified
//!
//! Not a match to the reference (see above) but three real properties: the
//! output never exceeds `clip * level_out` in magnitude (the filter's one
//! hard guarantee, checked against an input that is entirely above the
//! ceiling), a signal already inside `[-clip, clip]` is left unchanged
//! (there is nothing to correct, so every iteration must be a no-op), and
//! the *pre-clamp* estimate's distance from the ceiling shrinks
//! monotonically as `iterations` grows for a fixed `adaptive` — the
//! convergence the algorithm is built to have, checked directly rather than
//! inferred from the final clamped output (which would hide a
//! non-converging inner loop behind the outer guarantee).

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "apsyclip",
    description: "audio psychoacoustic clipper",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

/// The pre-clamp iterative estimate — see module doc for the technique.
/// Pure function of its inputs so it is directly testable without a
/// channel/frame history: the smoothing state is local to one call, so each
/// output sample is independent of its neighbours in this simplified
/// (non-block) implementation.
fn converge(x: f64, clip: f64, adaptive: f64, iterations: u32) -> f64 {
    let alpha = adaptive.clamp(0.0, 1.0);
    let mut y = x;
    let mut smoothed_overshoot = 0.0;
    for _ in 0..iterations.max(1) {
        let raw_overshoot = y - y.clamp(-clip, clip);
        smoothed_overshoot = alpha.mul_add(smoothed_overshoot, (1.0 - alpha) * raw_overshoot);
        y = x - smoothed_overshoot;
    }
    y
}

/// [`converge`], then the one unconditional guarantee: never exceed `clip`.
fn correct(x: f64, clip: f64, adaptive: f64, iterations: u32) -> f64 {
    let clip = clip.clamp(1e-6, 1.0);
    converge(x, clip, adaptive, iterations).clamp(-clip, clip)
}

#[derive(Debug, Clone)]
struct PsyClip {
    level_in: f64,
    level_out: f64,
    clip: f64,
    adaptive: f64,
    iterations: u32,
}

impl FrameFilter for PsyClip {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        for ch in &mut channels {
            for s in ch.iter_mut() {
                let x = *s * self.level_in;
                *s = correct(x, self.clip, self.adaptive, self.iterations) * self.level_out;
            }
        }
        let mut out = crate::sample::encode(
            &vaco_frame::FramePool::default(),
            fmt,
            layout,
            rate,
            &channels,
        )?;
        out.pts = input.pts;
        out.time_base = input.time_base;
        out.duration = input.duration;
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let iterations = req
        .named("iterations")
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(10)
        .clamp(1, 20);
    let filter = PsyClip {
        level_in: common::f64_opt(req, &["level_in"], 1.0),
        level_out: common::f64_opt(req, &["level_out"], 1.0),
        clip: common::f64_opt(req, &["clip"], 1.0),
        adaptive: common::f64_opt(req, &["adaptive"], 0.5),
        iterations,
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter).with_timeline(Timeline::always())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one hard guarantee: output magnitude never exceeds `clip`.
    #[test]
    fn output_never_exceeds_the_ceiling() {
        for x in [-10.0, -3.0, -1.5, 1.5, 3.0, 10.0] {
            let y = correct(x, 0.8, 0.5, 10);
            assert!(y.abs() <= 0.8 + 1e-9, "x={x}: y={y}");
        }
    }

    /// A signal already inside the ceiling has nothing to correct.
    #[test]
    fn signal_inside_the_ceiling_is_unchanged() {
        for x in [-0.5, -0.1, 0.0, 0.3, 0.79] {
            let y = correct(x, 0.8, 0.5, 10);
            assert!((y - x).abs() < 1e-9, "x={x}: y={y}");
        }
    }

    /// Falsification: with `adaptive = 0` (no smoothing across iterations,
    /// each pass corrects the full instantaneous overshoot) a single
    /// iteration must already land exactly on the clamp — confirming the
    /// loop is not vacuously converging regardless of what it computes.
    #[test]
    fn zero_adaptive_converges_in_one_iteration() {
        let y1 = correct(2.0, 1.0, 0.0, 1);
        let y10 = correct(2.0, 1.0, 0.0, 10);
        assert!((y1 - 1.0).abs() < 1e-9, "{y1}");
        assert!((y10 - 1.0).abs() < 1e-9, "{y10}");
    }

    /// The real convergence property, checked pre-clamp so the outer
    /// guarantee cannot hide a non-converging inner loop: for a fixed,
    /// heavily-smoothing `adaptive`, more iterations must move the estimate
    /// monotonically closer to the ceiling.
    #[test]
    fn more_iterations_converge_monotonically_toward_the_ceiling() {
        let clip = 1.0;
        let mut prev_gap = (converge(1.3, clip, 0.9, 1) - clip).abs();
        for iterations in 2..=20 {
            let gap = (converge(1.3, clip, 0.9, iterations) - clip).abs();
            assert!(
                gap <= prev_gap + 1e-12,
                "iterations={iterations}: gap {gap} > previous {prev_gap}"
            );
            prev_gap = gap;
        }
        // And it has actually made progress, not stalled at the start.
        assert!(prev_gap < (converge(1.3, clip, 0.9, 1) - clip).abs());
    }
}
