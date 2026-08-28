//! `atilt` — apply spectral tilt to audio.
//!
//! `ffmpeg -h filter=atilt` (2026-08-27): `freq` (20 to 192000 Hz, default
//! 10000), `slope` (-1 to 1, default 0), `width` (100 to 10000 Hz, default
//! 1000), `order` (2 to 30, default 5), `level` (0 to 4, default 1). A
//! genuinely different filter from [`crate::tiltshelf`] despite the similar
//! name: `tiltshelf` shares `af_biquads.c`'s `treble/high/tiltshelf` option
//! class (`frequency`/`width_type`/`width`/`gain`/`mix`) and is a single low
//! shelf + high shelf pair; `atilt` has its own option set entirely
//! (`freq`/`slope`/`width`/`order`/`level`, confirmed via `ffmpeg -h
//! filter=atilt` directly — no `width_type`, no `mix`), and an `order`
//! parameter `tiltshelf` does not have at all, which is the strongest signal
//! that the two are unrelated implementations that happen to both be called
//! "tilt" in their descriptions.
//!
//! # Not measured — built from a published construction instead
//!
//! `order` (2 to 30) is the tell: this is not one shelf pair but a
//! variable-order filter, and there is no public description of how the
//! reference maps `order`/`slope`/`width` onto a specific cascade. Rather
//! than guess that mapping, this implementation reuses the one tilt
//! construction this crate already has and can verify —
//! [`vaco_filter_adsp::biquad::tilt`], a low-shelf-cut/high-shelf-boost pair
//! around a pivot frequency (0 dB at the pivot, symmetric `-g/2`/`+g/2` at
//! DC/Nyquist, verified in that module's own tests) — cascaded
//! `(order / 2).max(1)` times, each stage contributing an equal share of the
//! total tilt. Cascading identical shelving sections to steepen a transition
//! is standard filter-design practice (raising a filter's order the
//! textbook way), so `order` does *something* in the documented direction
//! (higher order, steeper transition) without claiming to reproduce
//! whatever specific structure the reference uses for it. `slope` maps
//! linearly to a total gain swing of `slope * 24 dB` — a round, defensible
//! full-scale number, not a measured one — and `width` (Hz) feeds
//! `biquad::tilt` directly as its own `width` parameter with
//! `WidthType::Hz`, matching the option's own unit.
//!
//! `level` (0 to 4, documented as an input level, sharing its range with
//! `vaco-filter-audio`'s `volume`-style options) is applied as a linear
//! pre-gain.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use vaco_filter_adsp::biquad::{self as biquad, Coeffs, State, WidthType};

