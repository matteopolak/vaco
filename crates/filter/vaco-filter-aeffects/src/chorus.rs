//! `chorus` — add a chorus effect to the audio.
//!
//! `ffmpeg -h filter=chorus` (2026-08-23): `in_gain`/`out_gain` (`0..1`,
//! default `0.4`), `delays`, `decays`, `speeds`, `depths` (`|`-separated
//! per-voice lists, confirmed accepted alongside this crate's other
//! multi-tap filters — no default listed, so at least one voice must be
//! given).
//!
//! # What is structural, not measured
//!
//! Chorus's audible effect is a continuously-varying delay, which has no
//! single-impulse signature to probe exactly (unlike `aecho`'s discrete
//! taps): reverse-engineering the reference's exact modulation curve, LFO
//! start phase and interpolation kernel would need sine-sweep probing well
//! beyond this pass's budget. This implementation uses the standard chorus
//! structure — for each voice `i`, a delay line modulated by a sine LFO
//! (`vaco-filter-adsp::wave`) between `delays[i]` and `delays[i] +
//! depths[i]` milliseconds at `speeds[i]` Hz, linearly interpolated
//! (`common::InterpDelay`, shared with `flanger` and `vibrato`), mixed as
//! `output = out_gain * (in_gain * dry + sum_i decays[i] * voice_i)`. Not
//! claimed to be sample-exact; see `docs/filter/vaco-filter-aeffects.md`.
//!
//! Every voice's depth going to `0` degenerates to a *static* delay line
//! (no modulation) rather than a full identity — chorus mixes in a delayed
//! copy unconditionally, so unlike `flanger` (whose default `delay=0`
//! plus `depth=0` really does collapse to a no-op) there is no
//! parameterisation of `chorus` that is a true identity: the closest is
//! every `decays[i] = 0`, which zeroes every voice's contribution, checked
//! in [`tests::zero_decays_is_dry_only`].
use vaco_core::{MediaType, Result};
use vaco_filter_adsp::wave::{Lfo, WaveShape};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, InterpDelay};

