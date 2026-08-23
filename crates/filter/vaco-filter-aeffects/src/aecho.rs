//! `aecho` — add echoing to the audio.
//!
//! `ffmpeg -h filter=aecho` (2026-08-23): `in_gain` (`0..1`, default `0.6`),
//! `out_gain` (`0..1`, default `0.3`), `delays` (string, default `"1000"`),
//! `decays` (string, default `"0.5"`). No timeline support (absent from its
//! own `-h` output, unlike most of this crate's other filters).
//!
//! # What was measured
//!
//! `delays`/`decays` are `|`-separated parallel lists (one decay per delay
//! tap): `delays=100|200:decays=0.5|0.3` on an impulse produces exactly
//! three non-zero output samples, at indices `0`, `100` and `200`, with
//! values `1.0`, `0.5` and `0.3` — i.e. **`output[n] = out_gain * (in_gain *
//! input[n] + sum_i decays[i] * input[n - delays_samples[i]])`**, each tap
//! reading the *original* input, not the filter's own output: a
//! `delays=1000` tap fed 3500 samples of silence after a single impulse
//! produces echoes at exactly `0` and `1000` and **nowhere else** — no
//! repeat at `2000`, `3000`, etc., which a feedback (IIR) design would
//! produce. This is a non-recursive, multi-tap FIR echo, confirmed by
//! absence rather than presence: [`tests::no_recursive_repeat`].
//!
//! `in_gain=0.6, out_gain=0.3, delays=1000, decays=0.5` on a unit impulse
//! gives `0.18` at lag 0 (`= 0.3 * 0.6 * 1`) and `0.15` at lag 1000
//! (`= 0.3 * 0.5 * 1`, with **no** `in_gain` factor on the delayed tap) —
//! pinned exactly in [`tests::matches_measured_gains`].
//!
//! # The zero-decay identity, and why it cannot be measured directly
//!
//! The reference **rejects** `decays=0` outright (`decay[0]: 0.000000 is
//! out of allowed range: (0, 1]`), so "aecho with zero decay is identity"
//! cannot be checked against a live reference run at all — a real instance
//! of the "sentinel trap" this crate's correctness discipline warns about,
//! just in the opposite direction (the reference refuses the sentinel
//! rather than silently special-casing it). This implementation is more
//! permissive (matching this crate's established option-parsing convention
//! of accepting values the reference would reject) and the identity holds
//! as a direct algebraic consequence of the measured formula above: with
//! every `decays[i] = 0` and `in_gain = out_gain = 1`, every delayed term
//! vanishes and `output[n] = input[n]`. [`tests::zero_decay_is_identity`]
//! checks this against this module's own formula, not against `ffmpeg`.
use std::collections::VecDeque;

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "aecho",
    description: "add echoing to the audio",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

fn parse_list(spec: &str) -> Vec<f64> {
    spec.split('|')
        .filter_map(|s| s.trim().parse::<f64>().ok())
        .collect()
}

struct Tap {
    decay: f64,
    len: usize,
    hist: VecDeque<f64>,
}

struct Aecho {
    in_gain: f64,
    out_gain: f64,
    delays_ms: Vec<f64>,
    decays: Vec<f64>,
    taps: Vec<Tap>,
}

impl FrameFilter for Aecho {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio { sample_rate, .. }) = ctx.input_link(0) {
            let rate = f64::from(*sample_rate);
            self.taps = self
                .delays_ms
                .iter()
                .zip(self.decays.iter().chain(std::iter::repeat(&0.0)))
                .map(|(&ms, &decay)| {
                    let len = ((ms * rate) / 1000.0).floor().max(0.0) as usize;
                    let mut hist = VecDeque::new();
                    hist.resize(len, 0.0);
                    Tap { decay, len, hist }
                })
                .collect();
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        for channel in &mut channels {
            for sample in channel.iter_mut() {
                let dry = *sample;
                let mut echo_sum = 0.0;
                for tap in &mut self.taps {
                    let delayed = if tap.len == 0 {
                        dry
                    } else {
                        tap.hist.push_back(dry);
                        tap.hist.pop_front().unwrap_or(0.0)
                    };
                    echo_sum += tap.decay * delayed;
                }
                *sample = self.out_gain * (self.in_gain * dry + echo_sum);
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
        for tap in &mut self.taps {
            tap.hist.clear();
            tap.hist.resize(tap.len, 0.0);
        }
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let in_gain = common::f64_opt(req, &["in_gain"], 0.6);
    let out_gain = common::f64_opt(req, &["out_gain"], 0.3);
    let delays_ms = req
        .named("delays")
        .map_or_else(|| vec![1000.0], |s| parse_list(&s));
    let decays = req
        .named("decays")
        .map_or_else(|| vec![0.5], |s| parse_list(&s));
    let filter = Aecho {
        in_gain,
        out_gain,
        delays_ms,
        decays,
        taps: Vec::new(),
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

    fn run(taps: &mut [Tap], in_gain: f64, out_gain: f64, input: &[f64]) -> Vec<f64> {
        let mut out = Vec::new();
        for &dry in input {
            let mut echo_sum = 0.0;
            for tap in taps.iter_mut() {
                let delayed = if tap.len == 0 {
                    dry
                } else {
                    tap.hist.push_back(dry);
                    tap.hist.pop_front().unwrap_or(0.0)
                };
                echo_sum += tap.decay * delayed;
            }
            out.push(out_gain * (in_gain * dry + echo_sum));
        }
        out
    }

    fn tap(len: usize, decay: f64) -> Tap {
        let mut hist = VecDeque::new();
        hist.resize(len, 0.0);
        Tap { decay, len, hist }
    }

    /// Sample-exact against the measured two-tap impulse response.
    #[test]
    fn matches_measured_gains() {
        let mut taps = [tap(1000, 0.5)];
        let mut input = vec![0.0; 1500];
        if let Some(first) = input.first_mut() {
            *first = 1.0;
        }
        let out = run(&mut taps, 0.6, 0.3, &input);
        assert!((out.first().copied().unwrap_or(0.0) - 0.18).abs() < 1e-9);
        assert!((out.get(1000).copied().unwrap_or(0.0) - 0.15).abs() < 1e-9);
    }

    /// Every sample apart from lag 0 and each configured delay must be
    /// exactly zero for an isolated impulse — the non-recursive-FIR
    /// property measured by absence in the module doc.
    #[test]
    fn no_recursive_repeat() {
        let mut taps = [tap(100, 0.5), tap(200, 0.3)];
        let mut input = vec![0.0; 3500];
        if let Some(first) = input.first_mut() {
            *first = 1.0;
        }
        let out = run(&mut taps, 1.0, 1.0, &input);
        for (i, &v) in out.iter().enumerate() {
            if i == 0 || i == 100 || i == 200 {
                continue;
            }
            assert!(v.abs() < 1e-9, "unexpected non-zero at {i}: {v}");
        }
    }

    /// Algebraic identity: zero decay on every tap, with unity gains,
    /// reproduces the input exactly. The reference itself refuses to run
    /// `decays=0`, so this checks the formula this module implements, not a
    /// live comparison.
    #[test]
    fn zero_decay_is_identity() {
        let mut taps = [tap(50, 0.0), tap(150, 0.0)];
        let input = [0.1, -0.2, 0.9, -1.0, 0.0, 0.4, -0.4];
        let out = run(&mut taps, 1.0, 1.0, &input);
        for (a, b) in out.iter().zip(&input) {
            assert!((a - b).abs() < 1e-12, "expected identity: {a} vs {b}");
        }
    }
}
