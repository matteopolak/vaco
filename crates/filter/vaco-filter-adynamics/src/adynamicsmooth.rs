//! `adynamicsmooth` — apply dynamic smoothing of input audio.
//!
//! `ffmpeg -h filter=adynamicsmooth` (2026-08-27): `sensitivity` (0 to 1e6,
//! default 2), `basefreq` (2 to 1e6, default 22050). No `mix`, no
//! `attack`/`release` — a filter shaped entirely by those two knobs, which
//! is the signature of Andrew Simper (Cytomic)'s **Dynamic Smoothing Using a
//! Self-Modulating Filter** (Cytomic technical note, 2014, freely published)
//! rather than the envelope-follower-plus-static-curve shape every other
//! filter in this crate uses. That is a real, citable published algorithm —
//! not a transliteration of the reference's own source (D7) — and it is
//! implemented here from the paper's own description, then measured against
//! its own stated mathematical properties rather than against the reference
//! (which this project has no way to compare its internal coefficients to
//! without reading its source).
//!
//! # The algorithm
//!
//! Two cascaded one-pole (TPT/"topology-preserving transform") low-pass
//! filters sharing one time-varying coefficient `g`, self-modulated by how
//! fast the signal is currently changing:
//!
//! ```text
//! g0 = tan(pi * basefreq / fs) / (1 + tan(pi * basefreq / fs))   // base coefficient
//! band = low1 - low2                                              // "how fast is it moving"
//! g    = min(g0 + sensitivity * |band|, 1.0)                      // faster when moving fast
//! low1 += g * (x - low1)
//! low2 += g * (low1 - low2)
//! y = low2
//! ```
//!
//! The self-modulation is the whole point: a steady signal settles to a slow
//! `basefreq`-rate smoother (reducing noise/jitter), while a fast-moving
//! signal (a transient) briefly opens the filter up to track it closely
//! (avoiding the sluggish response a fixed low cutoff would otherwise cause)
//! — the paper's own stated design goal.
//!
//! # What is verified
//!
//! Two properties true of *any* correct implementation of this algorithm,
//! not a re-run of the same formula:
//!
//! * **Unity DC gain.** A cascade of two one-pole low-passes, each with
//!   unity DC gain by construction, must converge to a constant input
//!   exactly — checked by driving a constant signal for long enough that any
//!   reasonable `g` has settled.
//! * **`sensitivity = 0` degenerates to a fixed 2-pole low-pass.** With no
//!   self-modulation, `g` is pinned at `g0` for every sample, so the whole
//!   filter reduces to the textbook two-cascaded-one-pole-TPT-lowpass case —
//!   computed independently in the test from the one-pole difference
//!   equation directly, not by calling this module's own code twice.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "adynamicsmooth",
    description: "apply dynamic smoothing of input audio",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

/// One channel's pair of cascaded one-pole states.
#[derive(Debug, Clone, Copy, Default)]
struct Smoother {
    low1: f64,
    low2: f64,
}

impl Smoother {
    fn step(&mut self, x: f64, g0: f64, sensitivity: f64) -> f64 {
        let band = self.low1 - self.low2;
        let g = (sensitivity.mul_add(band.abs(), g0)).min(1.0);
        self.low1 += g * (x - self.low1);
        self.low2 += g * (self.low1 - self.low2);
        self.low2
    }
}

/// `g0 = tan(pi*fc/fs) / (1 + tan(pi*fc/fs))`, the TPT one-pole base
/// coefficient (Zavalishin, "The Art of VA Filter Design"; the same
/// normalisation Cytomic's own note uses).
fn base_coeff(basefreq: f64, sample_rate: f64) -> f64 {
    let fs = sample_rate.max(1.0);
    let fc = basefreq.clamp(2.0, fs * 0.499);
    let t = (std::f64::consts::PI * fc / fs).tan();
    t / (1.0 + t)
}

