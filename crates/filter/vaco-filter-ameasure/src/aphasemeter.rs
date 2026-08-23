//! `aphasemeter` — convert input audio to phase meter video output.
//!
//! `ffmpeg -h filter=aphasemeter` (2026-08-23): `rate`/`r` (video rate,
//! default 25), `size`/`s`, `rc`/`gc`/`bc` (meter colours), `mpc` (median
//! phase colour), `video` (default **true**), `phasing` (mono/out-of-phase
//! detection logging, default false), `tolerance`/`t` (0–1, default 0),
//! `angle`/`a` (90–180 degrees, default 170), `duration`/`d` (default 2 s).
//!
//! This crate produces no video output (a documented gap, same shape as
//! `ebur128`'s — see that module's doc); `size`/`rc`/`gc`/`bc`/`mpc`/`video`
//! are accepted and ignored. What is implemented is the measurement
//! `phasing` exists to drive: stereo correlation,
//! `sum(L*R) / sqrt(sum(L^2) * sum(R^2))`, computed over `1/rate`-second
//! blocks (the same granularity the reference draws one video column at),
//! feeding a `silencedetect`-shaped event state machine
//! (`vaco-filter-audio-dynamics::silencedetect` is the model this follows)
//! for `mono_start`/`mono_end` (correlation at or above `1 - tolerance`)
//! and `out_phase_start`/`out_phase_end` (correlation at or below
//! `cos(angle)`, i.e. the angle between channels exceeds the threshold),
//! each requiring `duration` seconds before it is reported.
//!
//! **Oracle.** [`correlation`] is a closed form with exact fixed points
//! checked directly, no filter machinery involved: identical channels
//! correlate at exactly `1.0`, exact opposites at exactly `-1.0`, and
//! orthogonal signals (by construction, not by chance) at exactly `0.0`.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "aphasemeter",
    description: "convert input audio to phase meter video output",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

