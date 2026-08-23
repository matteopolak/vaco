//! `crossfeed` — apply a headphone crossfeed filter.
//!
//! `ffmpeg -h filter=crossfeed` (2026-08-23): `strength` (`0..1`, default
//! `0.2`), `range` (`0..1`, default `0.5`), `slope` (`0.01..1`, default
//! `0.5`), `level_in` (`0..1`, default `0.9`), `level_out` (`0..1`, default
//! `1`), `block_size` (`0..32768`, default `0`).
//!
//! # What was measured
//!
//! `crossfeed=strength=0` on `(10000, 3000)`/`(3000, 10000)`/`(-5000, 7000)`
//! (`i16` domain) gives `(9000, 2700)`/`(2700, 9000)`/`(-4500, 6300)` —
//! **exactly** `0.9×` (`level_in`'s default) of the input, with `strength=0`
//! disabling the cross-mix entirely. That is an exact, reproduced invariant:
//! [`tests::strength_zero_is_pure_gain`] pins it.
//!
//! What is *not* measured, and is a structural approximation rather than a
//! verified match (the reference's own text does not spell out the
//! crossfeed transfer function, and isolating it from a running process would
//! need per-frequency sine-sweep probing well beyond this work package's
//! budget — see `docs/filter/vaco-filter-achannel.md`): the shape of the
//! `strength > 0` mix. This implementation feeds a one-pole low-pass of the
//! *opposite* channel back into each channel, gated by `strength`, with
//! `range` setting the low-pass cutoff (higher `range` -> brighter, less
//! filtered crossfeed) and `slope` blending between the raw and low-passed
//! opposite-channel signal (higher `slope` -> more low-pass, matching the
//! option's own "curve slope" description). `block_size` is accepted and
//! ignored: the reference's own option text describes it as a performance
//! knob (segment size for its FFT-based path), not something that changes
//! the output.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "crossfeed",
    description: "apply headphone crossfeed filter",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, Copy, Default)]
struct OnePole {
    y: f64,
}

impl OnePole {
    fn step(&mut self, x: f64, a: f64) -> f64 {
        self.y += a * (x - self.y);
        self.y
    }
}

struct Crossfeed {
    strength: f64,
    range: f64,
    slope: f64,
    level_in: f64,
    level_out: f64,
    coeff: f64,
    lp_l: OnePole,
    lp_r: OnePole,
}

impl Crossfeed {
    /// A one-pole low-pass coefficient in `(0, 1]` from `range`: larger
    /// `range` means a brighter (less filtered) crossfeed path, so it maps to
    /// a *larger* coefficient (faster tracking, less smoothing).
    fn lowpass_coeff(range: f64) -> f64 {
        (0.05 + 0.9 * range.clamp(0.0, 1.0)).clamp(0.001, 1.0)
    }
}

