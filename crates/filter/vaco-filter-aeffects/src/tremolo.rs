//! `tremolo` — apply a tremolo (amplitude modulation) effect.
//!
//! `ffmpeg -h filter=tremolo` (2026-08-23): `f` (frequency Hz, `0.1..20000`,
//! default `5`), `d` (depth, `0..1`, default `0.5`). Supports timeline
//! (`enable`).
//!
//! # What was measured
//!
//! Feeding a constant `1.0` signal at `f=1` Hz, `sample_rate=1000` Hz (so
//! one LFO period is exactly 1000 samples) gives a gain curve whose peak
//! (`1.0`) sits at sample `0` and whose trough (`1 - d`, matching `d=0.2,
//! 0.5, 1.0` exactly) sits at sample `500` — half a period later. Sampling
//! seven points across the period against every candidate of the shape
//! `1 - d/2 * (1 - cos(2*pi*f*t))` matches to `f32` rounding at every one
//! (index `100`: predicted `0.95225425`, measured `0.95225424`; index `333`:
//! predicted `0.62545372`, measured `0.62545371`). [`tests::matches_measured_gain_curve`]
//! pins this. `d=0` is the identity (`gain(t) == 1` for every `t`), which
//! falls straight out of the formula.
use vaco_core::{MediaType, Result};
use vaco_filter_adsp::wave::{Lfo, WaveShape};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "tremolo",
    description: "apply tremolo effect",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

struct Tremolo {
    freq: f64,
    depth: f64,
    lfo: Option<Lfo>,
}

impl FrameFilter for Tremolo {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio { sample_rate, .. }) = ctx.input_link(0) {
            // Phase 0.25 turns the sine table into a cosine, matching the
            // measured `1 - d/2*(1 - cos(wt))` shape (peak at t=0).
            self.lfo = Some(Lfo::new(
                WaveShape::Sine,
                self.freq,
                f64::from(*sample_rate),
                0.25,
                1.0 - self.depth,
                1.0,
            ));
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        let Some(lfo) = &mut self.lfo else {
            let mut out = crate::sample::encode(
                &vaco_frame::FramePool::default(),
                fmt,
                layout,
                rate,
                &channels,
            )?;
            out.pts = input.pts;
            return Ok(FrameOut::One(out));
        };
        let frame_len = channels.iter().map(Vec::len).max().unwrap_or(0);
        let gains: Vec<f64> = (0..frame_len).map(|_| lfo.advance()).collect();
        for channel in &mut channels {
            for (sample, &gain) in channel.iter_mut().zip(&gains) {
                *sample *= gain;
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
        if let Some(lfo) = &mut self.lfo {
            lfo.reset_phase(0.25);
        }
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let freq = common::f64_opt(req, &["f"], 5.0).clamp(0.1, 20000.0);
    let depth = common::f64_opt(req, &["d"], 0.5).clamp(0.0, 1.0);
    let filter = Tremolo {
        freq,
        depth,
        lfo: None,
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

    fn gain(depth: f64, freq: f64, t: f64) -> f64 {
        1.0 - depth / 2.0 * (1.0 - (2.0 * std::f64::consts::PI * freq * t).cos())
    }

    /// Sample-exact against the measured curve (see module doc) at several
    /// points across one period, for three depths.
    #[test]
    fn matches_measured_gain_curve() {
        let rate = 1000.0;
        for &d in &[0.2, 0.5, 1.0] {
            for &idx in &[0usize, 100, 250, 333, 500, 750, 900] {
                let t = idx as f64 / rate;
                let mut lfo = Lfo::new(WaveShape::Sine, 1.0, rate, 0.25, 1.0 - d, 1.0);
                for _ in 0..idx {
                    lfo.advance();
                }
                let got = lfo.peek();
                let want = gain(d, 1.0, t);
                assert!(
                    (got - want).abs() < 1e-6,
                    "d={d} idx={idx}: got {got} want {want}"
                );
            }
        }
    }

    /// `d=0` is an exact identity gain at every point in the cycle.
    #[test]
    fn zero_depth_is_identity() {
        let mut lfo = Lfo::new(WaveShape::Sine, 5.0, 48000.0, 0.25, 1.0, 1.0);
        for _ in 0..1000 {
            assert!((lfo.advance() - 1.0).abs() < 1e-12);
        }
    }

    /// Falsified and restored: without the `0.25` phase offset, the LFO
    /// would start at its midpoint rather than its peak, breaking the
    /// measured "peak at t=0" property.
    #[test]
    fn phase_offset_is_load_bearing() {
        let without_offset = Lfo::new(WaveShape::Sine, 1.0, 1000.0, 0.0, 0.5, 1.0);
        assert!(
            (without_offset.peek() - 0.75).abs() < 1e-9,
            "sanity: sine at phase 0 is midpoint"
        );
        let with_offset = Lfo::new(WaveShape::Sine, 1.0, 1000.0, 0.25, 0.5, 1.0);
        assert!(
            (with_offset.peek() - 1.0).abs() < 1e-9,
            "expected peak at t=0 with the 0.25 offset"
        );
    }
}