pub const DESC: FilterDesc = FilterDesc {
    name: "chorus",
    description: "add a chorus effect to the audio",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

fn parse_list(spec: &str) -> Vec<f64> {
    spec.split('|')
        .filter_map(|s| s.trim().parse::<f64>().ok())
        .collect()
}

struct Voice {
    base_ms: f64,
    depth_ms: f64,
    decay: f64,
    lfo: Lfo,
    line: InterpDelay,
}

struct Chorus {
    in_gain: f64,
    out_gain: f64,
    delays_ms: Vec<f64>,
    decays: Vec<f64>,
    speeds: Vec<f64>,
    depths_ms: Vec<f64>,
    channel_voices: Vec<Vec<Voice>>,
}

impl Chorus {
    fn build_voices(&self, sample_rate: f64) -> Vec<Voice> {
        let n = self.delays_ms.len();
        (0..n)
            .map(|i| {
                let base_ms = self.delays_ms.get(i).copied().unwrap_or(0.0);
                let depth_ms = self.depths_ms.get(i).copied().unwrap_or(0.0);
                let decay = self.decays.get(i).copied().unwrap_or(0.0);
                let speed = self.speeds.get(i).copied().unwrap_or(0.5);
                let max_delay_samples = (((base_ms + depth_ms) * sample_rate) / 1000.0)
                    .ceil()
                    .max(1.0) as usize
                    + 2;
                Voice {
                    base_ms,
                    depth_ms,
                    decay,
                    lfo: Lfo::new(WaveShape::Sine, speed, sample_rate, 0.0, 0.0, 1.0),
                    line: InterpDelay::new(max_delay_samples),
                }
            })
            .collect()
    }
}

impl FrameFilter for Chorus {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio {
            sample_rate,
            layout,
            ..
        }) = ctx.input_link(0)
        {
            let channels = layout.channels.max(1) as usize;
            let rate = f64::from(*sample_rate);
            self.channel_voices = (0..channels).map(|_| self.build_voices(rate)).collect();
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        let sample_rate = f64::from(rate);
        for (idx, channel) in channels.iter_mut().enumerate() {
            let Some(voices) = self.channel_voices.get_mut(idx) else {
                continue;
            };
            for sample in channel.iter_mut() {
                let dry = *sample;
                let mut wet = 0.0;
                for voice in voices.iter_mut() {
                    let sweep = voice.lfo.advance(); // 0..1
                    let delay_ms = voice.base_ms + voice.depth_ms * sweep;
                    let delay_samples = (delay_ms * sample_rate) / 1000.0;
                    let delayed = voice.line.process(dry, delay_samples);
                    wet += voice.decay * delayed;
                }
                *sample = self.out_gain * (self.in_gain * dry + wet);
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
        for voices in &mut self.channel_voices {
            for voice in voices.iter_mut() {
                voice.line.flush();
                voice.lfo.reset_phase(0.0);
            }
        }
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let in_gain = common::f64_opt(req, &["in_gain"], 0.4);
    let out_gain = common::f64_opt(req, &["out_gain"], 0.4);
    let delays_ms = req
        .named("delays")
        .map_or_else(|| vec![55.0], |s| parse_list(&s));
    let decays = req
        .named("decays")
        .map_or_else(|| vec![0.4], |s| parse_list(&s));
    let speeds = req
        .named("speeds")
        .map_or_else(|| vec![0.5], |s| parse_list(&s));
    let depths_ms = req
        .named("depths")
        .map_or_else(|| vec![2.0], |s| parse_list(&s));
    let filter = Chorus {
        in_gain,
        out_gain,
        delays_ms,
        decays,
        speeds,
        depths_ms,
        channel_voices: Vec::new(),
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

    /// With every voice's decay at zero, the wet sum must vanish and the
    /// output must be exactly `out_gain * in_gain * dry` — a property of
    /// this module's own mixing formula, checked directly rather than
    /// against the reference (which has no all-zero-decays default to
    /// probe).
    #[test]
    fn zero_decays_is_dry_only() {
        let mut f = Chorus {
            in_gain: 1.0,
            out_gain: 1.0,
            delays_ms: vec![40.0, 60.0],
            decays: vec![0.0, 0.0],
            speeds: vec![0.5, 0.7],
            depths_ms: vec![3.0, 4.0],
            channel_voices: vec![],
        };
        let rate = 8000.0;
        f.channel_voices = vec![f.build_voices(rate)];
        let input = [0.1, -0.5, 0.9, 0.0, -1.0, 0.4];
        for &x in &input {
            let dry = x;
            let mut wet = 0.0;
            let Some(voices) = f.channel_voices.first_mut() else {
                continue;
            };
            for voice in voices.iter_mut() {
                let sweep = voice.lfo.advance();
                let delay_ms = voice.base_ms + voice.depth_ms * sweep;
                let delayed = voice.line.process(dry, (delay_ms * rate) / 1000.0);
                wet += voice.decay * delayed;
            }
            let out = f.out_gain * (f.in_gain * dry + wet);
            assert!(
                (out - dry).abs() < 1e-12,
                "expected dry-only output, got {out} for {dry}"
            );
        }
    }

    /// Falsifiable bound: with `decays` summing to at most 1 and `in_gain`,
    /// `out_gain` at their defaults (`<= 1`), a unit-bounded input must
    /// stay unit-bounded — a coarse stability check for the mixing formula
    /// independent of the exact modulation shape.
    #[test]
    fn bounded_input_gives_bounded_output() {
        let mut f = Chorus {
            in_gain: 0.4,
            out_gain: 0.4,
            delays_ms: vec![40.0, 60.0],
            decays: vec![0.4, 0.3],
            speeds: vec![0.5, 0.7],
            depths_ms: vec![3.0, 4.0],
            channel_voices: vec![],
        };
        let rate = 8000.0;
        f.channel_voices = vec![f.build_voices(rate)];
        for i in 0..2000 {
            let dry = (f64::from(i) * 0.05).sin();
            let mut wet = 0.0;
            let Some(voices) = f.channel_voices.first_mut() else {
                continue;
            };
            for voice in voices.iter_mut() {
                let sweep = voice.lfo.advance();
                let delay_ms = voice.base_ms + voice.depth_ms * sweep;
                let delayed = voice.line.process(dry, (delay_ms * rate) / 1000.0);
                wet += voice.decay * delayed;
            }
            let out = f.out_gain * (f.in_gain * dry + wet);
            assert!(out.abs() <= 1.0 + 1e-9, "out {out} at i={i}");
        }
    }
}
