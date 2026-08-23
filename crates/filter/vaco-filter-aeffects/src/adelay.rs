//! `adelay` — delay one or more audio channels.
//!
//! `ffmpeg -h filter=adelay` (2026-08-23): `delays` (string, default `""`),
//! `all` (bool, default `false`). Supports timeline (`enable`).
//!
//! # What was measured
//!
//! `delays` is `|`-separated milliseconds, one entry per channel, converted
//! to a whole-sample delay via `floor(ms * sample_rate / 1000)`: at
//! `sample_rate=1000`, `delays=3|5` puts a stereo impulse at sample index 3
//! on the left and index 5 on the right, and a fractional `delays=2.5|2.9`
//! puts both at index 2 (floor, not round — `2.9` does **not** reach index
//! 3). A channel with no corresponding `delays` entry is left **unshifted**
//! when `all=false` (the default) — a right-channel impulse with only
//! `delays=3` given stays at index 0 — and reuses the **last given** delay
//! when `all=true` — the same input then puts it at index 3 too. See
//! [`tests::matches_measured_impulse_positions`].
//!
//! `adelay=0` (a single zero delay, or nothing) is exactly the identity:
//! every channel gets a zero-sample delay line, i.e. no change at all.
use std::collections::VecDeque;

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "adelay",
    description: "delay one or more audio channels",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

/// A defensive cap on one channel's delay, in milliseconds — not a
/// conformance clamp, since unlike `haas`/`stereowiden` the reference
/// declares no numeric range for `delays` at all (`ffmpeg -h filter=adelay`
/// prints it as a bare `<string>`). Every millisecond becomes one
/// `VecDeque<f64>` entry (`delay_ms * sample_rate / 1000`), so an
/// unbounded value is an unbounded, attacker-sized allocation reached
/// before `FramePool`'s own limits can see it — the same shape a fuzz
/// target found in `cellauto`'s frame-size options elsewhere in this
/// project. Ten minutes per channel is far past any real A/V-sync use of
/// this filter while still ruling out a multi-gigabyte delay line from one
/// absurd option string.
const MAX_DELAY_MS: f64 = 600_000.0;

/// Parse a `|`-separated list of millisecond delays into a per-channel
/// sample-count table, expanded to `channels` entries: unlisted channels get
/// `0` unless `all`, in which case they repeat the last listed value (or `0`
/// if none was given at all).
fn resolve_delays(spec: &str, channels: usize, sample_rate: f64, all: bool) -> Vec<usize> {
    let listed: Vec<usize> = spec
        .split('|')
        .filter_map(|s| s.trim().parse::<f64>().ok())
        .map(|ms| {
            ((ms.clamp(0.0, MAX_DELAY_MS) * sample_rate) / 1000.0)
                .floor()
                .max(0.0) as usize
        })
        .collect();

    let mut out = Vec::new();
    for ch in 0..channels {
        let value = if let Some(&v) = listed.get(ch) {
            v
        } else if all {
            listed.last().copied().unwrap_or(0)
        } else {
            0
        };
        out.push(value);
    }
    out
}

struct DelayLine {
    hist: VecDeque<f64>,
    len: usize,
}

impl DelayLine {
    fn new(len: usize) -> Self {
        let mut hist = VecDeque::new();
        hist.resize(len, 0.0);
        Self { hist, len }
    }

    fn step(&mut self, x: f64) -> f64 {
        if self.len == 0 {
            return x;
        }
        self.hist.push_back(x);
        self.hist.pop_front().unwrap_or(0.0)
    }

    fn flush(&mut self) {
        self.hist.clear();
        self.hist.resize(self.len, 0.0);
    }
}

struct Adelay {
    spec: String,
    all: bool,
    lines: Vec<DelayLine>,
}

impl FrameFilter for Adelay {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio {
            sample_rate,
            layout,
            ..
        }) = ctx.input_link(0)
        {
            let channels = layout.channels.max(1) as usize;
            let delays = resolve_delays(&self.spec, channels, f64::from(*sample_rate), self.all);
            self.lines = delays.into_iter().map(DelayLine::new).collect();
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        for (idx, channel) in channels.iter_mut().enumerate() {
            let Some(line) = self.lines.get_mut(idx) else {
                continue;
            };
            for sample in channel.iter_mut() {
                *sample = line.step(*sample);
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
        for line in &mut self.lines {
            line.flush();
        }
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let spec = req
        .named("delays")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let all = common::bool_opt(req, &["all"], false);
    let filter = Adelay {
        spec,
        all,
        lines: Vec::new(),
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

    /// Sample-exact against the measurements in the module doc: impulse
    /// positions for whole and fractional delays, and the `all` flag's
    /// effect on channels with no explicit entry.
    #[test]
    fn matches_measured_impulse_positions() {
        let cases: &[(&str, bool, usize, &[usize])] = &[
            ("3|5", false, 2, &[3, 5]),
            ("2.5|2.9", false, 2, &[2, 2]),
            ("3", false, 2, &[3, 0]),
            ("3", true, 2, &[3, 3]),
        ];
        for &(spec, all, channels, want) in cases {
            let got = resolve_delays(spec, channels, 1000.0, all);
            assert_eq!(got, want, "spec={spec} all={all}");
        }
    }

    /// An absurd delay must not turn into an absurd allocation:
    /// `resolve_delays` clamps to [`MAX_DELAY_MS`] before converting to a
    /// sample count, so even a delay spec many orders of magnitude past any
    /// real use resolves to a bounded `Vec<usize>` capacity, not an OOM.
    #[test]
    fn an_absurd_delay_is_clamped_not_allocated_verbatim() {
        let got = resolve_delays("1e15", 1, 192_000.0, false);
        let max_samples = (MAX_DELAY_MS * 192_000.0 / 1000.0) as usize;
        assert_eq!(got, vec![max_samples]);
        // Comfortably below "attacker asked for petabytes": a bound in the
        // hundreds of millions of samples, not 10^17.
        assert!(max_samples < 200_000_000, "got {max_samples}");
    }

    /// `adelay` with every channel's delay resolving to zero must be an
    /// exact identity — the invariant the crate's correctness discipline
    /// calls out by name for this family.
    #[test]
    fn zero_delay_is_identity() {
        let delays = resolve_delays("0|0", 2, 48000.0, false);
        assert_eq!(delays, vec![0, 0]);
        let mut lines: Vec<DelayLine> = delays.into_iter().map(DelayLine::new).collect();
        let input = [0.1, -0.5, 0.75, -0.9, 0.0];
        for &x in &input {
            for line in &mut lines {
                assert!((line.step(x) - x).abs() < 1e-12);
            }
        }
    }

    /// Falsifiable: a delay line of length N must reproduce an impulse
    /// exactly N samples later and nowhere else.
    #[test]
    fn delay_line_places_impulse_at_exactly_n() {
        for n in [0usize, 1, 4, 10] {
            let mut line = DelayLine::new(n);
            let mut out = Vec::new();
            for i in 0..(n + 5) {
                let x = if i == 0 { 1.0 } else { 0.0 };
                out.push(line.step(x));
            }
            for (i, &v) in out.iter().enumerate() {
                if i == n {
                    assert!((v - 1.0).abs() < 1e-12, "n={n} i={i}: {v}");
                } else {
                    assert!(v.abs() < 1e-12, "n={n} i={i}: {v}");
                }
            }
        }
    }
}
