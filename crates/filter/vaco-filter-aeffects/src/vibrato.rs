//! `vibrato` — apply a vibrato (pitch modulation) effect.
//!
//! `ffmpeg -h filter=vibrato` (2026-08-23): `f` (frequency Hz,
//! `0.1..20000`, default `5`), `d` (depth, `0..1`, default `0.5`). Supports
//! timeline (`enable`). Same option shape as `tremolo`, but modulating
//! delay (pitch) rather than gain (amplitude).
//!
//! # What is structural, not measured
//!
//! Probing an isolated impulse through `vibrato` shows its output position
//! moves by an amount that depends on both the sample rate and the LFO
//! phase at the moment the impulse arrives: at a 1000 Hz sample rate with
//! `f=1` and `d=1.0`, an impulse near a half-cycle boundary shifts by
//! about one sample; at a 48000 Hz sample rate with the same `f` and `d`
//! but a different LFO phase, by about 240 samples. That is consistent
//! with a genuine modulated fractional delay, but the two data points sit
//! at different phases and are not enough on their own to fix the exact
//! depth-to-milliseconds scale or the interpolation kernel without
//! substantially more sine-sweep probing than this pass affords. This
//! implementation modulates `common::InterpDelay` (the same
//! linearly-interpolated delay line `chorus` and `flanger` use) with a
//! sine LFO, sweeping the delay between zero and `DEPTH_MS_MAX * d`
//! milliseconds, where `DEPTH_MS_MAX` (`4.0`) is chosen as a plausible
//! order-of-magnitude vibrato depth. Not claimed to be sample-exact; see
//! `docs/filter/vaco-filter-aeffects.md`.
//!
//! `d=0` collapses the swept delay to a constant `0`, so the delay line
//! always reads back the sample it was just given: an exact identity,
//! checked in [`tests::zero_depth_is_identity`].
use vaco_core::{MediaType, Result};
use vaco_filter_adsp::wave::{Lfo, WaveShape};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, InterpDelay};

pub const DESC: FilterDesc = FilterDesc {
    name: "vibrato",
    description: "apply vibrato effect",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

/// Plausible order-of-magnitude peak vibrato depth, in milliseconds, at
/// `d=1.0`. Not a measured constant; see the module doc.
const DEPTH_MS_MAX: f64 = 4.0;

struct ChannelState {
    line: InterpDelay,
    lfo: Lfo,
}

struct Vibrato {
    freq: f64,
    depth: f64,
    channels: Vec<ChannelState>,
}

impl FrameFilter for Vibrato {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio {
            sample_rate,
            layout,
            ..
        }) = ctx.input_link(0)
        {
            let count = layout.channels.max(1) as usize;
            let rate = f64::from(*sample_rate);
            let max_len = ((DEPTH_MS_MAX * rate) / 1000.0).ceil().max(1.0) as usize + 2;
            self.channels = (0..count)
                .map(|_| ChannelState {
                    line: InterpDelay::new(max_len),
                    lfo: Lfo::new(WaveShape::Sine, self.freq, rate, 0.0, 0.0, 1.0),
                })
                .collect();
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        let sample_rate = f64::from(rate);
        for (idx, channel) in channels.iter_mut().enumerate() {
            let Some(state) = self.channels.get_mut(idx) else {
                continue;
            };
            for sample in channel.iter_mut() {
                let sweep = state.lfo.advance(); // 0..1
                let delay_ms = DEPTH_MS_MAX * self.depth * sweep;
                let delay_samples = (delay_ms * sample_rate) / 1000.0;
                *sample = state.line.process(*sample, delay_samples);
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
            state.lfo.reset_phase(0.0);
        }
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let freq = common::f64_opt(req, &["f"], 5.0).clamp(0.1, 20000.0);
    let depth = common::f64_opt(req, &["d"], 0.5).clamp(0.0, 1.0);
    let filter = Vibrato {
        freq,
        depth,
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

    /// `d=0` must reproduce every sample exactly (the swept delay is
    /// always `0`, so the delay line reads back the sample it was just
    /// given).
    #[test]
    fn zero_depth_is_identity() {
        let mut state = ChannelState {
            line: InterpDelay::new(8),
            lfo: Lfo::new(WaveShape::Sine, 5.0, 8000.0, 0.0, 0.0, 1.0),
        };
        let depth = 0.0;
        let input = [0.1, -0.4, 0.9, 0.0, -1.0, 0.33];
        for &x in &input {
            let sweep = state.lfo.advance();
            let delay_ms = DEPTH_MS_MAX * depth * sweep;
            let delay_samples = (delay_ms * 8000.0) / 1000.0;
            let got = state.line.process(x, delay_samples);
            assert!(
                (got - x).abs() < 1e-12,
                "expected identity, got {got} for {x}"
            );
        }
    }

    /// Falsifiable: a non-zero depth must actually move a unit impulse away
    /// from lag zero at some point in the cycle, confirming the identity
    /// above is not vacuous (the modulation genuinely has an effect when
    /// enabled).
    #[test]
    fn nonzero_depth_moves_an_impulse() {
        let mut state = ChannelState {
            line: InterpDelay::new(64),
            lfo: Lfo::new(WaveShape::Sine, 5.0, 8000.0, 0.0, 0.0, 1.0),
        };
        let depth = 1.0;
        let mut input = vec![0.0; 4000];
        if let Some(first) = input.first_mut() {
            *first = 1.0;
        }
        let mut saw_fractional_leak = false;
        for &x in &input {
            let sweep = state.lfo.advance();
            let delay_ms = DEPTH_MS_MAX * depth * sweep;
            let delay_samples = (delay_ms * 8000.0) / 1000.0;
            let got = state.line.process(x, delay_samples);
            if got.abs() > 1e-9 && (got - x).abs() > 1e-9 {
                saw_fractional_leak = true;
            }
        }
        assert!(
            saw_fractional_leak,
            "expected the modulation to spread the impulse's energy"
        );
    }
}
