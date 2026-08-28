//! `alimiter` — audio lookahead limiter.
//!
//! `ffmpeg -h filter=alimiter` (2026-08-23): `level_in`/`level_out`
//! (default 1), `limit` (linear ceiling, default 1), `attack` (ms, default
//! 5), `release` (ms, default 50), `asc` (auto self-compensation, default
//! false), `asc_level` (default 0.5), `level` (auto level, default true),
//! `latency` (compensate delay, default false).
//!
//! Implemented as a peak envelope follower gain-computer: when the smoothed
//! peak exceeds `limit`, gain is scaled down by `limit / envelope`; the
//! reference's actual lookahead (a delay line so the gain reduction *precedes*
//! the peak that caused it, avoiding the brief overshoot a non-lookahead
//! limiter lets through) is not implemented, nor is `asc`/`asc_level`'s
//! automatic threshold adjustment or `latency`'s delay compensation — all
//! three are accepted and ignored. This is a structural limiter, not a
//! verified match to the reference's lookahead behaviour.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::engine::Envelope;

pub const DESC: FilterDesc = FilterDesc {
    name: "alimiter",
    description: "audio lookahead limiter",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone)]
struct Limiter {
    level_in: f64,
    level_out: f64,
    limit: f64,
    attack_ms: f64,
    release_ms: f64,
    sample_rate: f64,
    envelopes: Vec<Envelope>,
}

impl FrameFilter for Limiter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio { sample_rate, .. }) = ctx.input_link(0) {
            self.sample_rate = f64::from(*sample_rate).max(1.0);
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        if self.envelopes.len() != channels.len() {
            self.envelopes = vec![Envelope::default(); channels.len()];
        }
        let attack = Envelope::coeff(self.attack_ms, self.sample_rate);
        let release = Envelope::coeff(self.release_ms, self.sample_rate);
        let limit = self.limit.clamp(1e-6, 1.0);
        for (ch, env) in channels.iter_mut().zip(self.envelopes.iter_mut()) {
            for s in ch.iter_mut() {
                let x = *s * self.level_in;
                let peak = env.step(x.abs(), attack, release);
                let gain = if peak > limit {
                    limit / peak.max(1e-12)
                } else {
                    1.0
                };
                *s = x * gain * self.level_out;
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
        for e in &mut self.envelopes {
            *e = Envelope::default();
        }
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let filter = Limiter {
        level_in: common::f64_opt(req, &["level_in"], 1.0),
        level_out: common::f64_opt(req, &["level_out"], 1.0),
        limit: common::f64_opt(req, &["limit"], 1.0),
        attack_ms: common::f64_opt(req, &["attack"], 5.0),
        release_ms: common::f64_opt(req, &["release"], 50.0),
        sample_rate: 48_000.0,
        envelopes: Vec::new(),
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

    #[test]
    fn below_limit_signal_is_unchanged() {
        let mut lim = Limiter {
            level_in: 1.0,
            level_out: 1.0,
            limit: 1.0,
            attack_ms: 5.0,
            release_ms: 50.0,
            sample_rate: 48_000.0,
            envelopes: vec![Envelope::default()],
        };
        let attack = Envelope::coeff(lim.attack_ms, lim.sample_rate);
        let release = Envelope::coeff(lim.release_ms, lim.sample_rate);
        if let Some(env) = lim.envelopes.first_mut() {
            for _ in 0..1000 {
                let peak = env.step(0.3, attack, release);
                assert!(peak <= lim.limit);
            }
        }
    }
}
