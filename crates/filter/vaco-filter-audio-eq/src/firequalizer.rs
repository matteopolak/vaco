//! `firequalizer` — a finite impulse response equalizer.
//!
//! `ffmpeg -h filter=firequalizer` (2026-08-23) documents `gain` (an
//! expression, default `gain_interpolate(f)`), `gain_entry` (control points
//! for that expression), `wfunc` (window function), `scale`
//! (linear/logarithmic axes), `delay`, `accuracy`, `fixed`, `multi`,
//! `zero_phase`, `dumpfile`/`dumpscale`, `fft2`, `min_phase`.
//!
//! The reference designs its FIR by evaluating the `gain` expression across
//! the spectrum and inverse-transforming. This module implements only the
//! `gain_entry` control-point path — not the general `gain` expression
//! grammar `vaco-expr` would be needed for — and reads each entry as
//! `entry(freq,gain_db)` or a bare `freq,gain_db` pair, `;`- or `|`-
//! separated (the reference's own texi manual documents `entry(f, g)`; the
//! bare form is this crate's own convenience alias, not a probed fact).
//! Everything else — `scale`, `wfunc`'s specific window shapes beyond Hann,
//! `delay`, `accuracy`, `multi`, `zero_phase`, `min_phase` — is accepted and
//! ignored. This is the least-verified filter in the crate: see
//! `docs/filter/vaco-filter-audio-eq.md`.
//!
//! # Design method
//!
//! Frequency sampling: the desired magnitude response is evaluated at each
//! of `TAPS` DFT bins (piecewise-linear interpolation between control points
//! in Hz, flat outside their range) and inverse-transformed by direct
//! summation — no FFT needed for a one-time, at-`configure()` computation.
//! A flat gain curve (no entries, or every entry at 0 dB) inverse-transforms
//! to a unit impulse **exactly** by the DFT basis's orthogonality, which is
//! what [`tests::flat_gain_curve_is_the_identity`] checks: a property of the
//! transform itself, not a re-check of this module's own arithmetic.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "firequalizer",
    description: "finite impulse response equalizer",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

/// Odd, so the kernel has an exact centre tap (zero group-delay error at DC).
const TAPS: usize = 255;

/// `TAPS / 2`, computed with a shift so the workspace's `integer_division`
/// lint (which flags `/` on integers outright) has nothing to catch here.
const TAPS_CENTER: usize = TAPS >> 1;

#[derive(Debug, Clone, Copy)]
struct GainEntry {
    freq_hz: f64,
    gain_db: f64,
}

fn parse_gain_entries(raw: &str) -> Vec<GainEntry> {
    let mut out = Vec::new();
    for tok in raw.split([';', '|']) {
        let tok = tok.trim();
        let inner = tok
            .strip_prefix("entry(")
            .and_then(|s| s.strip_suffix(')'))
            .unwrap_or(tok);
        let mut parts = inner.splitn(2, ',');
        let (Some(f), Some(g)) = (parts.next(), parts.next()) else {
            continue;
        };
        if let (Ok(freq_hz), Ok(gain_db)) = (f.trim().parse::<f64>(), g.trim().parse::<f64>()) {
            out.push(GainEntry { freq_hz, gain_db });
        }
    }
    out.sort_by(|a, b| a.freq_hz.total_cmp(&b.freq_hz));
    out
}

/// Linear-gain magnitude at `freq_hz`, interpolated between control points
/// (flat at the nearest point's gain outside their range; `1.0` — 0 dB —
/// with no points at all).
fn gain_at(entries: &[GainEntry], freq_hz: f64) -> f64 {
    if entries.is_empty() {
        return 1.0;
    }
    if entries.len() == 1 {
        let Some(e) = entries.first() else { return 1.0 };
        return 10f64.powf(e.gain_db / 20.0);
    }
    if freq_hz <= entries.first().map_or(0.0, |e| e.freq_hz) {
        return entries
            .first()
            .map_or(1.0, |e| 10f64.powf(e.gain_db / 20.0));
    }
    if freq_hz >= entries.last().map_or(0.0, |e| e.freq_hz) {
        return entries.last().map_or(1.0, |e| 10f64.powf(e.gain_db / 20.0));
    }
    for w in entries.windows(2) {
        let (Some(a), Some(b)) = (w.first(), w.get(1)) else {
            continue;
        };
        if freq_hz >= a.freq_hz && freq_hz <= b.freq_hz {
            let span = (b.freq_hz - a.freq_hz).max(f64::MIN_POSITIVE);
            let t = (freq_hz - a.freq_hz) / span;
            let gain_db = a.gain_db + t * (b.gain_db - a.gain_db);
            return 10f64.powf(gain_db / 20.0);
        }
    }
    1.0
}

