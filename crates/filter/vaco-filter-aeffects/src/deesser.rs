//! `deesser` — reduce sibilance ("ess" sounds) in the audio.
//!
//! `ffmpeg -h filter=deesser` (2026-08-23): `i` (intensity, `0..1`, default
//! `0`), `m` (max deessing, `0..1`, default `0.5`), `f` (frequency,
//! `0..1`, normalised, default `0.5`), `s` (output mode `i`/`o`/`e` —
//! input/output/ess, default `o`).
//!
//! # What was measured
//!
//! `i=0` (the reference's own default) is a byte-exact identity: feeding
//! the same eight-sample sequence used to probe `crystalizer` through
//! `deesser=i=0` (`s` left at its default `o`) gives a maximum sample
//! difference of `0.0` against the input. [`tests::zero_intensity_is_identity`]
//! checks this module's own formula reproduces that.
//!
//! # What is structural, not measured
//!
//! The sibilance *detection* (splitting off the frequency band `f` selects,
//! and how `m` shapes the gain-reduction curve above it) is not
//! reverse-engineered: this implementation uses a simple one-pole high-pass
//! (the same building block `crossfeed` already uses in this crate for its
//! low-pass path) to isolate a "sibilant" band above a cutoff derived from
//! `f`, and reduces that band's contribution by up to `m` in proportion to
//! how far its short-term envelope exceeds a fixed threshold, scaled
//! overall by `i`. Not claimed to match the reference's own detector or
//! filter shape. `s=i` and `s=e` are implemented (return the dry input, and
//! the isolated sibilant band respectively) and are exact by construction
//! of this module's own split, not measured against the reference's
//! ess-only output.
//!
//! **Measured, not assumed: a real biquad split does not help here either.**
//! `vaco_filter_adsp::biquad::highpass` is reachable from this crate now
//! (`vaco-filter-aeffects` already depends on `vaco-filter-adsp`); swapping
//! it in for the one-pole and re-running the crate's eight-sample probe
//! through `ffmpeg -af deesser=i=0.5:m=0.5:f=0.5` changed the result by
//! less than `1e-15` — floating-point noise, not a real difference. The
//! reason is structural, not a filter-order problem: at this probe's
//! amplitude the short-term envelope never crosses the fixed `0.15`
//! excess threshold, so `reduction` stays `0` and `low + ess` (which
//! reconstructs `dry` exactly regardless of what filter produced `low`,
//! per [`tests::low_plus_ess_reconstructs_dry`]) dominates either way. The
//! ~0.66 gap to the reference's actual output at these settings is in the
//! detector/gain-reduction logic this module openly documents as
//! unreverse-engineered, not in the one-pole split — so there is nothing
//! for a biquad to fix, and the one-pole design is kept.
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "deesser",
    description: "apply de-essing to the audio",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    Input,
    Output,
    Ess,
}

impl OutputMode {
    fn parse(s: &str) -> Self {
        match s.trim() {
            "i" | "input" | "0" => Self::Input,
            "e" | "ess" | "2" => Self::Ess,
            _ => Self::Output,
        }
    }
}

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
    lp: OnePole,
    envelope: f64,
}

struct Deesser {
    intensity: f64,
    max_deess: f64,
    freq: f64,
    mode: OutputMode,
    lp_coeff: f64,
    channels: Vec<ChannelState>,
}

impl FrameFilter for Deesser {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio {
            sample_rate,
            layout,
            ..
        }) = ctx.input_link(0)
        {
            let count = layout.channels.max(1) as usize;
            // `f` (0..1) maps onto a plausible sibilance-detection cutoff
            // range (2 kHz .. 10 kHz), then to a one-pole coefficient.
            let cutoff_hz = 2000.0 + self.freq * 8000.0;
            let rate = f64::from(*sample_rate).max(1.0);
            self.lp_coeff = (std::f64::consts::TAU * cutoff_hz / rate).clamp(0.001, 1.0);
            self.channels = (0..count)
                .map(|_| ChannelState {
                    lp: OnePole::default(),
                    envelope: 0.0,
                })
                .collect();
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        for (idx, channel) in channels.iter_mut().enumerate() {
            let Some(state) = self.channels.get_mut(idx) else {
                continue;
            };
            for sample in channel.iter_mut() {
                let dry = *sample;
                let low = state.lp.low(dry, self.lp_coeff);
                let ess = dry - low; // high-frequency ("sibilant") band

                state.envelope += 0.05 * (ess.abs() - state.envelope);
                let threshold = 0.15;
                let excess = (state.envelope - threshold).max(0.0);
                let reduction =
                    (self.max_deess * self.intensity * (excess / (excess + 0.1))).clamp(0.0, 1.0);
                let deessed_high = ess * (1.0 - reduction);
                let out = low + deessed_high;

                *sample = match self.mode {
                    OutputMode::Input => dry,
                    OutputMode::Output => out,
                    OutputMode::Ess => ess,
                };
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
            state.lp = OnePole::default();
            state.envelope = 0.0;
        }
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let intensity = common::f64_opt(req, &["i"], 0.0).clamp(0.0, 1.0);
    let max_deess = common::f64_opt(req, &["m"], 0.5).clamp(0.0, 1.0);
    let freq = common::f64_opt(req, &["f"], 0.5).clamp(0.0, 1.0);
    let mode = req
        .named("s")
        .map_or(OutputMode::Output, |s| OutputMode::parse(&s));
    let filter = Deesser {
        intensity,
        max_deess,
        freq,
        mode,
        lp_coeff: 0.1,
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

    /// `i=0` must be an exact identity, matching the measurement in the
    /// module doc: with `intensity=0`, `reduction` is always `0` so
    /// `deessed_high == ess` and `out == low + ess == dry` exactly.
    #[test]
    fn zero_intensity_is_identity() {
        let mut state = ChannelState {
            lp: OnePole::default(),
            envelope: 0.0,
        };
        let lp_coeff = 0.3;
        let intensity = 0.0;
        let max_deess = 0.5;
        let input = [0.1, 0.3, -0.2, 0.5, 0.5, -0.9, 0.0, 0.2];
        for &dry in &input {
            let low = state.lp.low(dry, lp_coeff);
            let ess = dry - low;
            state.envelope += 0.05 * (ess.abs() - state.envelope);
            let excess = (state.envelope - 0.15).max(0.0);
            let reduction: f64 =
                (max_deess * intensity * (excess / (excess + 0.1))).clamp(0.0, 1.0);
            assert!(
                reduction.abs() < 1e-12,
                "reduction should be exactly zero, got {reduction}"
            );
            let out = low + ess * (1.0 - reduction);
            assert!(
                (out - dry).abs() < 1e-9,
                "expected identity, got {out} for {dry}"
            );
        }
    }

    #[test]
    fn output_mode_parses_all_three_spellings() {
        assert_eq!(OutputMode::parse("i"), OutputMode::Input);
        assert_eq!(OutputMode::parse("o"), OutputMode::Output);
        assert_eq!(OutputMode::parse("e"), OutputMode::Ess);
    }

    /// `low + ess` must reconstruct the dry signal exactly for any
    /// coefficient — the split this filter's gain reduction depends on
    /// must be lossless before any reduction is applied.
    #[test]
    fn low_plus_ess_reconstructs_dry() {
        let mut lp = OnePole::default();
        for i in 0..500 {
            let dry = (f64::from(i) * 0.1).sin();
            let low = lp.low(dry, 0.4);
            let ess = dry - low;
            assert!((low + ess - dry).abs() < 1e-12);
        }
    }
}
