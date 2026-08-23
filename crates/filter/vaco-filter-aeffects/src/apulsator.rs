//! `apulsator` — audio pulsator (periodic per-channel gain modulation).
//!
//! `ffmpeg -h filter=apulsator` (2026-08-23): `level_in`/`level_out`
//! (`0.015625..64`, default `1`), `mode` (`sine`/`triangle`/`square`/
//! `sawup`/`sawdown`, default `sine`), `amount` (`0..1`, default `1`),
//! `offset_l`/`offset_r` (`0..1`, default `0`/`0.5`), `width` (`0..2`,
//! default `1`), `timing` (`bpm`/`ms`/`hz`, default `hz`), `bpm`
//! (`30..300`, default `120`), `ms` (`10..2000`, default `500`), `hz`
//! (`0.01..100`, default `2`).
//!
//! # What was measured (`mode=sine`, `timing=hz`, `width=1`)
//!
//! A constant `1.0` stereo signal through `hz=1:amount=1:offset_l=0:
//! offset_r=0.5` gives, at `sample_rate=1000` (one period = 1000 samples):
//! `L(t) = 0.5 + 0.5*sin(2*pi*t + 0*2*pi)`, `R(t) = 0.5 + 0.5*sin(2*pi*t +
//! 0.5*2*pi)` — sample-exact at seven points across the period (e.g.
//! `t=0.1`: `L=0.7939`, matching `0.5+0.5*sin(0.6283)=0.79389` exactly).
//! Repeating at `amount=0.5` gives `gain = 1 - amount*(1 - base)` exactly
//! (e.g. `base=0` at `t=0.75` gives `gain = 1 - 0.5*(1-0) = 0.5`, matching
//! measured `L(750)=0.5`). See [`tests::matches_measured_sine_default_width`].
//!
//! `offset` is therefore a phase fraction of one full cycle (`0..1`, not
//! degrees or radians), applied identically to both channels' otherwise
//! shared LFO shape.
//!
//! # What is structural, not measured
//!
//! `width != 1` visibly reshapes the curve into something that is not a
//! simple rescaled sine (probed at `width=0.5`: the curve reaches near-peak
//! by `t=0.1` then flattens at the neutral `0.5` for a stretch before moving
//! again, rather than tracing a faster sine) — reverse-engineering its exact
//! shape needs more probing than this pass affords. This implementation
//! instead compresses the active part of the waveform into the first
//! `min(width, 1)` fraction of each half-cycle and holds the neutral value
//! for the remainder, which is monotonic in `width` and identity at
//! `width=1` but is **not** claimed to match the reference bit-for-bit.
//! `timing=bpm`/`ms` (converted to `hz` as `bpm/60` and `1000/ms`
//! respectively) and `mode` values other than `sine` are likewise
//! structural: only `mode=sine, timing=hz` was directly probed.
use vaco_core::{MediaType, Result};
use vaco_filter_adsp::wave::WaveShape;
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "apulsator",
    description: "audio pulsator",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

/// `base(phase)` in `[0, 1]`: the measured `0.5 + 0.5*shape(phase)` shape,
/// with `width` compressing the active region as described in the module
/// doc's structural note (identity reshape at `width=1`).
fn base(mode: WaveShape, phase: f64, width: f64) -> f64 {
    let width = width.clamp(1e-6, 2.0);
    let half = (phase.rem_euclid(1.0)) * 2.0; // which half-cycle, and position within it (0..2)
    let half_index = half.floor();
    let within = half - half_index; // 0..1 within this half-cycle
    let compressed = if width >= 1.0 {
        within
    } else if within < width {
        within / width
    } else {
        1.0 // held at the far end of the half-cycle (neutral once re-centred below)
    };
    let effective_phase = f64::midpoint(half_index, compressed);
    mode.sample(effective_phase, 0.0, 1.0)
}

fn resolve_hz(timing: &str, hz: f64, bpm: f64, ms: f64) -> f64 {
    match timing {
        "bpm" => bpm / 60.0,
        "ms" => {
            if ms > 0.0 {
                1000.0 / ms
            } else {
                hz
            }
        }
        _ => hz,
    }
}

struct Apulsator {
    level_in: f64,
    level_out: f64,
    mode: WaveShape,
    amount: f64,
    offset_l: f64,
    offset_r: f64,
    width: f64,
    freq_hz: f64,
    sample_rate: f64,
    phase: f64,
}

impl Apulsator {
    fn gain_for(&self, offset: f64, t: f64) -> f64 {
        let phase = (self.freq_hz * t + offset).rem_euclid(1.0);
        let b = base(self.mode, phase, self.width);
        1.0 - self.amount * (1.0 - b)
    }
}

