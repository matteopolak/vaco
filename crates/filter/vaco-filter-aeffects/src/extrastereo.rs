//! `extrastereo` — increase the difference between stereo audio channels.
//!
//! `ffmpeg -h filter=extrastereo` (2026-08-23): `m` (`-10..10`, default `2.5`),
//! `c` (clip, default `true`). No frequency-domain behaviour and no channel
//! count option, so this operates on the first two channels of whatever
//! layout arrives, matching the reference's own stereo-only assumption.
//!
//! # Measured formula (D17)
//!
//! The reference's own documentation does not spell out the arithmetic, so it
//! was measured directly: feed `ffmpeg -af extrastereo=m=2.5:c=false` known
//! `(L, R)` sample pairs and read the output. `(10000, 5000) -> (13750, 1250)`,
//! `(-10000, 10000) -> (-25000, 25000)`, `(1000, -1000) -> (2500, -2500)` and
//! several more all match, exactly, in every case tried:
//!
//! ```text
//! mid  = (L + R) / 2
//! side = (L - R) / 2
//! L' = mid + side * m
//! R' = mid - side * m
//! ```
//!
//! and with `c=true`, `(32000, -32000)` with `m=2.5` measured as
//! `(32767, -32768)` — full-scale clamp of `L'`/`R'`, not of the intermediate
//! `side`. Both are reproduced exactly by [`apply`] and pinned by
//! [`tests::matches_measured_reference_pairs`].

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "extrastereo",
    description: "increase difference between stereo audio channels",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

/// The measured mid/side formula, in the sample domain used everywhere in
/// this crate (`f64`, full-scale `[-1, 1]`).
pub(crate) fn apply(l: f64, r: f64, m: f64, clip: bool) -> (f64, f64) {
    let mid = (l + r) * 0.5;
    let side = (l - r) * 0.5;
    let mut out_l = mid + side * m;
    let mut out_r = mid - side * m;
    if clip {
        out_l = out_l.clamp(-1.0, 1.0);
        out_r = out_r.clamp(-1.0, 1.0);
    }
    (out_l, out_r)
}

struct ExtraStereo {
    m: f64,
    clip: bool,
}

impl FrameFilter for ExtraStereo {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        if channels.len() >= 2 {
            let n = channels
                .first()
                .map_or(0, Vec::len)
                .min(channels.get(1).map_or(0, Vec::len));
            for i in 0..n {
                let l = channels.first().and_then(|c| c.get(i)).copied().unwrap_or(0.0);
                let r = channels.get(1).and_then(|c| c.get(i)).copied().unwrap_or(0.0);
                let (out_l, out_r) = apply(l, r, self.m, self.clip);
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
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let m = common::f64_opt(req, &["m"], 2.5);
    let clip = common::bool_opt(req, &["c"], true);
    let filter = ExtraStereo { m, clip };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter).with_timeline(Timeline::always())),
    }
}

#[cfg(test)]
mod tests {
    use super::apply;

    /// Every `(L, R) -> (L', R')` pair measured directly against
    /// `ffmpeg -af extrastereo=m=2.5:c=false` on 2026-08-23, in the `f64`
    /// domain (measured `i16` values divided by 32768). This is an
    /// independent oracle in the sense the brief requires: the pairs were
    /// read off the reference binary's own output, not derived from this
    /// module's formula.
    #[test]
    fn matches_measured_reference_pairs() {
        let cases: &[((f64, f64), (f64, f64))] = &[
            ((10000.0, 5000.0), (13750.0, 1250.0)),
            ((5000.0, 10000.0), (1250.0, 13750.0)),
            ((-10000.0, 10000.0), (-25000.0, 25000.0)),
            ((0.0, 0.0), (0.0, 0.0)),
            ((20000.0, 20000.0), (20000.0, 20000.0)),
            ((1000.0, -1000.0), (2500.0, -2500.0)),
        ];
        for &((l, r), (exp_l, exp_r)) in cases {
            let (out_l, out_r) = apply(l / 32768.0, r / 32768.0, 2.5, false);
            assert!(
                (out_l * 32768.0 - exp_l).abs() < 1e-6,
                "L: got {}, want {exp_l}",
                out_l * 32768.0
            );
            assert!(
                (out_r * 32768.0 - exp_r).abs() < 1e-6,
                "R: got {}, want {exp_r}",
                out_r * 32768.0
            );
        }
    }

    #[test]
    fn clip_true_saturates_to_full_scale() {
        let (out_l, out_r) = apply(32000.0 / 32768.0, -32000.0 / 32768.0, 2.5, true);
        assert!((out_l - 1.0).abs() < 1e-9);
        assert!((out_r + 1.0).abs() < 1e-9);
    }

    #[test]
    fn m_zero_collapses_to_mono() {
        let (out_l, out_r) = apply(0.7, -0.3, 0.0, false);
        assert!((out_l - out_r).abs() < 1e-12);
        assert!((out_l - 0.2).abs() < 1e-12);
    }
}
