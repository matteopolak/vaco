//! `earwax` — widen the stereo image.
//!
//! `ffmpeg -h filter=earwax` (2026-08-23) documents no options at all.
//!
//! # A measured FIR, not a recalled one (D17)
//!
//! The reference's own text says nothing about the algorithm, and its source
//! is never consulted (D7) — so the filter was measured directly: feed a
//! single full-scale impulse into the left channel of a 44100 Hz stereo
//! stream and read back what `ffmpeg -af earwax` produces. At 44100 Hz the
//! response is an exact, finite, 32-tap FIR — every output sample past lag 32
//! is exactly zero, and every non-zero sample is an exact integer multiple of
//! `amplitude / 128` (checked at three different impulse amplitudes, `32000`,
//! `16000` and `12800`, all agreeing on the same `/128` fixed-point taps with
//! zero rounding noise). Feeding the impulse into the right channel instead
//! gives the mirror image of the left-channel response (`(L, R) ->
//! (out_L, out_R)` becomes `(out_R, out_L)`), so the filter is a symmetric
//! 2x2 FIR built from just two 32-tap sequences — a "direct" one (own
//! channel) and a "cross" one (opposite channel):
//!
//! ```text
//! outL[n] = sum_{k=1..32} direct[k] * inL[n-k] + cross[k] * inR[n-k]
//! outR[n] = sum_{k=1..32} direct[k] * inR[n-k] + cross[k] * inL[n-k]
//! ```
//!
//! [`tests::matches_measured_impulse_response`] pins the exact measured
//! sequence (impulse in, 40 samples out) against `ffmpeg -af earwax` at
//! 44100 Hz, sample for sample.
//!
//! **What this does not cover:** at other sample rates, the same probe shows
//! a *much* longer, non-causal-looking response (energy appearing before the
//! nominal lag-0 point), which is the signature of the reference resampling
//! internally around a fixed 44100 Hz design rather than recomputing the
//! taps for the real rate — plausible, since the classic "earwax" effect this
//! filter's name and one-line description echo is documented elsewhere (in
//! an unrelated codebase, not consulted here) as 44.1kHz-specific. This
//! implementation applies the measured 44100 Hz taps unconditionally,
//! regardless of the input's actual sample rate — an exact match at
//! 44100 Hz, a structural approximation everywhere else. See
//! `docs/filter/vaco-filter-aeffects.md`.

use std::collections::VecDeque;

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "earwax",
    description: "widen the stereo image",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

/// `direct[k-1]` is the own-channel tap at lag `k`, in units of `1/128`.
/// Measured 2026-08-23 against `ffmpeg -af earwax` at 44100 Hz (see the
/// module doc). `direct[0]` (lag 1) is genuinely zero, not a rounding
/// artifact — the reference's own lag-1 own-channel response measured as
/// exactly zero at three different impulse amplitudes.
const DIRECT: [i32; 32] = [
    0, -5, -11, -6, -12, 9, -2, 22, -10, 7, -18, -5, -6, 0, 23, 7, 4, -3, -29, -7, 1, -5, -3, -1,
    3, 1, 0, 5, 3, -5, -11, -6,
];

/// `cross[k-1]`: the opposite-channel tap at lag `k`, same units and
/// provenance as [`DIRECT`].
const CROSS: [i32; 32] = [
    4, 0, 0, 6, 6, -4, -7, -14, 15, 6, 15, -14, 1, 2, -20, -3, -11, 12, 30, 6, -7, -2, -5, -4, 6,
    9, -5, -2, 3, -1, 4, 4,
];

#[derive(Debug, Clone, Default)]
struct EarWax {
    hist_l: VecDeque<f64>,
    hist_r: VecDeque<f64>,
}

impl EarWax {
    /// One sample step. `hist_l`/`hist_r` hold only *past* samples (lag 1..32)
    /// at the time this runs — the current sample is pushed afterwards — which
    /// is what makes lag 0's contribution exactly zero, matching the measured
    /// impulse response.
    fn step(&mut self, l: f64, r: f64) -> (f64, f64) {
        let mut out_l = 0.0;
        let mut out_r = 0.0;
        for (k, (&hl, &hr)) in self.hist_l.iter().zip(self.hist_r.iter()).enumerate() {
            let Some(&d) = DIRECT.get(k) else { break };
            let Some(&c) = CROSS.get(k) else { break };
            let d = f64::from(d) / 128.0;
            let c = f64::from(c) / 128.0;
            out_l += d * hl + c * hr;
            out_r += d * hr + c * hl;
        }
        self.hist_l.push_front(l);
        self.hist_r.push_front(r);
        self.hist_l.truncate(32);
        self.hist_r.truncate(32);
        (out_l, out_r)
    }
}

