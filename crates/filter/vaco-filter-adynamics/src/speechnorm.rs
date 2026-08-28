//! `speechnorm` — speech normalizer.
//!
//! `ffmpeg -h filter=speechnorm` (2026-08-23): `peak`/`p` (target linear
//! peak, default 0.95), `expansion`/`e` (max gain multiplier, default 2),
//! `compression`/`c` (max attenuation multiplier, default 2), `threshold`/
//! `t` (linear, default 0 — disabled), `raise`/`r` and `fall`/`f` (gain
//! change per sample, default 0.001 each), `channels`/`h`, `invert`/`i`,
//! `link`/`l`, `rms`/`m`. This crate implements the core adaptive-gain loop
//! (peak envelope tracked per channel, gain adapted toward `peak / envelope`
//! at `raise`/`fall` rates, clamped to `[1/compression, expansion]`) but not
//! `threshold`'s gating, `invert`, `channels` selection, or `rms` (RMS
//! targeting instead of peak) — `link` (all channels share one gain) is
//! implemented since it is the simpler of the two cases to get right, and
//! per-channel independent gain (the default, `link=false`) is what
//! actually runs unless `link=true` is set.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "speechnorm",
    description: "speech normalizer",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone)]
struct SpeechNorm {
    peak: f64,
    expansion: f64,
    compression: f64,
    raise: f64,
    fall: f64,
    link: bool,
    gains: Vec<f64>,
}

impl FrameFilter for SpeechNorm {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let LinkFormat::Audio { layout, .. } = ctx.link(0) {
            self.gains = vec![1.0; layout.channels.max(1) as usize];
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        if self.gains.len() != channels.len() {
            self.gains = vec![1.0; channels.len()];
        }
        let low = (1.0 / self.compression.max(1e-6)).max(1e-6);
        let high = self.expansion.max(low);
        for (ci, ch) in channels.iter_mut().enumerate() {
            for s in ch.iter_mut() {
                let mag = s.abs();
                let target = if mag > 1e-9 {
                    (self.peak / mag).clamp(low, high)
                } else {
                    high
                };
                if self.link {
                    let g = self.gains.first().copied().unwrap_or(1.0);
                    let ng = step_toward(g, target, self.raise, self.fall);
                    for slot in &mut self.gains {
                        *slot = ng;
                    }
                } else if let Some(g) = self.gains.get_mut(ci) {
                    *g = step_toward(*g, target, self.raise, self.fall);
                }
                let g = self.gains.get(ci).copied().unwrap_or(1.0);
                *s *= g;
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
        for g in &mut self.gains {
            *g = 1.0;
        }
    }
}

fn step_toward(current: f64, target: f64, raise: f64, fall: f64) -> f64 {
    if target > current {
        (current + raise.max(0.0)).min(target)
    } else {
        (current - fall.max(0.0)).max(target)
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let filter = SpeechNorm {
        peak: common::f64_opt(req, &["peak", "p"], 0.95),
        expansion: common::f64_opt(req, &["expansion", "e"], 2.0),
        compression: common::f64_opt(req, &["compression", "c"], 2.0),
        raise: common::f64_opt(req, &["raise", "r"], 0.001),
        fall: common::f64_opt(req, &["fall", "f"], 0.001),
        link: common::bool_opt(req, &["link", "l"], false),
        gains: Vec::new(),
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
    fn gain_never_leaves_the_compression_expansion_bounds() {
        let low = 1.0 / 2.0;
        let high = 2.0;
        let mut g = 1.0;
        for target in [0.1f64, 5.0, 0.4, 3.0] {
            let clamped = target.clamp(low, high);
            g = step_toward(g, clamped, 0.5, 0.5);
            assert!((low..=high).contains(&g));
        }
    }
}