#[derive(Debug, Clone)]
struct DynamicSmooth {
    sensitivity: f64,
    basefreq: f64,
    g0: f64,
    states: Vec<Smoother>,
}

impl FrameFilter for DynamicSmooth {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio { sample_rate, .. }) = ctx.input_link(0) {
            self.g0 = base_coeff(self.basefreq, f64::from(*sample_rate));
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        if self.states.len() != channels.len() {
            self.states = vec![Smoother::default(); channels.len()];
        }
        for (ch, st) in channels.iter_mut().zip(self.states.iter_mut()) {
            for s in ch.iter_mut() {
                *s = st.step(*s, self.g0, self.sensitivity);
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

    fn flush_state(&mut self) {
        for s in &mut self.states {
            *s = Smoother::default();
        }
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let basefreq = common::f64_opt(req, &["basefreq"], 22050.0);
    let filter = DynamicSmooth {
        sensitivity: common::f64_opt(req, &["sensitivity"], 2.0),
        basefreq,
        g0: base_coeff(basefreq, 48_000.0),
        states: Vec::new(),
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

    /// Unity DC gain: a cascade of two unity-DC-gain one-poles must converge
    /// exactly to a held constant input, for any `sensitivity`.
    #[test]
    fn settles_exactly_on_a_constant_input() {
        for sensitivity in [0.0, 2.0, 1000.0] {
            let g0 = base_coeff(1000.0, 48_000.0);
            let mut s = Smoother::default();
            let mut y = 0.0;
            for _ in 0..20_000 {
                y = s.step(0.7, g0, sensitivity);
            }
            assert!((y - 0.7).abs() < 1e-6, "sensitivity {sensitivity}: y={y}");
        }
    }

    /// `sensitivity = 0` must reduce exactly to two cascaded fixed one-pole
    /// low-passes at `g0` — computed here from the raw one-pole difference
    /// equation directly, independent of [`Smoother::step`], so this is a
    /// real second implementation rather than the same code called twice.
    #[test]
    fn zero_sensitivity_matches_a_plain_two_pole_cascade() {
        let g0 = base_coeff(500.0, 48_000.0);
        let input: Vec<f64> = (0..200).map(|n| if n < 50 { 0.0 } else { 1.0 }).collect();

        let mut s = Smoother::default();
        let got: Vec<f64> = input.iter().map(|&x| s.step(x, g0, 0.0)).collect();

        let mut l1 = 0.0f64;
        let mut l2 = 0.0f64;
        let want: Vec<f64> = input
            .iter()
            .map(|&x| {
                l1 += g0 * (x - l1);
                l2 += g0 * (l1 - l2);
                l2
            })
            .collect();

        for (g, w) in got.iter().zip(want.iter()) {
            assert!((g - w).abs() < 1e-12, "{g} vs {w}");
        }
    }

    /// Falsification: a nonzero `sensitivity` must actually change the
    /// output during a transient (otherwise the self-modulation term is
    /// dead code) — checked against the `sensitivity = 0` trace on a step
    /// input, where the two must disagree partway through the transient.
    #[test]
    fn nonzero_sensitivity_changes_the_transient_response() {
        let g0 = base_coeff(200.0, 48_000.0);
        let input: Vec<f64> = (0..100).map(|n| if n < 10 { 0.0 } else { 1.0 }).collect();
        let mut s_fixed = Smoother::default();
        let mut s_dynamic = Smoother::default();
        let mut disagreed = false;
        for &x in &input {
            let a = s_fixed.step(x, g0, 0.0);
            let b = s_dynamic.step(x, g0, 50.0);
            if (a - b).abs() > 1e-6 {
                disagreed = true;
            }
        }
        assert!(disagreed, "sensitivity had no effect on the transient");
    }

    #[test]
    fn base_coeff_is_between_zero_and_one() {
        for fc in [2.0, 100.0, 1000.0, 22050.0] {
            let g = base_coeff(fc, 48_000.0);
            assert!((0.0..1.0).contains(&g), "fc={fc}: g={g}");
        }
    }
}
