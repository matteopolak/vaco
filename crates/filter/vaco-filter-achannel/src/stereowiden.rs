//! `stereowiden` — apply a stereo widening effect.
//!
//! `ffmpeg -h filter=stereowiden` (2026-08-23): `delay` (`1..100` ms, default
//! `20`), `feedback` (`0..0.9`, default `0.3`), `crossfeed` (`0..0.8`,
//! default `0.3`), `drymix` (`0..1`, default `0.8`).
//!
//! # Measured formula (D17)
//!
//! An impulse in the left channel only, with `crossfeed=0.5:feedback=0.1:
//! drymix=0.8:delay=20` at 48000 Hz, gives exactly two non-zero output
//! samples: `(25600, -16000)` at lag 0 and `(0, -3200)` at lag 960 (`960 =
//! 20 ms * 48000 Hz / 1000`, exactly). `25600 = 0.8 * 32000` (`drymix`),
//! `16000 = 0.5 * 32000` (`crossfeed`), `3200 = 0.1 * 32000` (`feedback`).
//! Repeating with the impulse in the right channel gives the mirror image
//! (`(-16000, 25600)` at lag 0, `(-3200, 0)` at lag 960). Both are exactly:
//!
//! ```text
//! outL[n] = drymix * L[n] - crossfeed * R[n] - feedback * R[n - d]
//! outR[n] = drymix * R[n] - crossfeed * L[n] - feedback * L[n - d]
//! ```
//!
//! where `d = floor(delay_ms * sample_rate / 1000)`. No further feedback
//! taps appear past lag `d` (checked out to lag 2150), so — despite the
//! option's own name — this is a single-tap cross-delay, not a recirculating
//! feedback loop. [`tests::matches_measured_reference_pairs`] pins the exact
//! measured coefficients.

use std::collections::VecDeque;

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "stereowiden",
    description: "apply stereo widening effect",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

/// The measured formula, independent of sample rate: `delay_samples` is
/// resolved from `delay_ms` once the real rate is known.
pub(crate) fn apply(
    l: f64,
    r: f64,
    delayed_l: f64,
    delayed_r: f64,
    crossfeed: f64,
    feedback: f64,
    drymix: f64,
) -> (f64, f64) {
    let out_l = drymix * l - crossfeed * r - feedback * delayed_r;
    let out_r = drymix * r - crossfeed * l - feedback * delayed_l;
    (out_l, out_r)
}

struct StereoWiden {
    delay_ms: f64,
    feedback: f64,
    crossfeed: f64,
    drymix: f64,
    delay_samples: usize,
    hist_l: VecDeque<f64>,
    hist_r: VecDeque<f64>,
}

impl StereoWiden {
    /// Reset both delay lines, pre-filled with `delay_samples` zeros.
    ///
    /// The pre-fill matters: a queue that merely caps its own length at
    /// `delay_samples` returns whatever it has accumulated so far while
    /// still filling up, which is a *growing*, not fixed, delay for the
    /// first `delay_samples` samples — pre-filling with silence is what
    /// makes `front()` a fixed `delay_samples`-old value from sample zero.
    fn reset_delay_lines(&mut self) {
        self.hist_l.clear();
        self.hist_r.clear();
        self.hist_l.resize(self.delay_samples.max(1), 0.0);
        self.hist_r.resize(self.delay_samples.max(1), 0.0);
    }
}

impl FrameFilter for StereoWiden {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio { sample_rate, .. }) = ctx.input_link(0) {
            self.delay_samples = ((self.delay_ms * f64::from(*sample_rate)) / 1000.0) as usize;
        }
        self.reset_delay_lines();
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
                let l = channels.first().and_then(|c| c.get(i)).copied().unwrap_or(0.0);
                let r = channels.get(1).and_then(|c| c.get(i)).copied().unwrap_or(0.0);
                let delayed_l = self.hist_l.front().copied().unwrap_or(0.0);
                let delayed_r = self.hist_r.front().copied().unwrap_or(0.0);
                let (out_l, out_r) = apply(
                    l,
                    r,
                    delayed_l,
                    delayed_r,
                    self.crossfeed,
                    self.feedback,
                    self.drymix,
                );
                self.hist_l.push_back(l);
                self.hist_r.push_back(r);
                self.hist_l.pop_front();
                self.hist_r.pop_front();
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
        self.reset_delay_lines();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let delay_ms = common::f64_opt(req, &["delay"], 20.0);
    let feedback = common::f64_opt(req, &["feedback"], 0.3);
    let crossfeed = common::f64_opt(req, &["crossfeed"], 0.3);
    let drymix = common::f64_opt(req, &["drymix"], 0.8);
    let filter = StereoWiden {
        delay_ms,
        feedback,
        crossfeed,
        drymix,
        delay_samples: 1,
        hist_l: VecDeque::new(),
        hist_r: VecDeque::new(),
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter).with_timeline(Timeline::always())),
    }
}

