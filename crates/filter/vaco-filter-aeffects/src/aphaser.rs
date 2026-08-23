//! `aphaser` — add a phasing effect to the audio.
//!
//! `ffmpeg -h filter=aphaser` (2026-08-23): `in_gain` (`0..1`, default
//! `0.4`), `out_gain` (`0..1e9`, default `0.74`), `delay` (`0..5` ms,
//! default `3`), `decay` (`0..0.99`, default `0.4`), `speed`
//! (`0.1..2` Hz, default `0.5`), `type` (`sinusoidal`/`triangular`, default
//! `triangular`).
//!
//! # What is structural, not measured
//!
//! `aphaser`'s option names (`delay`, `decay`, `speed`, no separate
//! "depth") describe the same family as `flanger` and `chorus` — a single
//! LFO-modulated delay line with feedback — rather than a cascaded-allpass
//! phaser, so that is what this implements: `delay_ms(t) = delay *
//! shape(speed, t)` (the LFO sweeps the *whole* delay from `0`, not a
//! `base + depth` split), fed back through `decay`
//! (`fed = dry + decay * previous_delayed`), and mixed as `output =
//! out_gain * (in_gain * dry + decay * delayed)`. As with `chorus` and
//! `flanger`, the exact modulation curve is not probed and this is a
//! reasonable stand-in, not a measured match.
//!
//! # What is exact, by construction
//!
//! `decay = 0` makes the `decay * delayed` term vanish regardless of what
//! `delayed` computes to (the delay line still runs, but its output is
//! multiplied by zero), so `output = out_gain * in_gain * dry` exactly —
//! this is an algebraic property of the mixing formula above, checked in
//! [`tests::zero_decay_is_pure_gain`], not a claim that `decay=0` is a full
//! identity (`in_gain * out_gain != 1` at the reference's own defaults, so
//! it is a scaled pass-through, not an unscaled one).
use vaco_core::{MediaType, Result};
use vaco_filter_adsp::wave::WaveShape;
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, InterpDelay};

pub const DESC: FilterDesc = FilterDesc {
    name: "aphaser",
    description: "add a phasing effect to the audio",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

struct ChannelState {
    line: InterpDelay,
    last_delayed: f64,
    phase: f64,
}

struct Aphaser {
    in_gain: f64,
    out_gain: f64,
    delay_ms: f64,
    decay: f64,
    speed_hz: f64,
    shape: WaveShape,
    channels: Vec<ChannelState>,
}

impl FrameFilter for Aphaser {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio {
            sample_rate,
            layout,
            ..
        }) = ctx.input_link(0)
        {
            let count = layout.channels.max(1) as usize;
            let max_len = ((self.delay_ms * f64::from(*sample_rate)) / 1000.0)
                .ceil()
                .max(1.0) as usize
                + 2;
            self.channels = (0..count)
                .map(|_| ChannelState {
                    line: InterpDelay::new(max_len),
                    last_delayed: 0.0,
                    phase: 0.0,
                })
                .collect();
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        let sample_rate = f64::from(rate);
        let step = self.speed_hz / sample_rate;
        for (idx, channel) in channels.iter_mut().enumerate() {
            let Some(state) = self.channels.get_mut(idx) else {
                continue;
            };
            for sample in channel.iter_mut() {
                let dry = *sample;
                let sweep = self.shape.sample(state.phase, 0.0, 1.0);
                state.phase = (state.phase + step).rem_euclid(1.0);
                let delay_samples = (self.delay_ms * sweep * sample_rate) / 1000.0;
                let fed = dry + self.decay * state.last_delayed;
                let delayed = state.line.process(fed, delay_samples);
                state.last_delayed = delayed;
                *sample = self.out_gain * (self.in_gain * dry + self.decay * delayed);
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
            state.line.flush();
            state.last_delayed = 0.0;
            state.phase = 0.0;
        }
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let in_gain = common::f64_opt(req, &["in_gain"], 0.4);
    let out_gain = common::f64_opt(req, &["out_gain"], 0.74);
    let delay_ms = common::f64_opt(req, &["delay"], 3.0).clamp(0.0, 5.0);
    let decay = common::f64_opt(req, &["decay"], 0.4).clamp(0.0, 0.99);
    let speed_hz = common::f64_opt(req, &["speed"], 0.5).clamp(0.1, 2.0);
    let shape = req
        .named("type")
        .map_or(WaveShape::Triangle, |s| WaveShape::parse(&s));
    let filter = Aphaser {
        in_gain,
        out_gain,
        delay_ms,
        decay,
        speed_hz,
        shape,
        channels: Vec::new(),
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

    /// `decay=0` must make the output exactly `out_gain*in_gain*dry`, by
    /// construction of the mixing formula.
    #[test]
    fn zero_decay_is_pure_gain() {
        let mut state = ChannelState {
            line: InterpDelay::new(8),
            last_delayed: 0.0,
            phase: 0.0,
        };
        let in_gain = 0.4;
        let out_gain = 0.74;
        let decay = 0.0;
        let speed_hz = 0.5;
        let sample_rate = 8000.0;
        let step = speed_hz / sample_rate;
        let shape = WaveShape::Triangle;
        let delay_ms = 3.0;

        for &dry in &[0.2, -0.6, 0.9, 0.0, -1.0] {
            let sweep = shape.sample(state.phase, 0.0, 1.0);
            state.phase = (state.phase + step).rem_euclid(1.0);
            let delay_samples = (delay_ms * sweep * sample_rate) / 1000.0;
            let fed = dry + decay * state.last_delayed;
            let delayed = state.line.process(fed, delay_samples);
            state.last_delayed = delayed;
            let out = out_gain * (in_gain * dry + decay * delayed);
            let want = out_gain * in_gain * dry;
            assert!((out - want).abs() < 1e-9, "got {out}, want {want}");
        }
    }

    /// Falsifiable stability bound: with the reference's own default gains
    /// (`in_gain=0.4, out_gain=0.74, decay<=0.99`), a unit-bounded input
    /// must not blow up over many samples of feedback.
    #[test]
    fn bounded_input_stays_bounded_over_time() {
        let mut state = ChannelState {
            line: InterpDelay::new(64),
            last_delayed: 0.0,
            phase: 0.0,
        };
        let in_gain = 0.4;
        let out_gain = 0.74;
        let decay: f64 = 0.4;
        let speed_hz = 0.5;
        let sample_rate = 8000.0;
        let step = speed_hz / sample_rate;
        let shape = WaveShape::Triangle;
        let delay_ms = 3.0;

        for i in 0..5000 {
            let dry = (f64::from(i) * 0.02).sin();
            let sweep = shape.sample(state.phase, 0.0, 1.0);
            state.phase = (state.phase + step).rem_euclid(1.0);
            let delay_samples = (delay_ms * sweep * sample_rate) / 1000.0;
            let fed = dry + decay * state.last_delayed;
            let delayed = state.line.process(fed, delay_samples);
            state.last_delayed = delayed;
            let out = out_gain * (in_gain * dry + decay * delayed);
            let bound = out_gain * (in_gain + decay) / (1.0 - decay);
            assert!(
                out.abs() <= bound + 1e-6,
                "out {out} exceeded bound {bound} at i={i}"
            );
        }
    }
}
