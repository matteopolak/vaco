//! `dynaudnorm` — dynamic audio normalizer.
//!
//! `ffmpeg -h filter=dynaudnorm` (2026-08-23): `framelen`/`f` (ms, default
//! 500), `gausssize`/`g` (default 31 — the reference's Gaussian filter
//! size), `peak`/`p` (target linear peak, default 0.95), `maxgain`/`m`
//! (default 10), `targetrms`/`r` (default 0, disabled), `coupling`/`n`
//! (default true), `correctdc`/`c`, `altboundary`/`b`, `compress`/`s`,
//! `threshold`/`t`, `channels`/`h`, `overlap`/`o`, `curve`/`v`.
//!
//! This crate re-blocks audio into `framelen`-sized blocks (via
//! `vaco-filter-core`'s `AudioFilter`/`Blocked` adapter — the framework
//! machinery built exactly for "an FFT-domain-shaped filter needs exactly N
//! samples", which this is not FFT-domain but does need fixed blocks for
//! the same reason), measures each block's peak (or RMS when `targetrms >
//! 0`), and moves a per-block gain toward `min(maxgain, target/measured)`
//! with an exponential moving average standing in for the reference's
//! Gaussian-windowed smoothing across `gausssize` blocks (time constant
//! `2/(gausssize+1)`, the standard EMA-to-window-length correspondence).
//! Applied causally, with no lookahead — the reference's actual Gaussian
//! filter is zero-phase (looks both forward and backward across blocks),
//! so this will lag a fast level change the reference would have already
//! anticipated. `overlap`, `correctdc`, `altboundary`, `compress`,
//! `threshold`, `channels`, `curve` are accepted and not applied.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{AudioFilter, Blocked, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "dynaudnorm",
    description: "dynamic audio normalizer",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone)]
struct DynAudNorm {
    framelen_ms: f64,
    gausssize: f64,
    peak: f64,
    maxgain: f64,
    targetrms: f64,
    coupling: bool,
    block_samples: u32,
    gains: Vec<f64>,
}

impl AudioFilter for DynAudNorm {
    fn frame_size(&self) -> u32 {
        self.block_samples
    }

    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let LinkFormat::Audio {
            sample_rate,
            layout,
            ..
        } = ctx.link(0)
        {
            let samples = (self.framelen_ms / 1000.0 * f64::from(*sample_rate)).round();
            self.block_samples = if samples >= 1.0 && samples.is_finite() {
                samples as u32
            } else {
                1024
            };
            self.gains = vec![1.0; layout.channels.max(1) as usize];
        }
        Ok(())
    }

    fn filter_samples(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        if self.gains.len() != channels.len() {
            self.gains = vec![1.0; channels.len()];
        }
        let alpha = 2.0 / (self.gausssize.max(1.0) + 1.0);
        let measure = |ch: &[f64]| -> f64 {
            if ch.is_empty() {
                return 0.0;
            }
            if self.targetrms > 0.0 {
                (ch.iter().map(|s| s * s).sum::<f64>() / ch.len() as f64).sqrt()
            } else {
                ch.iter().fold(0.0f64, |a, &b| a.max(b.abs()))
            }
        };
        let target = if self.targetrms > 0.0 {
            self.targetrms
        } else {
            self.peak
        };

        if self.coupling {
            let measured = channels.iter().map(|c| measure(c)).fold(0.0f64, f64::max);
            let raw = if measured > 1e-9 {
                (target / measured).min(self.maxgain)
            } else {
                self.maxgain
            };
            let g0 = self.gains.first().copied().unwrap_or(1.0);
            let smoothed = g0 + alpha * (raw - g0);
            for g in &mut self.gains {
                *g = smoothed;
            }
        } else {
            for (ci, ch) in channels.iter().enumerate() {
                let measured = measure(ch);
                let raw = if measured > 1e-9 {
                    (target / measured).min(self.maxgain)
                } else {
                    self.maxgain
                };
                if let Some(g) = self.gains.get_mut(ci) {
                    *g += alpha * (raw - *g);
                }
            }
        }

        for (ci, ch) in channels.iter_mut().enumerate() {
            let g = self.gains.get(ci).copied().unwrap_or(1.0);
            for s in ch.iter_mut() {
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

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let filter = DynAudNorm {
        framelen_ms: common::f64_opt(req, &["framelen", "f"], 500.0),
        gausssize: common::f64_opt(req, &["gausssize", "g"], 31.0),
        peak: common::f64_opt(req, &["peak", "p"], 0.95),
        maxgain: common::f64_opt(req, &["maxgain", "m"], 10.0),
        targetrms: common::f64_opt(req, &["targetrms", "r"], 0.0),
        coupling: common::bool_opt(req, &["coupling", "n"], true),
        block_samples: 1024,
        gains: Vec::new(),
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(Blocked::new(filter)).with_timeline(Timeline::always())),
    }
}