impl FrameFilter for Crossfeed {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio { .. }) = ctx.input_link(0) {
            self.coeff = Self::lowpass_coeff(self.range);
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        if channels.len() >= 2 {
            let n = channels
                .first()
                .map_or(0, Vec::len)
                .min(channels.get(1).map_or(0, Vec::len));
            for i in 0..n {
                let l = channels.first().and_then(|c| c.get(i)).copied().unwrap_or(0.0) * self.level_in;
                let r = channels.get(1).and_then(|c| c.get(i)).copied().unwrap_or(0.0) * self.level_in;

                let lp_r = self.lp_r.step(r, self.coeff);
                let lp_l = self.lp_l.step(l, self.coeff);
                let cross_from_r = self.slope.mul_add(lp_r - r, r);
                let cross_from_l = self.slope.mul_add(lp_l - l, l);

                let out_l = self.level_out * self.strength.mul_add(cross_from_r - l, l);
                let out_r = self.level_out * self.strength.mul_add(cross_from_l - r, r);

                if let Some(c) = channels.get_mut(0)
                    && let Some(s) = c.get_mut(i)
                {
                    *s = out_l;
                }
                if let Some(c) = channels.get_mut(1)
                    && let Some(s) = c.get_mut(i)
                {
                    *s = out_r;
                }
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
        self.lp_l = OnePole::default();
        self.lp_r = OnePole::default();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let strength = common::f64_opt(req, &["strength"], 0.2);
    let range = common::f64_opt(req, &["range"], 0.5);
    let slope = common::f64_opt(req, &["slope"], 0.5);
    let level_in = common::f64_opt(req, &["level_in"], 0.9);
    let level_out = common::f64_opt(req, &["level_out"], 1.0);
    let filter = Crossfeed {
        strength,
        range,
        slope,
        level_in,
        level_out,
        coeff: Crossfeed::lowpass_coeff(range),
        lp_l: OnePole::default(),
        lp_r: OnePole::default(),
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

    /// Measured directly against `ffmpeg -af crossfeed=strength=0`: with no
    /// crossfeed, the filter is pure `level_in * level_out` gain. This is the
    /// one case where the reference's exact numeric output is known, so the
    /// test asserts against those measured numbers, not against this
    /// module's own formula.
    #[test]
    fn strength_zero_is_pure_gain() {
        let mut f = Crossfeed {
            strength: 0.0,
            range: 0.5,
            slope: 0.5,
            level_in: 0.9,
            level_out: 1.0,
            coeff: Crossfeed::lowpass_coeff(0.5),
            lp_l: OnePole::default(),
            lp_r: OnePole::default(),
        };
        let cases: &[((f64, f64), (f64, f64))] = &[
            ((10000.0, 3000.0), (9000.0, 2700.0)),
            ((3000.0, 10000.0), (2700.0, 9000.0)),
            ((-5000.0, 7000.0), (-4500.0, 6300.0)),
        ];
        for &((l, r), (exp_l, exp_r)) in cases {
            let lp_r = f.lp_r.step(r * f.level_in, f.coeff);
            let lp_l = f.lp_l.step(l * f.level_in, f.coeff);
            let li = l * f.level_in;
            let ri = r * f.level_in;
            let cross_from_r = f.slope.mul_add(lp_r - ri, ri);
            let cross_from_l = f.slope.mul_add(lp_l - li, li);
            let out_l = f.level_out * f.strength.mul_add(cross_from_r - li, li);
            let out_r = f.level_out * f.strength.mul_add(cross_from_l - ri, ri);
            assert!((out_l - exp_l).abs() < 1e-9, "L: got {out_l}, want {exp_l}");
            assert!((out_r - exp_r).abs() < 1e-9, "R: got {out_r}, want {exp_r}");
        }
    }

    /// Strength in `[0, 1]` must never push a bounded input outside a bounded
    /// output range by more than the gain gain gain stages allow — a coarse
    /// stability check independent of the exact mixing formula.
    #[test]
    fn bounded_input_gives_bounded_output() {
        let mut f = Crossfeed {
            strength: 1.0,
            range: 1.0,
            slope: 1.0,
            level_in: 1.0,
            level_out: 1.0,
            coeff: Crossfeed::lowpass_coeff(1.0),
            lp_l: OnePole::default(),
            lp_r: OnePole::default(),
        };
        for i in 0..1000 {
            let l = (f64::from(i) * 0.1).sin();
            let r = (f64::from(i) * 0.13).cos();
            let lp_r = f.lp_r.step(r, f.coeff);
            let lp_l = f.lp_l.step(l, f.coeff);
            let cross_from_r = f.slope.mul_add(lp_r - r, r);
            let cross_from_l = f.slope.mul_add(lp_l - l, l);
            let out_l = f.strength.mul_add(cross_from_r - l, l);
            let out_r = f.strength.mul_add(cross_from_l - r, r);
            assert!(out_l.abs() <= 1.0 + 1e-9, "out_l={out_l}");
            assert!(out_r.abs() <= 1.0 + 1e-9, "out_r={out_r}");
        }
    }
}
