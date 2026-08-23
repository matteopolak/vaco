//! `haas` — apply a Haas stereo enhancer.
//!
//! `ffmpeg -h filter=haas` (2026-08-23): `level_in`/`level_out` (default `1`),
//! `side_gain` (default `1`), `middle_source` (`left`/`right`/`mid`/`side`,
//! default `mid`), `middle_phase` (default `false`), `left_delay` (`0..40`
//! ms, default `2.05`), `left_balance` (`-1..1`, default `-1`), `left_gain`
//! (default `1`), `left_phase` (default `false`), `right_delay` (default
//! `2.12`), `right_balance` (default `1`), `right_gain` (default `1`),
//! `right_phase` (default `true`).
//!
//! # Measured formula (D17)
//!
//! A full-scale impulse in the left channel through `haas` at its **default**
//! options, at 48000 Hz, produces exactly three non-zero output samples:
//! `(16000, 16000)` at lag 0, `(0, 16000)` at lag 98, and `(-16000, 0)` at
//! lag 101. `98 = floor(2.05 ms * 48000 Hz / 1000)`, `101 =
//! floor(2.12 ms * 48000 Hz / 1000)`. `16000 = 32000 / 2`, i.e. `mid = (L +
//! R) / 2` with `L = 32000, R = 0`.
//!
//! Working backwards: the *undelayed* `mid` signal appears identically in
//! both outputs at lag 0 (`side_gain`'s default of `1` times `mid`, added to
//! both channels — a direct, unpanned centre image). Two further branches
//! are each `mid`, delayed and gain/phase-adjusted per their own
//! `{left,right}_*` options, then panned by `{left,right}_balance` into the
//! *opposite* sense from what the names suggest: `left_balance = -1`
//! (the "hard left" end of its range) puts its branch **entirely into the
//! right output**, and `right_balance = 1` puts its branch **entirely into
//! the left output** — measured, not assumed, since the alternative
//! (balance routing into the like-named channel) does not match. That
//! resolves to a generic per-branch pan law that does not care which
//! branch it is:
//!
//! ```text
//! contribution_to_L = branch * (1 + balance) / 2
//! contribution_to_R = branch * (1 - balance) / 2
//! ```
//!
//! (`balance = -1` above zeroes the `L` contribution and keeps the branch's
//! full value in `R`, matching the measurement.) `right_phase`'s default of
//! `true` inverts that branch's sign, which is why the lag-101 sample is
//! negative. [`tests::matches_measured_default_impulse_response`] pins the
//! full three-sample sequence.
//!
//! **Structural, not measured:** `middle_source` values other than the
//! default `mid`, `middle_phase`, and whether `side_gain` scales only the
//! direct term or the whole mix — the default probe above cannot distinguish
//! `side_gain` from "no gain applied" since it is `1` either way. See
//! `docs/filter/vaco-filter-aeffects.md`.

use std::collections::VecDeque;

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "haas",
    description: "apply Haas Stereo Enhancer",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MiddleSource {
    Left,
    Right,
    Mid,
    Side,
}

impl MiddleSource {
    fn parse(s: &str) -> Self {
        match s.trim() {
            "left" | "0" => Self::Left,
            "right" | "1" => Self::Right,
            "side" | "3" => Self::Side,
            _ => Self::Mid,
        }
    }

    fn compute(self, l: f64, r: f64) -> f64 {
        match self {
            Self::Left => l,
            Self::Right => r,
            Self::Mid => (l + r) * 0.5,
            Self::Side => (l - r) * 0.5,
        }
    }
}

struct Branch {
    delay_ms: f64,
    balance: f64,
    gain: f64,
    phase: bool,
    delay_samples: usize,
    hist: VecDeque<f64>,
}

impl Branch {
    fn new(delay_ms: f64, balance: f64, gain: f64, phase: bool) -> Self {
        Self {
            delay_ms,
            balance,
            gain,
            phase,
            delay_samples: 0,
            hist: VecDeque::new(),
        }
    }

    fn configure(&mut self, sample_rate: f64) {
        self.delay_samples = ((self.delay_ms * sample_rate) / 1000.0) as usize;
        self.flush();
    }

    /// Push `mid` into this branch's delay line and return its
    /// `(to_l, to_r)` contribution for the sample that is now `delay_samples`
    /// old.
    ///
    /// The queue is pre-filled with `delay_samples` zeros in [`Self::flush`]
    /// so that `front()` reads a *fixed* `delay_samples`-old value from the
    /// very first call — without the pre-fill, a queue that merely caps its
    /// own length at `delay_samples` returns whatever it has accumulated so
    /// far while still filling up, which is a *growing*, not fixed, delay
    /// for the first `delay_samples` calls.
    fn step(&mut self, mid: f64) -> (f64, f64) {
        let sign = if self.phase { -1.0 } else { 1.0 };
        let delayed = self.hist.front().copied().unwrap_or(0.0);
        self.hist.push_back(mid);
        self.hist.pop_front();
        let value = sign * self.gain * delayed;
        let to_l = value * (1.0 + self.balance) * 0.5;
        let to_r = value * (1.0 - self.balance) * 0.5;
        (to_l, to_r)
    }

    fn flush(&mut self) {
        self.hist.clear();
        self.hist.resize(self.delay_samples.max(1), 0.0);
    }
}

struct Haas {
    level_in: f64,
    level_out: f64,
    side_gain: f64,
    middle_source: MiddleSource,
    middle_phase: bool,
    left: Branch,
    right: Branch,
}