impl FrameFilter for EarWax {
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
                let (out_l, out_r) = self.step(l, r);
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
        self.hist_l.clear();
        self.hist_r.clear();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(EarWax::default())),
    }
}

#[cfg(test)]
mod tests {
    use super::EarWax;

    /// Sample-exact against `ffmpeg -af earwax` at 44100 Hz: a full-scale
    /// impulse (`12800/32768`) in the left channel at sample 10 of a
    /// 60-sample buffer, compared against every non-zero output sample
    /// `ffmpeg` produced (measured 2026-08-23, see the module doc for the
    /// exact command). This is the strongest oracle available for this
    /// filter — the reference binary itself, not a re-derivation.
    #[test]
    fn matches_measured_impulse_response() {
        let measured_l: [(i32, f64); 32] = [
            (2, -500.0),
            (3, -1100.0),
            (4, -600.0),
            (5, -1200.0),
            (6, 900.0),
            (7, -200.0),
            (8, 2200.0),
            (9, -1000.0),
            (10, 700.0),
            (11, -1800.0),
            (12, -500.0),
            (13, -600.0),
            (14, 0.0),
            (15, 2300.0),
            (16, 700.0),
            (17, 400.0),
            (18, -300.0),
            (19, -2900.0),
            (20, -700.0),
            (21, 100.0),
            (22, -500.0),
            (23, -300.0),
            (24, -100.0),
            (25, 300.0),
            (26, 100.0),
            (27, 0.0),
            (28, 500.0),
            (29, 300.0),
            (30, -500.0),
            (31, -1100.0),
            (32, -600.0),
            (1, 0.0),
        ];
        let measured_r: [(i32, f64); 32] = [
            (1, 400.0),
            (4, 600.0),
            (5, 600.0),
            (6, -400.0),
            (7, -700.0),
            (8, -1400.0),
            (9, 1500.0),
            (10, 600.0),
            (11, 1500.0),
            (12, -1400.0),
            (13, 100.0),
            (14, 200.0),
            (15, -2000.0),
            (16, -300.0),
            (17, -1100.0),
            (18, 1200.0),
            (19, 3000.0),
            (20, 600.0),
            (21, -700.0),
            (22, -200.0),
            (23, -500.0),
            (24, -400.0),
            (25, 600.0),
            (26, 900.0),
            (27, -500.0),
            (28, -200.0),
            (29, 300.0),
            (30, -100.0),
            (31, 400.0),
            (32, 400.0),
            (2, 0.0),
            (3, 0.0),
        ];

        let amplitude = 12800.0 / 32768.0;
        let mut f = EarWax::default();
        let mut out = Vec::new();
        for n in 0..50 {
            let l = if n == 10 { amplitude } else { 0.0 };
            out.push(f.step(l, 0.0));
        }

        // `expected` is the raw `i16` value `ffmpeg -af earwax` produced at
        // 44100 Hz with a `12800`-amplitude impulse; this implementation runs
        // the same impulse at `12800 / 32768` in its own `[-1, 1]` f64
        // domain, so the two agree at `expected / 32768`.
        for &(lag, expected) in &measured_l {
            let n = 10 + lag as usize;
            let (got_l, _) = out.get(n).copied().unwrap_or((0.0, 0.0));
            let want = expected / 32768.0;
            assert!(
                (got_l - want).abs() < 1e-9,
                "lag {lag}: got {got_l}, want {want}"
            );
        }
        for &(lag, expected) in &measured_r {
            let n = 10 + lag as usize;
            let (_, got_r) = out.get(n).copied().unwrap_or((0.0, 0.0));
            let want = expected / 32768.0;
            assert!(
                (got_r - want).abs() < 1e-9,
                "lag {lag}: got {got_r}, want {want}"
            );
        }
    }

    #[test]
    fn silence_stays_silent() {
        let mut f = EarWax::default();
        for _ in 0..64 {
            let (l, r) = f.step(0.0, 0.0);
            assert!(l.abs() < 1e-12);
            assert!(r.abs() < 1e-12);
        }
    }
}