impl FrameFilter for Apulsator {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio { sample_rate, .. }) = ctx.input_link(0) {
            self.sample_rate = f64::from(*sample_rate);
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        let sample_rate = if self.sample_rate > 0.0 {
            self.sample_rate
        } else {
            f64::from(rate)
        };
        for (idx, channel) in channels.iter_mut().enumerate() {
            let offset = if idx % 2 == 0 {
                self.offset_l
            } else {
                self.offset_r
            };
            for (n, sample) in channel.iter_mut().enumerate() {
                let t = self.phase + (n as f64) / sample_rate;
                let gain = self.gain_for(offset, t);
                *sample = self.level_out * gain * (self.level_in * *sample);
            }
        }
        let advanced = channels.iter().map(Vec::len).max().unwrap_or(0);
        self.phase += (advanced as f64) / sample_rate;
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
        self.phase = 0.0;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let level_in = common::f64_opt(req, &["level_in"], 1.0);
    let level_out = common::f64_opt(req, &["level_out"], 1.0);
    let mode = req
        .named("mode")
        .map_or(WaveShape::Sine, |s| WaveShape::parse(&s));
    let amount = common::f64_opt(req, &["amount"], 1.0).clamp(0.0, 1.0);
    let offset_l = common::f64_opt(req, &["offset_l"], 0.0).clamp(0.0, 1.0);
    let offset_r = common::f64_opt(req, &["offset_r"], 0.5).clamp(0.0, 1.0);
    let width = common::f64_opt(req, &["width"], 1.0).clamp(0.0, 2.0);
    let timing = req
        .named("timing")
        .map_or_else(|| "hz".to_string(), |s| s.trim().to_string());
    let hz = common::f64_opt(req, &["hz"], 2.0);
    let bpm = common::f64_opt(req, &["bpm"], 120.0);
    let ms = common::f64_opt(req, &["ms"], 500.0);
    let freq_hz = resolve_hz(&timing, hz, bpm, ms);
    let filter = Apulsator {
        level_in,
        level_out,
        mode,
        amount,
        offset_l,
        offset_r,
        width,
        freq_hz,
        sample_rate: 0.0,
        phase: 0.0,
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sample-exact against the measured default-width sine curve for both
    /// channels, at `amount=1` and `amount=0.5`.
    #[test]
    fn matches_measured_sine_default_width() {
        let rate = 1000.0;
        let cases: &[(f64, f64, f64, usize, f64)] = &[
            // (offset, amount, freq, idx, want)
            (0.0, 1.0, 1.0, 0, 0.5),
            (0.0, 1.0, 1.0, 250, 1.0),
            (0.0, 1.0, 1.0, 500, 0.5),
            (0.0, 1.0, 1.0, 750, 0.0),
            (0.5, 1.0, 1.0, 0, 0.5),
            (0.5, 1.0, 1.0, 250, 0.0),
            (0.0, 0.5, 1.0, 0, 0.75),
            (0.0, 0.5, 1.0, 750, 0.5),
        ];
        for &(offset, amount, freq, idx, want) in cases {
            let f = Apulsator {
                level_in: 1.0,
                level_out: 1.0,
                mode: WaveShape::Sine,
                amount,
                offset_l: 0.0,
                offset_r: 0.0,
                width: 1.0,
                freq_hz: freq,
                sample_rate: rate,
                phase: 0.0,
            };
            let t = idx as f64 / rate;
            let got = f.gain_for(offset, t);
            assert!(
                (got - want).abs() < 1e-6,
                "offset={offset} amount={amount} idx={idx}: got {got} want {want}"
            );
        }
    }

    /// `amount=0` must be a perfect identity gain regardless of mode,
    /// offset or width.
    #[test]
    fn zero_amount_is_identity() {
        let f = Apulsator {
            level_in: 1.0,
            level_out: 1.0,
            mode: WaveShape::Square,
            amount: 0.0,
            offset_l: 0.3,
            offset_r: 0.7,
            width: 0.4,
            freq_hz: 3.0,
            sample_rate: 48000.0,
            phase: 0.0,
        };
        for i in 0..2000 {
            let t = f64::from(i) / 48000.0;
            assert!((f.gain_for(f.offset_l, t) - 1.0).abs() < 1e-12);
        }
    }

    /// `width=1` must leave the wave shape untouched (identity reshape).
    #[test]
    fn width_one_is_identity_reshape() {
        for i in 0..1000 {
            let phase = f64::from(i) / 1000.0;
            let direct = WaveShape::Sine.sample(phase, 0.0, 1.0);
            let via_base = base(WaveShape::Sine, phase, 1.0);
            assert!(
                (direct - via_base).abs() < 1e-9,
                "phase={phase}: {direct} vs {via_base}"
            );
        }
    }
}