#[cfg(test)]
mod tests {
    use super::{StereoWiden, apply};
    use std::collections::VecDeque;

    /// Drives the *actual* delay line (not the pure [`apply`] formula) with a
    /// real impulse, the way `filter_frame` does. This is the test that
    /// would have caught the priming bug: a delay line that merely caps its
    /// own length at `delay_samples` returns the wrong (too-early) value for
    /// every sample before the line first fills, because an unprimed queue's
    /// `front()` is "the oldest sample seen so far", not "the sample from
    /// `delay_samples` ago". [`StereoWiden::reset_delay_lines`] pre-fills
    /// with silence specifically to avoid that.
    #[test]
    fn end_to_end_delay_line_matches_measured_lags() {
        let mut f = StereoWiden {
            delay_ms: 20.0,
            feedback: 0.1,
            crossfeed: 0.5,
            drymix: 0.8,
            delay_samples: 960,
            hist_l: VecDeque::new(),
            hist_r: VecDeque::new(),
        };
        f.reset_delay_lines();

        let amplitude = 32000.0 / 32768.0;
        let mut out = Vec::new();
        for n in 0..1000 {
            let l = if n == 0 { amplitude } else { 0.0 };
            let r = 0.0;
            let delayed_l = f.hist_l.front().copied().unwrap_or(0.0);
            let delayed_r = f.hist_r.front().copied().unwrap_or(0.0);
            let (out_l, out_r) = apply(l, r, delayed_l, delayed_r, f.crossfeed, f.feedback, f.drymix);
            f.hist_l.push_back(l);
            f.hist_r.push_back(r);
            f.hist_l.pop_front();
            f.hist_r.pop_front();
            out.push((out_l, out_r));
        }

        // Every sample before lag 0 and strictly between lag 0 and lag 960
        // must be exactly silent — no premature "leak" from an unprimed
        // delay line.
        for n in 1..960 {
            let (l, r) = out.get(n).copied().unwrap_or((1.0, 1.0));
            assert!(l.abs() < 1e-12, "unexpected leak at n={n}: L={l}");
            assert!(r.abs() < 1e-12, "unexpected leak at n={n}: R={r}");
        }
        let (l0, r0) = out.first().copied().unwrap_or((0.0, 0.0));
        assert!((l0 - 0.8 * amplitude).abs() < 1e-9);
        assert!((r0 - (-0.5 * amplitude)).abs() < 1e-9);
        let (l960, r960) = out.get(960).copied().unwrap_or((0.0, 0.0));
        assert!((l960 - 0.0).abs() < 1e-9);
        assert!((r960 - (-0.1 * amplitude)).abs() < 1e-9);
    }

    /// Measured directly against
    /// `ffmpeg -af stereowiden=crossfeed=0.5:feedback=0.1:drymix=0.8:delay=20`
    /// at 48000 Hz on 2026-08-23: an impulse in either channel alone produces
    /// exactly these two non-zero samples (lag 0 and lag `delay_samples`).
    #[test]
    fn matches_measured_reference_pairs() {
        let amplitude = 32000.0 / 32768.0;
        // Lag 0: no delayed history yet.
        let (out_l, out_r) = apply(amplitude, 0.0, 0.0, 0.0, 0.5, 0.1, 0.8);
        assert!((out_l - 0.8 * amplitude).abs() < 1e-9);
        assert!((out_r - (-0.5 * amplitude)).abs() < 1e-9);

        // Lag == delay_samples: the left impulse is now the delayed sample.
        let (out_l, out_r) = apply(0.0, 0.0, amplitude, 0.0, 0.5, 0.1, 0.8);
        assert!((out_l - 0.0).abs() < 1e-9);
        assert!((out_r - (-0.1 * amplitude)).abs() < 1e-9);

        // Mirror image: impulse in the right channel.
        let (out_l, out_r) = apply(0.0, amplitude, 0.0, 0.0, 0.5, 0.1, 0.8);
        assert!((out_l - (-0.5 * amplitude)).abs() < 1e-9);
        assert!((out_r - 0.8 * amplitude).abs() < 1e-9);
        let (out_l, out_r) = apply(0.0, 0.0, 0.0, amplitude, 0.5, 0.1, 0.8);
        assert!((out_l - (-0.1 * amplitude)).abs() < 1e-9);
        assert!((out_r - 0.0).abs() < 1e-9);
    }

    #[test]
    fn zero_crossfeed_and_feedback_is_pure_drymix() {
        let (out_l, out_r) = apply(0.5, -0.25, 0.1, 0.2, 0.0, 0.0, 0.8);
        assert!((out_l - 0.4).abs() < 1e-9);
        assert!((out_r - (-0.2)).abs() < 1e-9);
    }
}
