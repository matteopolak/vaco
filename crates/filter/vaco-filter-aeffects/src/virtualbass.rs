//! `virtualbass` — audio virtual bass (psychoacoustic bass enhancement).
//!
//! `ffmpeg -h filter=virtualbass` (2026-08-23): `cutoff` (`100..500` Hz,
//! default `250`), `strength` (`0.5..3`, default `3`). Supports timeline
//! (`enable`) on `strength` only.
//!
//! # What is structural, not measured
//!
//! Virtual bass is a psychoacoustic technique: frequencies below `cutoff`
//! are usually inaudible on small speakers, so instead of reproducing them
//! directly, the missing fundamental is replaced with its own harmonics
//! (which *are* reproducible) and the ear's harmonic-series perception
//! fills in the missing fundamental. This crate has no biquad design of
//! its own (`vaco-filter-audio-eq` owns that and does not export it across
//! crates), so this implementation isolates the sub-`cutoff` band with a
//! simple one-pole low-pass, generates harmonics with a `tanh` saturator,
//! removes the residual fundamental from the harmonic signal with a second
//! one-pole low-pass subtraction (leaving just the new harmonics), and adds
//! that back scaled by `strength`. Not claimed to be sample-exact; see
//! `docs/filter/vaco-filter-aeffects.md`.
//!
//! `strength`'s documented range (`0.5..3`) has no zero, so there is no
//! identity case to measure or reproduce — [`tests::larger_strength_adds_more_energy`]
//! instead checks the one property this design guarantees regardless of
//! its exact shape: more `strength` adds more harmonic energy, monotonically.
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "virtualbass",
    description: "audio virtual bass",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, Copy, Default)]
struct OnePole {
    y: f64,
}

impl OnePole {
    fn low(&mut self, x: f64, a: f64) -> f64 {
        self.y += a * (x - self.y);
        self.y
    }
}

struct ChannelState {
    sub_lp: OnePole,
    harm_lp: OnePole,
}

struct Virtualbass {
    cutoff: f64,
    strength: f64,
    coeff: f64,
    channels: Vec<ChannelState>,
}

impl FrameFilter for Virtualbass {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio {
            sample_rate,
            layout,
            ..
        }) = ctx.input_link(0)
        {
            let count = layout.channels.max(1) as usize;
            let rate = f64::from(*sample_rate).max(1.0);
            self.coeff = (std::f64::consts::TAU * self.cutoff / rate).clamp(0.001, 1.0);
            self.channels = (0..count)
                .map(|_| ChannelState {
                    sub_lp: OnePole::default(),
                    harm_lp: OnePole::default(),
                })
                .collect();
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        let drive = 2.0 + self.strength; // more strength -> harder saturation -> richer harmonics
        for (idx, channel) in channels.iter_mut().enumerate() {
            let Some(state) = self.channels.get_mut(idx) else {
                continue;
            };
            for sample in channel.iter_mut() {
                let dry = *sample;
                let sub = state.sub_lp.low(dry, self.coeff);
                let harmonics = (sub * drive).tanh();
                let harmonics_high = harmonics - state.harm_lp.low(harmonics, self.coeff);
                *sample = dry + (self.strength / 3.0) * harmonics_high;
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
        for state in &mut self.channels {
            state.sub_lp = OnePole::default();
            state.harm_lp = OnePole::default();
        }
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let cutoff = common::f64_opt(req, &["cutoff"], 250.0).clamp(100.0, 500.0);
    let strength = common::f64_opt(req, &["strength"], 3.0).clamp(0.5, 3.0);
    let filter = Virtualbass {
        cutoff,
        strength,
        coeff: 0.1,
        channels: Vec::new(),
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

    fn energy(strength: f64, coeff: f64, input: &[f64]) -> f64 {
        let mut sub_lp = OnePole::default();
        let mut harm_lp = OnePole::default();
        let drive = 2.0 + strength;
        let mut sum = 0.0;
        for &dry in input {
            let sub = sub_lp.low(dry, coeff);
            let harmonics = (sub * drive).tanh();
            let harmonics_high = harmonics - harm_lp.low(harmonics, coeff);
            let added = (strength / 3.0) * harmonics_high;
            sum += added * added;
        }
        sum
    }

    /// Falsifiable monotonicity check: within the documented `strength`
    /// range, more strength must add at least as much harmonic energy as
    /// less, for the same input — the one shape-independent guarantee this
    /// design makes.
    #[test]
    fn larger_strength_adds_more_energy() {
        let input: Vec<f64> = (0..2000)
            .map(|i| (f64::from(i) * 0.05).sin() * 0.8)
            .collect();
        let coeff = 0.05;
        let low = energy(0.5, coeff, &input);
        let mid = energy(1.5, coeff, &input);
        let high = energy(3.0, coeff, &input);
        assert!(low <= mid + 1e-9, "low={low} mid={mid}");
        assert!(mid <= high + 1e-9, "mid={mid} high={high}");
        assert!(high > 0.0, "expected some harmonic energy at max strength");
    }

    /// Silence in must be silence out, regardless of strength — a
    /// saturator has no harmonics to generate from nothing.
    #[test]
    fn silence_stays_silent() {
        let mut sub_lp = OnePole::default();
        let mut harm_lp = OnePole::default();
        let coeff = 0.1;
        let drive = 5.0;
        for _ in 0..100 {
            let sub = sub_lp.low(0.0, coeff);
            let harmonics = (sub * drive).tanh();
            let harmonics_high = harmonics - harm_lp.low(harmonics, coeff);
            assert!(harmonics_high.abs() < 1e-12);
        }
    }
}