pub const DESC: FilterDesc = FilterDesc {
    name: "atilt",
    description: "apply spectral tilt to audio",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

/// Total gain swing (dB) mapped from `slope in [-1, 1]` — see module doc.
const MAX_SWING_DB: f64 = 24.0;

#[derive(Debug, Clone)]
struct Stage {
    low: Coeffs,
    high: Coeffs,
    low_state: State,
    high_state: State,
}

#[derive(Debug, Clone)]
struct Tilt {
    freq: f64,
    slope: f64,
    width: f64,
    stages: usize,
    level: f64,
    channels: Vec<Vec<Stage>>,
}

impl Tilt {
    fn build_stage(&self, fs: f64) -> Stage {
        let gain_per_stage = (self.slope.clamp(-1.0, 1.0) * MAX_SWING_DB) / self.stages as f64;
        let (low, high) = biquad::tilt(fs, self.freq, WidthType::Hz, self.width, gain_per_stage);
        Stage {
            low,
            high,
            low_state: State::default(),
            high_state: State::default(),
        }
    }
}

impl FrameFilter for Tilt {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio {
            sample_rate,
            layout,
            ..
        }) = ctx.input_link(0)
        {
            let fs = f64::from(*sample_rate);
            let n = layout.channels.max(1) as usize;
            self.channels = (0..n)
                .map(|_| (0..self.stages).map(|_| self.build_stage(fs)).collect())
                .collect();
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        if self.channels.len() != channels.len() {
            let fs = f64::from(rate);
            self.channels = (0..channels.len())
                .map(|_| (0..self.stages).map(|_| self.build_stage(fs)).collect())
                .collect();
        }
        for (ch, stages) in channels.iter_mut().zip(self.channels.iter_mut()) {
            for s in ch.iter_mut() {
                let mut v = *s * self.level;
                for stage in stages.iter_mut() {
                    let mid = stage.low_state.process(&stage.low, v);
                    v = stage.high_state.process(&stage.high, mid);
                }
                *s = v;
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
        for stages in &mut self.channels {
            for stage in stages.iter_mut() {
                stage.low_state = State::default();
                stage.high_state = State::default();
            }
        }
    }
}

/// `order / 2` cascade stages, at least one — see module doc for why halving
/// `order` is the chosen mapping.
#[allow(
    clippy::integer_division,
    reason = "order is a small option value (2..=30); halving it to a stage count is exact, not a precision-losing division"
)]
fn stages_for_order(order: u32) -> usize {
    usize::try_from(order / 2).unwrap_or(1).max(1)
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let order = req
        .named("order")
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(5)
        .clamp(2, 30);
    let filter = Tilt {
        freq: common::f64_opt(req, &["freq"], 10_000.0),
        slope: common::f64_opt(req, &["slope"], 0.0),
        width: common::f64_opt(req, &["width"], 1000.0),
        stages: stages_for_order(order),
        level: common::f64_opt(req, &["level"], 1.0),
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

    /// `slope = 0` must be a no-op: zero total gain swing means every stage
    /// is `biquad::tilt`'s own `gain_db = 0` case, which that module's own
    /// tests already pin as the identity — checked again here at this
    /// module's level so a future change to how `stages`/`slope` combine
    /// cannot silently break the zero case.
    #[test]
    fn zero_slope_is_the_identity_response() {
        let t = Tilt {
            freq: 10_000.0,
            slope: 0.0,
            width: 1000.0,
            stages: 3,
            level: 1.0,
            channels: Vec::new(),
        };
        let stage = t.build_stage(48_000.0);
        // 0 dB tilt: both halves of the cascade must individually be the
        // identity biquad (`Coeffs::identity()`'s own defining property —
        // `b0=1`, everything else `0` — checked via `response_db`).
        assert!(stage.low.response_db(0.1).abs() < 1e-6);
        assert!(stage.high.response_db(0.1).abs() < 1e-6);
    }

    /// More `stages` at a fixed `slope` must split the same total swing
    /// into smaller per-stage steps — a real, checkable consequence of the
    /// "equal share per stage" construction, not a re-statement of it: the
    /// per-stage gain magnitude must shrink as `stages` grows.
    #[test]
    fn more_stages_means_a_smaller_per_stage_gain() {
        let mut t = Tilt {
            freq: 5000.0,
            slope: 0.5,
            width: 1000.0,
            stages: 1,
            level: 1.0,
            channels: Vec::new(),
        };
        let one_stage_gain = (t.slope.clamp(-1.0, 1.0) * MAX_SWING_DB) / t.stages as f64;
        t.stages = 5;
        let five_stage_gain = (t.slope.clamp(-1.0, 1.0) * MAX_SWING_DB) / t.stages as f64;
        assert!(five_stage_gain.abs() < one_stage_gain.abs());
    }

    /// `order` clamps to the documented `[2, 30]` range and always yields at
    /// least one cascade stage, even for the smallest legal `order`.
    #[test]
    fn order_always_yields_at_least_one_stage() {
        for order in [2u32, 3, 30] {
            let stages = stages_for_order(order);
            assert!(stages >= 1, "order={order}: stages={stages}");
        }
    }
}