/// Pearson correlation of two equal-length channels. `0.0` (not `NaN`) when
/// either has zero energy.
fn correlation(l: &[f64], r: &[f64]) -> f64 {
    let n = l.len().min(r.len());
    let mut sum_ll = 0.0;
    let mut sum_rr = 0.0;
    let mut sum_lr = 0.0;
    for i in 0..n {
        let lv = l.get(i).copied().unwrap_or(0.0);
        let rv = r.get(i).copied().unwrap_or(0.0);
        sum_ll += lv * lv;
        sum_rr += rv * rv;
        sum_lr += lv * rv;
    }
    let denom = (sum_ll * sum_rr).sqrt();
    if denom > 1e-15 {
        (sum_lr / denom).clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// A `silencedetect`-shaped "condition held for at least `duration`
/// seconds" event tracker, parameterised over the condition so `mono` and
/// `out_phase` share one implementation instead of two copies of the same
/// state machine.
#[derive(Debug, Clone, Default)]
struct EventTracker {
    active: bool,
    run_seconds: f64,
    start_seconds: f64,
    reported: bool,
}

impl EventTracker {
    fn observe(&mut self, held: bool, t: f64, block_seconds: f64, duration: f64, name: &str) {
        if held {
            if !self.active {
                self.active = true;
                self.run_seconds = 0.0;
                self.start_seconds = t;
                self.reported = false;
            }
            self.run_seconds += block_seconds;
            if !self.reported && self.run_seconds >= duration {
                tracing::info!(
                    target: "vaco_filter_ameasure::aphasemeter",
                    "{name}_start: {:.6}",
                    self.start_seconds,
                );
                self.reported = true;
            }
        } else {
            if self.active && self.reported {
                tracing::info!(
                    target: "vaco_filter_ameasure::aphasemeter",
                    "{name}_end: {t:.6} | {name}_duration: {:.6}",
                    self.run_seconds,
                );
            }
            self.active = false;
            self.run_seconds = 0.0;
        }
    }
}

#[derive(Debug, Clone, Default)]
struct PhaseMeter {
    rate_hz: f64,
    tolerance: f64,
    angle_deg: f64,
    duration_s: f64,
    phasing: bool,
    sample_rate: f64,
    elapsed_seconds: f64,
    mono: EventTracker,
    out_phase: EventTracker,
}

impl FrameFilter for PhaseMeter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio { sample_rate, .. }) = ctx.input_link(0) {
            self.sample_rate = f64::from(*sample_rate).max(1.0);
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        if !self.phasing {
            return Ok(FrameOut::One(input));
        }
        let (_fmt, _rate, _samples, _layout, channels) = crate::sample::decode(&input)?;
        if channels.len() < 2 {
            return Ok(FrameOut::One(input));
        }
        let Some(l) = channels.first() else {
            return Ok(FrameOut::One(input));
        };
        let Some(r) = channels.get(1) else {
            return Ok(FrameOut::One(input));
        };
        let block_len = (self.sample_rate / self.rate_hz.max(1e-6))
            .round()
            .max(1.0) as usize;
        let block_seconds = block_len as f64 / self.sample_rate;
        let angle_cos = (self.angle_deg.to_radians()).cos();
        let mono_threshold = 1.0 - self.tolerance;

        let n = l.len().min(r.len());
        let mut start = 0usize;
        while start < n {
            let end = (start + block_len).min(n);
            let (Some(lw), Some(rw)) = (l.get(start..end), r.get(start..end)) else {
                break;
            };
            let corr = correlation(lw, rw);
            let t = self.elapsed_seconds;
            self.mono
                .observe(corr >= mono_threshold, t, block_seconds, self.duration_s, "mono");
            self.out_phase.observe(
                corr <= angle_cos,
                t,
                block_seconds,
                self.duration_s,
                "out_phase",
            );
            self.elapsed_seconds += block_seconds;
            start = end;
        }
        Ok(FrameOut::One(input))
    }

    fn flush_state(&mut self) {
        self.elapsed_seconds = 0.0;
        self.mono = EventTracker::default();
        self.out_phase = EventTracker::default();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let filter = PhaseMeter {
        rate_hz: common::f64_opt(req, &["rate", "r"], 25.0),
        tolerance: common::f64_opt(req, &["tolerance", "t"], 0.0).clamp(0.0, 1.0),
        angle_deg: common::f64_opt(req, &["angle", "a"], 170.0).clamp(90.0, 180.0),
        duration_s: common::f64_opt(req, &["duration", "d"], 2.0),
        phasing: common::bool_opt(req, &["phasing"], false),
        sample_rate: 48_000.0,
        elapsed_seconds: 0.0,
        mono: EventTracker::default(),
        out_phase: EventTracker::default(),
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter)),
    }
}

#[cfg(test)]
mod tests {
    use super::correlation;

    #[test]
    fn identical_channels_are_fully_correlated() {
        let l = [0.1, -0.4, 0.9, -0.2];
        assert!((correlation(&l, &l) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn exact_opposites_are_fully_anti_correlated() {
        let l = [0.1, -0.4, 0.9, -0.2];
        let r: Vec<f64> = l.iter().map(|v| -v).collect();
        assert!((correlation(&l, &r) - (-1.0)).abs() < 1e-12);
    }

    /// Constructed to be exactly orthogonal, not merely uncorrelated by
    /// chance: `sum(l*r) == 0` by inspection.
    #[test]
    fn orthogonal_channels_are_exactly_zero() {
        let l = [1.0, 0.0, -1.0, 0.0];
        let r = [0.0, 1.0, 0.0, -1.0];
        assert!(correlation(&l, &r).abs() < 1e-15);
    }

    #[test]
    fn silence_is_zero_not_nan() {
        let l = [0.0, 0.0];
        let r = [0.0, 0.0];
        let c = correlation(&l, &r);
        assert!(c.abs() < 1e-15);
        assert!(!c.is_nan());
    }
}
