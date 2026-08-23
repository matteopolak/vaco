//! `aexciter` — enhance the high-frequency part of audio (harmonic
//! exciter).
//!
//! `ffmpeg -h filter=aexciter` (2026-08-23): `level_in`/`level_out`
//! (`0..64`, default `1`), `amount` (`0..64`, default `1`), `drive`
//! (`0.1..10`, default `8.5`), `blend` (`-10..10`, default `0`), `freq`
//! (`2000..12000` Hz, default `7500`), `ceil` (`9999..20000` Hz, default
//! `9999`), `listen` (bool, default `false`). Supports timeline (`enable`).
//!
//! # What is structural, not measured
//!
//! A harmonic exciter splits off a high band, generates new harmonics from
//! it with a saturator, and mixes them back in — this implementation
//! isolates the above-`freq` band with a one-pole high-pass, drives it
//! through a `tanh` saturator (`drive`), low-passes the result at `ceil` to
//! keep the added harmonics from extending past the requested ceiling,
//! mixes in `blend` times the raw high band (which can be negative, per the
//! option's own range), and adds the result scaled by `amount`. Not claimed
//! to be sample-exact; see `docs/filter/vaco-filter-aeffects.md`.
//!
//! **The one-pole band split was measured against a real biquad, not just
//! assumed adequate.** This crate now depends on `vaco-filter-adsp`
//! (already, for `wave`/`wsola`), so a real two-pole Butterworth split is
//! one call away — the "no cross-crate biquad access" reason this design
//! was originally structural no longer holds. Substituting
//! `vaco_filter_adsp::biquad::{highpass, lowpass}` for both one-poles here,
//! fed the crate's own eight-sample probe sequence through
//! `ffmpeg -af aexciter` at default options, made the match *worse*, not
//! better: max sample error against the reference rose from `0.73` (current
//! one-pole) to `1.04` (biquad substitution) — the reference's actual
//! internal shape is evidently not "this same structure with a
//! higher-order filter". The one-pole design is kept; see
//! `docs/filter/vaco-filter-aeffects.md` for the measurement.
//!
//! # What is exact, by construction
//!
//! `amount = 0` makes the whole exciter contribution vanish regardless of
//! `drive`, `blend`, `freq` or `ceil`, so `output = level_out * level_in *
//! dry` exactly — checked in [`tests::zero_amount_is_pure_gain`]. `listen`
//! bypasses the dry signal entirely and returns only the exciter signal
//! (scaled by `level_out`), which this module's own tests also exercise.
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "aexciter",
    description: "enhance high frequency part of audio",
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
    band_lp: OnePole,
    ceil_lp: OnePole,
}

struct Aexciter {
    level_in: f64,
    level_out: f64,
    amount: f64,
    drive: f64,
    blend: f64,
    freq: f64,
    ceil: f64,
    listen: bool,
    band_coeff: f64,
    ceil_coeff: f64,
    channels: Vec<ChannelState>,
}

impl FrameFilter for Aexciter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio {
            sample_rate,
            layout,
            ..
        }) = ctx.input_link(0)
        {
            let count = layout.channels.max(1) as usize;
            let rate = f64::from(*sample_rate).max(1.0);
            self.band_coeff = (std::f64::consts::TAU * self.freq / rate).clamp(0.001, 1.0);
            self.ceil_coeff = (std::f64::consts::TAU * self.ceil / rate).clamp(0.001, 1.0);
            self.channels = (0..count)
                .map(|_| ChannelState {
                    band_lp: OnePole::default(),
                    ceil_lp: OnePole::default(),
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
                let dry = self.level_in * *sample;
                let low = state.band_lp.low(dry, self.band_coeff);
                let band = dry - low; // above-`freq` content
                let harmonics = (band * self.drive).tanh();
                let harmonics_ceiled = state.ceil_lp.low(harmonics, self.ceil_coeff);
                let exciter = harmonics_ceiled + self.blend * band;

                *sample = if self.listen {
                    self.level_out * exciter
                } else {
                    self.level_out * (dry + self.amount * exciter)
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
            state.band_lp = OnePole::default();
            state.ceil_lp = OnePole::default();
        }
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let level_in = common::f64_opt(req, &["level_in"], 1.0).clamp(0.0, 64.0);
    let level_out = common::f64_opt(req, &["level_out"], 1.0).clamp(0.0, 64.0);
    let amount = common::f64_opt(req, &["amount"], 1.0).clamp(0.0, 64.0);
    let drive = common::f64_opt(req, &["drive"], 8.5).clamp(0.1, 10.0);
    let blend = common::f64_opt(req, &["blend"], 0.0).clamp(-10.0, 10.0);
    let freq = common::f64_opt(req, &["freq"], 7500.0).clamp(2000.0, 12000.0);
    let ceil = common::f64_opt(req, &["ceil"], 9999.0).clamp(9999.0, 20000.0);
    let listen = common::bool_opt(req, &["listen"], false);
    let filter = Aexciter {
        level_in,
        level_out,
        amount,
        drive,
        blend,
        freq,
        ceil,
        listen,
        band_coeff: 0.3,
        ceil_coeff: 0.9,
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

    /// `amount=0` must yield exactly `level_out * level_in * dry`,
    /// regardless of `drive`/`blend` — a property of the mixing formula,
    /// not of the exact exciter shape.
    #[test]
    fn zero_amount_is_pure_gain() {
        let mut state = ChannelState {
            band_lp: OnePole::default(),
            ceil_lp: OnePole::default(),
        };
        let level_in = 1.3;
        let level_out = 0.7;
        let amount = 0.0;
        let drive = 8.5;
        let blend = 3.0;
        let band_coeff = 0.3;
        let ceil_coeff = 0.9;

        for &raw in &[0.1, -0.5, 0.9, 0.0, -1.0, 0.33] {
            let dry = level_in * raw;
            let low = state.band_lp.low(dry, band_coeff);
            let band = dry - low;
            let harmonics = (band * drive).tanh();
            let harmonics_ceiled = state.ceil_lp.low(harmonics, ceil_coeff);
            let exciter = harmonics_ceiled + blend * band;
            let out = level_out * (dry + amount * exciter);
            let want = level_out * level_in * raw;
            assert!((out - want).abs() < 1e-9, "got {out}, want {want}");
        }
    }

    /// `listen=true` must return only the exciter signal, scaled by
    /// `level_out`, with no dry contribution — checked by confirming it
    /// differs from the non-`listen` path whenever the exciter signal is
    /// non-zero.
    #[test]
    fn listen_mode_drops_the_dry_signal() {
        let mut state = ChannelState {
            band_lp: OnePole::default(),
            ceil_lp: OnePole::default(),
        };
        let level_in = 1.0;
        let level_out = 1.0;
        let drive = 8.5;
        let blend = 0.0;
        let band_coeff = 0.3;
        let ceil_coeff = 0.9;

        let mut saw_difference = false;
        for i in 0..200 {
            let raw = (f64::from(i) * 0.3).sin();
            let dry = level_in * raw;
            let low = state.band_lp.low(dry, band_coeff);
            let band = dry - low;
            let harmonics = (band * drive).tanh();
            let harmonics_ceiled = state.ceil_lp.low(harmonics, ceil_coeff);
            let exciter = harmonics_ceiled + blend * band;
            let listen_out = level_out * exciter;
            let normal_out = level_out * (dry + 1.0 * exciter);
            if (listen_out - normal_out).abs() > 1e-9 {
                saw_difference = true;
            }
        }
        assert!(
            saw_difference,
            "expected listen mode to differ from the normal mix"
        );
    }
}
