//! `compand` — compress or expand audio dynamic range via a piecewise
//! linear transfer function (the "expander" this crate's epic groups with
//! the compressor/limiter/gate/sidechain family; GitHub #472).
//!
//! `ffmpeg -h filter=compand` (2026-08-23): `attacks`/`decays` (seconds,
//! defaults `"0"`/`"0.8"`, comma-separated per channel — only the first
//! value is read here and applied to every channel, a documented
//! simplification), `points` (`in_db/out_db` pairs, `|`-separated, default
//! `"-70/-70|-60/-20|1/0"`), `soft-knee` (default 0.01, not applied — the
//! transfer curve here is the piecewise-linear interpolation of `points`
//! with no additional knee rounding), `gain` (dB, default 0), `volume`
//! (initial volume in dB, default 0 — read as the envelope's starting
//! level rather than a separate gain stage), `delay` (default 0, not
//! applied: the reference delays the signal so the gain change precedes
//! the level that caused it; this crate applies gain to the same sample
//! that produced the envelope reading instead, a structural gap shared
//! with `alimiter`'s missing lookahead).

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common::{self, db, from_db};
use crate::engine::Envelope;

pub const DESC: FilterDesc = FilterDesc {
    name: "compand",
    description: "compress or expand dynamic range of audio",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, Copy)]
pub(crate) struct Point {
    pub(crate) in_db: f64,
    pub(crate) out_db: f64,
}

impl Point {
    /// Used by [`crate::mcompand`], whose `points` sub-grammar (`,`-separated
    /// within a `|`-separated band) is parsed independently of this module's
    /// [`parse_points`].
    pub(crate) const fn new(in_db: f64, out_db: f64) -> Self {
        Self { in_db, out_db }
    }
}

pub(crate) fn parse_points(raw: &str) -> Vec<Point> {
    let mut pts: Vec<Point> = raw
        .split('|')
        .filter_map(|seg| {
            let (a, b) = seg.trim().split_once('/')?;
            Some(Point {
                in_db: a.trim().parse().ok()?,
                out_db: b.trim().parse().ok()?,
            })
        })
        .collect();
    pts.sort_by(|a, b| a.in_db.total_cmp(&b.in_db));
    pts
}

/// Piecewise-linear interpolation of `points` at `in_db`, flat beyond the
/// endpoints — the standard reading of a transfer-function control-point
/// list, not a probe of the reference's exact curve shape.
pub(crate) fn transfer_db(points: &[Point], in_db: f64) -> f64 {
    let Some(first) = points.first() else {
        return in_db;
    };
    let Some(last) = points.last() else {
        return in_db;
    };
    if in_db <= first.in_db {
        return first.out_db;
    }
    if in_db >= last.in_db {
        return last.out_db;
    }
    for w in points.windows(2) {
        let (Some(a), Some(b)) = (w.first(), w.get(1)) else {
            continue;
        };
        if in_db >= a.in_db && in_db <= b.in_db {
            let span = (b.in_db - a.in_db).max(f64::MIN_POSITIVE);
            let t = (in_db - a.in_db) / span;
            return a.out_db + t * (b.out_db - a.out_db);
        }
    }
    in_db
}

fn first_seconds(raw: &str, default: f64) -> f64 {
    raw.split([',', '|'])
        .next()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .unwrap_or(default)
}

#[derive(Debug, Clone)]
struct Compand {
    points: Vec<Point>,
    attack_ms: f64,
    decay_ms: f64,
    gain_db: f64,
    initial_db: f64,
    sample_rate: f64,
    envelopes: Vec<Envelope>,
}

impl FrameFilter for Compand {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio {
            sample_rate,
            layout,
            ..
        }) = ctx.input_link(0)
        {
            self.sample_rate = f64::from(*sample_rate).max(1.0);
            let n = layout.channels.max(1) as usize;
            self.envelopes = vec![Envelope::default(); n];
            for e in &mut self.envelopes {
                // Jump straight to `volume`'s initial level: `step` with
                // `attack = release = 1.0` sets `level` to the input exactly.
                let _ = e.step(from_db(self.initial_db), 1.0, 1.0);
            }
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        if self.envelopes.len() != channels.len() {
            self.envelopes = vec![Envelope::default(); channels.len()];
        }
        let attack = Envelope::coeff(self.attack_ms, self.sample_rate);
        let decay = Envelope::coeff(self.decay_ms, self.sample_rate);
        for (ch, env) in channels.iter_mut().zip(self.envelopes.iter_mut()) {
            for s in ch.iter_mut() {
                let level = env.step(s.abs(), attack, decay);
                let level_db = db(level);
                let target_db = transfer_db(&self.points, level_db) + self.gain_db;
                let gain = from_db(target_db - level_db);
                *s *= gain;
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
    let points_raw = req
        .named("points")
        .unwrap_or_else(|| "-70/-70|-60/-20|1/0".to_owned());
    let attacks = req.named("attacks").unwrap_or_default();
    let decays = req.named("decays").unwrap_or_else(|| "0.8".to_owned());
    let filter = Compand {
        points: parse_points(&points_raw),
        attack_ms: first_seconds(&attacks, 0.0) * 1000.0,
        decay_ms: first_seconds(&decays, 0.8) * 1000.0,
        gain_db: common::f64_opt(req, &["gain"], 0.0),
        initial_db: common::f64_opt(req, &["volume"], 0.0),
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
    fn identity_points_leave_level_unchanged() {
        // A transfer curve that is the identity everywhere must not change
        // the gain at any level.
        let pts = parse_points("-70/-70|0/0");
        for level_db in [-60.0, -20.0, -1.0, 0.0] {
            assert!((transfer_db(&pts, level_db) - level_db).abs() < 1e-9);
        }
    }

    #[test]
    fn empty_points_string_falls_back_to_identity() {
        let pts = parse_points("");
        assert!((transfer_db(&pts, -12.0) - (-12.0)).abs() < 1e-9);
    }
}