impl FrameFilter for Haas {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio { sample_rate, .. }) = ctx.input_link(0) {
            self.left.configure(f64::from(*sample_rate));
            self.right.configure(f64::from(*sample_rate));
        }
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
                let l = channels
                    .first()
                    .and_then(|c| c.get(i))
                    .copied()
                    .unwrap_or(0.0)
                    * self.level_in;
                let r = channels
                    .get(1)
                    .and_then(|c| c.get(i))
                    .copied()
                    .unwrap_or(0.0)
                    * self.level_in;
                let mut mid = self.middle_source.compute(l, r);
                if self.middle_phase {
                    mid = -mid;
                }
                let (l_to_l, l_to_r) = self.left.step(mid);
                let (r_to_l, r_to_r) = self.right.step(mid);
                let out_l = self.level_out * (self.side_gain * mid + l_to_l + r_to_l);
                let out_r = self.level_out * (self.side_gain * mid + l_to_r + r_to_r);
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
        self.left.flush();
        self.right.flush();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let level_in = common::f64_opt(req, &["level_in"], 1.0);
    let level_out = common::f64_opt(req, &["level_out"], 1.0);
    let side_gain = common::f64_opt(req, &["side_gain"], 1.0);
    let middle_source = req
        .named("middle_source")
        .map_or(MiddleSource::Mid, |s| MiddleSource::parse(&s));
    let middle_phase = common::bool_opt(req, &["middle_phase"], false);
    // `ffmpeg -h filter=haas` (2026-08-23): `left_delay`/`right_delay` are
    // `0..40` ms and `left_balance`/`right_balance` are `-1..1`. Clamping
    // matches the reference's own option range *and* keeps
    // `Branch::configure`'s `delay_samples` — sized directly from
    // `delay_ms * sample_rate` with no cap of its own — from turning an
    // absurd `delay_ms` into an absurd `Vec`-backed `VecDeque` allocation
    // (the same shape `cellauto=size=911111x91111` hit elsewhere in this
    // project's fuzzing: an attacker-sized allocation reached before
    // `FramePool`'s own limits could see it).
    let left = Branch::new(
        common::f64_opt(req, &["left_delay"], 2.05).clamp(0.0, 40.0),
        common::f64_opt(req, &["left_balance"], -1.0).clamp(-1.0, 1.0),
        common::f64_opt(req, &["left_gain"], 1.0),
        common::bool_opt(req, &["left_phase"], false),
    );
    let right = Branch::new(
        common::f64_opt(req, &["right_delay"], 2.12).clamp(0.0, 40.0),
        common::f64_opt(req, &["right_balance"], 1.0).clamp(-1.0, 1.0),
        common::f64_opt(req, &["right_gain"], 1.0),
        common::bool_opt(req, &["right_phase"], true),
    );
    let filter = Haas {
        level_in,
        level_out,
        side_gain,
        middle_source,
        middle_phase,
        left,
        right,
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter)),
    }
}

#[cfg(test)]
mod tests {
    use super::{Branch, MiddleSource};

    /// Sample-exact against `ffmpeg -af haas` (all-default options) at
    /// 48000 Hz on 2026-08-23: a full-scale left-channel impulse produces
    /// exactly `(16000, 16000)` at lag 0, `(0, 16000)` at lag 98 and
    /// `(-16000, 0)` at lag 101, out of 32000/32768 amplitude.
    #[test]
    fn matches_measured_default_impulse_response() {
        let amplitude = 32000.0 / 32768.0;
        let mut left = Branch::new(2.05, -1.0, 1.0, false);
        let mut right = Branch::new(2.12, 1.0, 1.0, true);
        left.configure(48000.0);
        right.configure(48000.0);
        assert_eq!(left.delay_samples, 98);
        assert_eq!(right.delay_samples, 101);

        let mut out = Vec::new();
        for n in 0..150 {
            let l = if n == 0 { amplitude } else { 0.0 };
            let r = 0.0;
            let mid = MiddleSource::Mid.compute(l, r);
            let (l_to_l, l_to_r) = left.step(mid);
            let (r_to_l, r_to_r) = right.step(mid);
            out.push((mid + l_to_l + r_to_l, mid + l_to_r + r_to_r));
        }

        let want_lag0 = amplitude / 2.0;
        let (got_l, got_r) = out.first().copied().unwrap_or((0.0, 0.0));
        assert!((got_l - want_lag0).abs() < 1e-9, "lag0 L: {got_l}");
        assert!((got_r - want_lag0).abs() < 1e-9, "lag0 R: {got_r}");

        let (got_l, got_r) = out.get(98).copied().unwrap_or((0.0, 0.0));
        assert!((got_l - 0.0).abs() < 1e-9, "lag98 L: {got_l}");
        assert!((got_r - want_lag0).abs() < 1e-9, "lag98 R: {got_r}");

        let (got_l, got_r) = out.get(101).copied().unwrap_or((0.0, 0.0));
        assert!((got_l - (-want_lag0)).abs() < 1e-9, "lag101 L: {got_l}");
        assert!((got_r - 0.0).abs() < 1e-9, "lag101 R: {got_r}");
    }

    #[test]
    fn middle_source_selects_the_documented_channel() {
        assert_eq!(MiddleSource::parse("left"), MiddleSource::Left);
        assert_eq!(MiddleSource::parse("right"), MiddleSource::Right);
        assert_eq!(MiddleSource::parse("mid"), MiddleSource::Mid);
        assert_eq!(MiddleSource::parse("side"), MiddleSource::Side);
        assert!((MiddleSource::Side.compute(1.0, 0.4) - 0.3).abs() < 1e-12);
    }
}