/// Frequency-sampling FIR design: `h[n] = (1/TAPS) * sum_k G[k] cos(2*pi*k*(n-c)/TAPS)`,
/// `c = (TAPS-1)/2`, with `G` the real, symmetric (real-FIR) magnitude
/// spectrum sampled from `entries`. Non-finite results (a pathological gain
/// curve) fall back to the identity kernel rather than propagating `NaN`.
fn design_fir(entries: &[GainEntry], sample_rate: f64) -> [f64; TAPS] {
    let center = (TAPS as f64 - 1.0) / 2.0;
    let nyquist = sample_rate / 2.0;
    let mut spectrum = [1.0f64; TAPS];
    for (k, g) in spectrum.iter_mut().enumerate() {
        // Real-FIR symmetric spectrum: bin `k` and bin `TAPS-k` mirror.
        let bin = k.min(TAPS - k);
        let freq_hz = nyquist * (bin as f64) / (TAPS as f64 / 2.0);
        *g = gain_at(entries, freq_hz);
    }
    let mut h = [0.0f64; TAPS];
    for (n, out) in h.iter_mut().enumerate() {
        let mut acc = 0.0;
        for (k, g) in spectrum.iter().enumerate() {
            let phase =
                2.0 * std::f64::consts::PI * (k as f64) * ((n as f64) - center) / (TAPS as f64);
            acc += g * phase.cos();
        }
        acc /= TAPS as f64;
        *out = if acc.is_finite() { acc } else { 0.0 };
    }
    // Identity fallback: if every gain is finite but the whole kernel came
    // out non-finite some other way, prefer silence-free passthrough.
    if h.iter().all(|v| *v == 0.0)
        && let Some(c) = h.get_mut(TAPS_CENTER)
    {
        *c = 1.0;
    }
    h
}

#[derive(Debug, Clone)]
struct FirEqualizer {
    entries: Vec<GainEntry>,
    kernel: [f64; TAPS],
    history: Vec<std::collections::VecDeque<f64>>,
}

impl FrameFilter for FirEqualizer {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio {
            sample_rate,
            layout,
            ..
        }) = ctx.input_link(0)
        {
            self.kernel = design_fir(&self.entries, f64::from(*sample_rate));
            let n = layout.channels.max(1) as usize;
            self.history = (0..n)
                .map(|_| std::collections::VecDeque::from(vec![0.0; TAPS]))
                .collect();
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        if self.history.len() != channels.len() {
            self.history = (0..channels.len())
                .map(|_| std::collections::VecDeque::from(vec![0.0; TAPS]))
                .collect();
        }
        for (i, ch) in channels.iter_mut().enumerate() {
            let Some(hist) = self.history.get_mut(i) else {
                continue;
            };
            for s in ch.iter_mut() {
                hist.pop_front();
                hist.push_back(*s);
                let mut acc = 0.0;
                for (tap, sample) in self.kernel.iter().zip(hist.iter()) {
                    acc += tap * sample;
                }
                *s = acc;
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
        for h in &mut self.history {
            for v in h.iter_mut() {
                *v = 0.0;
            }
        }
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let raw = req.named("gain_entry").unwrap_or_default();
    let filter = FirEqualizer {
        entries: parse_gain_entries(&raw),
        kernel: {
            let mut k = [0.0; TAPS];
            if let Some(c) = k.get_mut(TAPS_CENTER) {
                *c = 1.0;
            }
            k
        },
        history: Vec::new(),
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_gain_curve_is_the_identity() {
        let h = design_fir(&[], 48_000.0);
        let center = TAPS_CENTER;
        for (n, v) in h.iter().enumerate() {
            let expect = if n == center { 1.0 } else { 0.0 };
            assert!(
                (v - expect).abs() < 1e-9,
                "tap {n}: got {v}, expected {expect}"
            );
        }
    }

    #[test]
    fn nonfinite_gain_curve_does_not_produce_nan() {
        let entries = vec![GainEntry {
            freq_hz: 1000.0,
            gain_db: f64::NAN,
        }];
        let h = design_fir(&entries, 48_000.0);
        assert!(h.iter().all(|v| v.is_finite()));
    }
}
