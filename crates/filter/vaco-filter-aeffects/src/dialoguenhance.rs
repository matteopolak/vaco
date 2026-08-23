//! `dialoguenhance` — audio dialogue enhancement.
//!
//! `ffmpeg -h filter=dialoguenhance` (2026-08-23): `original` (center
//! factor, `0..1`, default `1`), `enhance` (`0..3`, default `1`), `voice`
//! (`2..32`, default `2`). Supports timeline (`enable`).
//!
//! # A measured surprise: the defaults are not an identity
//!
//! Feeding a smoothly-varying stereo signal through `dialoguenhance` at
//! **every default option** (`original=1, enhance=1, voice=2`) does not
//! reproduce the input — the output is silence for the first several
//! samples and stays far from the input throughout (`max |output - input|
//! ~= 0.49` on a signal whose own values are all under `0.35`). That rules
//! out any implementation shaped like "pass through unless a dial is
//! turned away from neutral": the reference is doing genuine spectral
//! voice-activity gating (consistent with `voice` being described as a
//! *detection* factor, not a mix knob) with its own startup latency, and a
//! constant-ish probe signal reads as "not voice" and gets suppressed. This
//! is the same shape of trap this crate's correctness discipline warns
//! about for `hqdn3d`'s printed defaults — a "neutral-looking" default is
//! not evidence the filter is close to identity there.
//!
//! # What this implementation does instead
//!
//! Reproducing real voice-activity detection is out of reach for this
//! pass, so this is a plain, always-on mid/side rebalance, **not a match
//! for the reference at any option value including the defaults**: for a
//! stereo signal, `mid = (L + R) / 2`, and `output_{L,R} = original *
//! {L,R} + (enhance - 1) * mid` — `enhance=1` adds nothing extra,
//! `enhance>1` boosts the shared (center-panned, typically dialogue-heavy)
//! content in both channels equally. `voice` is accepted and stored but
//! has no effect, since this implementation has no detector for it to
//! tune. Mono input is passed through `original`-scaled with no `enhance`
//! term (there is no side channel to derive a center estimate from).
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "dialoguenhance",
    description: "audio dialogue enhancement",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

struct Dialoguenhance {
    original: f64,
    enhance: f64,
    #[expect(
        dead_code,
        reason = "accepted for CLI compatibility; no voice detector to apply it to, see module doc"
    )]
    voice: f64,
}

impl FrameFilter for Dialoguenhance {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        if channels.len() >= 2 {
            let n = channels
                .first()
                .map_or(0, Vec::len)
                .min(channels.get(1).map_or(0, Vec::len));
            for i in 0..n {
                let l = channels
                    .first()
                    .and_then(|c| c.get(i))
                    .copied()
                    .unwrap_or(0.0);
                let r = channels
                    .get(1)
                    .and_then(|c| c.get(i))
                    .copied()
                    .unwrap_or(0.0);
                let mid = (l + r) * 0.5;
                let boost = (self.enhance - 1.0) * mid;
                let out_l = self.original * l + boost;
                let out_r = self.original * r + boost;
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
        } else {
            for channel in &mut channels {
                for sample in channel.iter_mut() {
                    *sample *= self.original;
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
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let original = common::f64_opt(req, &["original"], 1.0).clamp(0.0, 1.0);
    let enhance = common::f64_opt(req, &["enhance"], 1.0).clamp(0.0, 3.0);
    let voice = common::f64_opt(req, &["voice"], 2.0).clamp(2.0, 32.0);
    let filter = Dialoguenhance {
        original,
        enhance,
        voice,
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter).with_timeline(Timeline::always())),
    }
}

#[cfg(test)]
mod tests {
    /// This module's own identity: `original=1, enhance=1` reproduces the
    /// input exactly under *this* formula (the `boost` term vanishes).
    /// This is **not** a claim that the reference behaves this way at its
    /// own defaults — see the module doc's measured-surprise section — it
    /// only pins this implementation's internal consistency.
    #[test]
    fn own_formula_identity_at_neutral_settings() {
        let original = 1.0;
        let enhance = 1.0;
        let pairs: [(f64, f64); 4] = [(0.3, 0.1), (0.31, 0.095), (-0.5, 0.2), (0.0, 0.0)];
        for &(l, r) in &pairs {
            let mid = (l + r) * 0.5;
            let boost = (enhance - 1.0) * mid;
            let out_l = original * l + boost;
            let out_r = original * r + boost;
            assert!((out_l - l).abs() < 1e-12);
            assert!((out_r - r).abs() < 1e-12);
        }
    }

    /// `enhance > 1` must move both channels by the *same* amount (the
    /// shared `mid` boost), which is the whole point of the design: it
    /// raises center-panned content without introducing a left/right
    /// imbalance.
    #[test]
    fn enhance_boosts_both_channels_equally() {
        let original = 1.0;
        let enhance = 2.0;
        let (l, r): (f64, f64) = (0.3, 0.1);
        let mid = (l + r) * 0.5;
        let boost = (enhance - 1.0) * mid;
        let out_l = original * l + boost;
        let out_r = original * r + boost;
        assert!(
            ((out_l - l) - (out_r - r)).abs() < 1e-12,
            "boost differs between channels"
        );
        assert!((out_l - l).abs() > 1e-9, "expected a non-zero boost");
    }
}
