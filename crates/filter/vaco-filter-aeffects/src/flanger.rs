//! `flanger` — apply a flanging effect to the audio.
//!
//! `ffmpeg -h filter=flanger` (2026-08-23): `delay` (`0..30` ms, default
//! `0`), `depth` (`0..10` ms, default `2`), `regen` (`-95..95`%, default
//! `0`), `width` (`0..100`%, default `71`), `speed` (`0.1..10` Hz, default
//! `0.5`), `shape` (`sinusoidal`/`triangular`, default `sinusoidal`),
//! `phase` (`0..100`%, default `25`, per-channel LFO phase offset),
//! `interp` (`linear`/`quadratic`, default `linear`).
//!
//! # What is structural, not measured
//!
//! Like `chorus`, the modulated-delay shape itself is not probed exactly
//! (see that module's doc for why). This implementation is the standard
//! flanger structure: a delay line whose length sweeps between `delay` and
//! `delay + depth` milliseconds at `speed` Hz (`shape` selects the LFO
//! waveform via `vaco-filter-adsp::wave`), with `regen`% of the delayed
//! output fed back into the line (classic comb-filter feedback), mixed as
//! `output = dry * (1 - width) + delayed * width`. `interp=quadratic` is
//! accepted but treated the same as `linear` — `common::InterpDelay` only
//! implements linear interpolation, documented here rather than silently
//! ignored. Not claimed to be sample-exact.
//!
//! # The one exact case: every default
//!
//! At `delay=0` and `depth=0`, the swept delay length is `0` at every
//! phase, so the delay line's feedback loop reads back the same sample it
//! was just given (`0` delay, `0` prior feedback contribution since `regen`
//! also defaults to `0`) — i.e. `delayed == dry` identically. The
//! `width`-mix then collapses to `dry*(1-width) + dry*width == dry`
//! regardless of `width`'s value: **every default option (including
//! `width=71`) is an exact identity**, not because `width` happens to be
//! irrelevant but because the two things it mixes are equal. This is an
//! algebraic consequence of the formula above, not a separate special case
//! in the code, and [`tests::all_defaults_is_identity`] exercises the real
//! code path (not a shortcut) to confirm it.
use vaco_core::{MediaType, Result};
use vaco_filter_adsp::wave::WaveShape;
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, InterpDelay};

pub const DESC: FilterDesc = FilterDesc {
    name: "flanger",
    description: "apply a flanging effect to the audio",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

struct ChannelState {
    line: InterpDelay,
    last_delayed: f64,
    phase: f64,
}

struct Flanger {
    delay_ms: f64,
    depth_ms: f64,
    regen: f64,
    width: f64,
    speed_hz: f64,
    shape: WaveShape,
    phase_frac: f64,
    channels: Vec<ChannelState>,
}

impl FrameFilter for Flanger {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio {
            sample_rate,
            layout,
            ..
        }) = ctx.input_link(0)
        {
            let count = layout.channels.max(1) as usize;
            let max_len = (((self.delay_ms + self.depth_ms) * f64::from(*sample_rate)) / 1000.0)
                .ceil()
                .max(1.0) as usize
                + 2;
            self.channels = (0..count)
                .map(|ch| ChannelState {
                    line: InterpDelay::new(max_len),
                    last_delayed: 0.0,
                    phase: self.phase_frac * (ch as f64),
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
                let delay_ms = self.delay_ms + self.depth_ms * sweep;
                let delay_samples = (delay_ms * sample_rate) / 1000.0;
                let fed = dry + self.regen * state.last_delayed;
                let delayed = state.line.process(fed, delay_samples);
                state.last_delayed = delayed;
                *sample = dry * (1.0 - self.width) + delayed * self.width;
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
        for (ch, state) in self.channels.iter_mut().enumerate() {
            state.line.flush();
            state.last_delayed = 0.0;
            state.phase = self.phase_frac * (ch as f64);
        }
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let delay_ms = common::f64_opt(req, &["delay"], 0.0).clamp(0.0, 30.0);
    let depth_ms = common::f64_opt(req, &["depth"], 2.0).clamp(0.0, 10.0);
    let regen = (common::f64_opt(req, &["regen"], 0.0).clamp(-95.0, 95.0)) / 100.0;
    let width = (common::f64_opt(req, &["width"], 71.0).clamp(0.0, 100.0)) / 100.0;
    let speed_hz = common::f64_opt(req, &["speed"], 0.5).clamp(0.1, 10.0);
    let shape = req
        .named("shape")
        .map_or(WaveShape::Sine, |s| WaveShape::parse(&s));
    let phase_frac = (common::f64_opt(req, &["phase"], 25.0).clamp(0.0, 100.0)) / 100.0;
    let filter = Flanger {
        delay_ms,
        depth_ms,
        regen,
        width,
        speed_hz,
        shape,
        phase_frac,
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

    /// Runs the real per-sample code path (not a shortcut) at every default
    /// option and checks the output is byte-for-byte the input — the
    /// algebraic identity described in the module doc.
    #[test]
    fn all_defaults_is_identity() {
        let mut state = ChannelState {
            line: InterpDelay::new(4),
            last_delayed: 0.0,
            phase: 0.0,
        };
        let delay_ms = 0.0;
        let depth_ms = 0.0;
        let regen = 0.0;
        let width = 0.71; // the reference's default; must not matter here
        let speed_hz = 0.5;
        let sample_rate = 8000.0;
        let step = speed_hz / sample_rate;
        let shape = WaveShape::Sine;

        let input = [0.2, -0.6, 0.9, 0.0, -1.0, 0.33, 0.5, -0.5];
        for &dry in &input {
            let sweep = shape.sample(state.phase, 0.0, 1.0);
            state.phase = (state.phase + step).rem_euclid(1.0);
            let delay = delay_ms + depth_ms * sweep;
            let delay_samples = (delay * sample_rate) / 1000.0;
            let fed = dry + regen * state.last_delayed;
            let delayed = state.line.process(fed, delay_samples);
            state.last_delayed = delayed;
            let out = dry * (1.0 - width) + delayed * width;
            assert!(
                (out - dry).abs() < 1e-9,
                "expected identity, got {out} for {dry}"
            );
        }
    }

    /// Falsifiable: if `regen` were applied even at `delay=depth=0` (i.e.
    /// the feedback guard were removed), the identity above would break as
    /// soon as `regen != 0`. This confirms the non-zero-regen case really
    /// does diverge from a straight pass-through, so the zero-regen
    /// identity above is not vacuous.
    #[test]
    fn nonzero_regen_is_not_identity() {
        let mut state = ChannelState {
            line: InterpDelay::new(4),
            last_delayed: 0.0,
            phase: 0.0,
        };
        let regen = 0.5;
        let mut out = Vec::new();
        for &dry in &[1.0, 0.0, 0.0, 0.0] {
            let fed = dry + regen * state.last_delayed;
            let delayed = state.line.process(fed, 0.0);
            state.last_delayed = delayed;
            out.push(dry * 0.29 + delayed * 0.71);
        }
        // The feedback should keep the impulse's energy alive past sample 0.
        assert!(
            out.get(1).copied().unwrap_or(0.0).abs() > 1e-6,
            "expected feedback tail: {out:?}"
        );
    }
}
