//! `crystalizer` — simple audio noise sharpening filter.
//!
//! `ffmpeg -h filter=crystalizer` (2026-08-23): `i` (intensity, `-10..10`,
//! default `2`), `c` (clipping, default `true`). Supports timeline
//! (`enable`).
//!
//! # What was measured
//!
//! `crystalizer=i=2:c=false` on `[0.1, 0.3, -0.2, 0.5, 0.5, -0.9, 0.0,
//! 0.2]` (per-sample, previous state starting at `0`) gives exactly
//! `[0.3, 0.7, -1.2, 1.9, 0.5, -3.7, 1.8, 0.6]` — matching
//! **`output[n] = input[n] + i * (input[n] - input[n-1])`**, `input[-1] =
//! 0`, at every one of the eight samples (including the exact-zero-delta
//! case `n=4`, where `input[4] == input[3]` so `output[4] == input[4]`
//! unchanged, and the sign-flip case `n=6`, where `input[6] = 0.0` still
//! produces a large positive output because the *previous* sample was
//! `-0.9`). With `c=true` (the default), the same computation is clamped to
//! `[-1, 1]` — the same eight samples come back as `[0.3, 0.7, -1.0, 1.0,
//! 0.5, -1.0, 1.0, 0.6]`. [`tests::matches_measured_sequence`] pins both.
//!
//! `i=0` is an exact identity, falling straight out of the formula
//! (`output[n] = input[n] + 0 = input[n]`).
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "crystalizer",
    description: "simple audio noise sharpening filter",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

struct Crystalizer {
    intensity: f64,
    clip: bool,
    prev: Vec<f64>,
}

impl FrameFilter for Crystalizer {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        if self.prev.len() < channels.len() {
            self.prev.resize(channels.len(), 0.0);
        }
        for (idx, channel) in channels.iter_mut().enumerate() {
            let Some(prev_slot) = self.prev.get_mut(idx) else {
                continue;
            };
            for sample in channel.iter_mut() {
                let x = *sample;
                let mut y = x + self.intensity * (x - *prev_slot);
                if self.clip {
                    y = y.clamp(-1.0, 1.0);
                }
                *prev_slot = x;
                *sample = y;
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
        for p in &mut self.prev {
            *p = 0.0;
        }
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let intensity = common::f64_opt(req, &["i"], 2.0).clamp(-10.0, 10.0);
    let clip = common::bool_opt(req, &["c"], true);
    let filter = Crystalizer {
        intensity,
        clip,
        prev: Vec::new(),
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter).with_timeline(Timeline::always())),
    }
}

#[cfg(test)]
mod tests {
    fn run(input: &[f64], intensity: f64, clip: bool) -> Vec<f64> {
        let mut prev = 0.0;
        input
            .iter()
            .map(|&x| {
                let mut y = x + intensity * (x - prev);
                if clip {
                    y = y.clamp(-1.0, 1.0);
                }
                prev = x;
                y
            })
            .collect()
    }

    /// Sample-exact against the measured sequence, both with and without
    /// clipping.
    #[test]
    fn matches_measured_sequence() {
        let input = [0.1, 0.3, -0.2, 0.5, 0.5, -0.9, 0.0, 0.2];
        let unclipped = run(&input, 2.0, false);
        let want_unclipped = [0.3, 0.7, -1.2, 1.9, 0.5, -3.7, 1.8, 0.6];
        for (got, want) in unclipped.iter().zip(&want_unclipped) {
            assert!((got - want).abs() < 1e-5, "got {got}, want {want}");
        }

        let clipped = run(&input, 2.0, true);
        let want_clipped = [0.3, 0.7, -1.0, 1.0, 0.5, -1.0, 1.0, 0.6];
        for (got, want) in clipped.iter().zip(&want_clipped) {
            assert!((got - want).abs() < 1e-5, "got {got}, want {want}");
        }
    }

    /// `i=0` is an exact identity for any input.
    #[test]
    fn zero_intensity_is_identity() {
        let input = [0.1, -0.5, 0.9, 0.0, -1.0, 0.33];
        let out = run(&input, 0.0, true);
        for (a, b) in out.iter().zip(&input) {
            assert!((a - b).abs() < 1e-12);
        }
    }

    /// A sample equal to its predecessor is always passed through unchanged
    /// (the delta term vanishes), whatever the intensity.
    #[test]
    fn equal_consecutive_samples_are_unchanged() {
        for &intensity in &[-5.0, 0.0, 3.0, 10.0] {
            let out = run(&[0.5, 0.5], intensity, false);
            assert!(
                (out.get(1).copied().unwrap_or(0.0) - 0.5).abs() < 1e-12,
                "intensity={intensity}"
            );
        }
    }
}
