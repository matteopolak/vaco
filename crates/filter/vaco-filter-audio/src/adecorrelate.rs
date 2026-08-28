//! `adecorrelate` — apply decorrelation to input audio.
//!
//! `ffmpeg -h filter=adecorrelate` (2026-08-27): `stages` (1 to 16, default
//! 6), `seed` (default -1, meaning "pick one"). Single audio pad in, single
//! audio pad out, no `eof_action` section (not a `framesync` candidate).
//!
//! # Why this cannot be measured, and what is built instead
//!
//! Decorrelation-by-random-allpass is a documented, published technique —
//! Kendall, "The Decorrelation of Audio Signals and Its Impact on Spatial
//! Imagery" (Computer Music Journal, 1995): cascade a channel through a
//! handful of allpass sections whose parameters are drawn independently per
//! channel, which scrambles inter-channel phase without touching the
//! per-channel magnitude spectrum. But *which* random sequence the reference
//! draws is not observable from outside the binary — `seed=-1` is
//! nondeterministic by the option's own default, and even a fixed `seed`
//! only pins *the reference's own* generator, not one this project can
//! recover by probing input/output pairs (D7/D17: matching an unpublished
//! `av_lfg`-driven design from black-box measurement alone is not
//! tractable, the same call already made for `vaco-filter-asource`'s
//! `anoisesrc` and `vaco-filter-temporal`/`vaco-filter-source`'s generators).
//!
//! So this implementation reproduces the *documented effect* — cascaded
//! Schroeder allpass sections (Schroeder, "Natural Sounding Artificial
//! Reverberation", JAES 1962 — the same allpass comb structure reverb
//! algorithms use, applied here for its phase-only, magnitude-preserving
//! property rather than its reverberant one), independently seeded per
//! channel so channels decorrelate from each other — without claiming to
//! reproduce the reference's specific coefficients. `seed >= 0` is
//! reproducible run to run; `seed < 0` falls back to a fixed constant rather
//! than a time-based one, matching `vaco-filter-asource::rng`'s own
//! documented reasoning: a `seed` option is for reproducibility, not for
//! imitating a bitstream nobody can observe.
//!
//! # What is verified
//!
//! Not "does this match the reference" (it cannot, per above) but the real
//! mathematical property a Schroeder allpass section must have: it is
//! energy-preserving. Feeding an impulse through one stage and summing the
//! squared output over a window long enough for the feedback to decay below
//! machine epsilon reproduces the impulse's own energy (`1.0`) to within
//! floating-point tolerance — see `tests::single_stage_is_energy_preserving`.
//! That is a property of *any* correct Schroeder allpass, independent of
//! this module's own difference equation, so it is a real oracle rather than
//! a second copy of the same arithmetic.

use std::collections::VecDeque;

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

const AUDIO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Audio,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "adecorrelate",
    description: "apply decorrelation to input audio",
    inputs: AUDIO_PAD,
    outputs: AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

/// `SplitMix64` (Vigna, public domain / CC0). Duplicated rather than shared:
/// `vaco-filter-asource::rng` keeps its copy `pub(crate)` for the identical
/// reason documented there, and this crate does not depend on that one.
#[allow(
    clippy::unreadable_literal,
    reason = "the SplitMix64 constants are the published magic numbers"
)]
#[derive(Debug, Clone, Copy)]
struct SplitMix64 {
    state: u64,
}

#[allow(
    clippy::unreadable_literal,
    reason = "the SplitMix64 constants are the published magic numbers"
)]
impl SplitMix64 {
    const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    /// A uniform `f64` in `[0, 1)`.
    fn next_unit(&mut self) -> f64 {
        let bits = self.next_u64() >> 11;
        #[allow(
            clippy::cast_precision_loss,
            reason = "53 significant bits fit exactly in an f64 mantissa"
        )]
        {
            (bits as f64) * (1.0 / ((1u64 << 53) as f64))
        }
    }
}

const fn resolve_seed(seed: i64, fallback: u64) -> u64 {
    if seed < 0 {
        fallback
    } else {
        #[allow(clippy::cast_sign_loss, reason = "seed >= 0 was just checked")]
        {
            seed as u64
        }
    }
}

/// One Schroeder allpass section: `w[n] = x[n] + g*w[n-M]`,
/// `y[n] = -g*w[n] + w[n-M]`. Unconditionally stable for `|g| < 1`.
#[derive(Debug, Clone)]
struct AllpassStage {
    delay: VecDeque<f64>,
    gain: f64,
}

impl AllpassStage {
    fn new(delay_samples: usize, gain: f64) -> Self {
        Self {
            delay: std::iter::repeat_n(0.0, delay_samples.max(1)).collect(),
            gain,
        }
    }

    fn process(&mut self, x: f64) -> f64 {
        let w_delayed = self.delay.pop_front().unwrap_or(0.0);
        let w = self.gain.mul_add(w_delayed, x);
        self.delay.push_back(w);
        self.gain.mul_add(-w, w_delayed)
    }

    fn reset(&mut self) {
        for slot in &mut self.delay {
            *slot = 0.0;
        }
    }
}

/// A cascade of [`AllpassStage`]s for one channel.
#[derive(Debug, Clone)]
struct Cascade(Vec<AllpassStage>);

impl Cascade {
    /// Independently-seeded per channel (`base_seed` mixed with `channel`),
    /// so left and right decorrelate from each other rather than running the
    /// same random filter twice. Delay lengths span 5-500 samples (a few
    /// tenths of a millisecond to ~10 ms at 48 kHz — long enough to scramble
    /// phase across the audible band, short enough to stay a phase effect
    /// rather than an audible echo) and the feedback gain is fixed at 0.6, a
    /// typical Schroeder allpass value from the published reverb literature.
    fn new(stages: u32, base_seed: u64, channel: u32) -> Self {
        let mut rng = SplitMix64::new(base_seed ^ (u64::from(channel).wrapping_mul(0x9E37_79B1)));
        let mut v = Vec::new();
        for _ in 0..stages {
            let delay = 5 + (rng.next_unit() * 495.0) as usize;
            v.push(AllpassStage::new(delay, 0.6));
        }
        Self(v)
    }

    fn process(&mut self, x: f64) -> f64 {
        let mut y = x;
        for stage in &mut self.0 {
            y = stage.process(y);
        }
        y
    }

    fn reset(&mut self) {
        for stage in &mut self.0 {
            stage.reset();
        }
    }
}

#[derive(Debug, Clone)]
struct Decorrelate {
    stages: u32,
    seed: u64,
    channels: Vec<Cascade>,
}

impl FrameFilter for Decorrelate {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(vaco_filter_core::LinkFormat::Audio { layout, .. }) = ctx.input_link(0) {
            let n = layout.channels.max(1);
            self.channels = (0..n)
                .map(|c| Cascade::new(self.stages, self.seed, c))
                .collect();
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        if self.channels.len() != channels.len() {
            self.channels = (0..channels.len() as u32)
                .map(|c| Cascade::new(self.stages, self.seed, c))
                .collect();
        }
        for (ch, cascade) in channels.iter_mut().zip(self.channels.iter_mut()) {
            for s in ch.iter_mut() {
                *s = cascade.process(*s);
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
        for c in &mut self.channels {
            c.reset();
        }
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let stages = req
        .named("stages")
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(6)
        .clamp(1, 16);
    let seed_opt = req
        .named("seed")
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(-1);
    let seed = resolve_seed(seed_opt, 0xADEC_0771_1234_5678);
    let filter = Decorrelate {
        stages,
        seed,
        channels: Vec::new(),
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

    /// The real oracle: a Schroeder allpass section is energy-preserving.
    /// Feed an impulse through one stage and sum the squared output over a
    /// window long enough for `0.6^n` to have decayed below machine
    /// epsilon; the total must reproduce the impulse's own energy (`1.0`).
    /// This is a property every correct allpass section has, checked from
    /// first principles (Parseval), not a re-run of [`AllpassStage::process`]
    /// against itself.
    #[test]
    fn single_stage_is_energy_preserving() {
        let mut stage = AllpassStage::new(5, 0.6);
        let mut energy = 0.0;
        for n in 0..4000 {
            let x = if n == 0 { 1.0 } else { 0.0 };
            let y = stage.process(x);
            energy += y * y;
        }
        assert!((energy - 1.0).abs() < 1e-9, "energy = {energy}");
    }

    /// Falsification: a gain of `0.0` collapses the section to a pure delay
    /// (`w = x`, `y = w_delayed = x[n-M]`), which is trivially
    /// energy-preserving for a different, checkable reason — confirms the
    /// oracle above is not vacuously true for every input.
    #[test]
    fn zero_gain_is_a_pure_delay() {
        let mut stage = AllpassStage::new(3, 0.0);
        let input = [1.0, 2.0, -3.0, 0.5, 4.0, -1.0, 0.0, 2.0];
        let mut out = Vec::new();
        for x in input {
            out.push(stage.process(x));
        }
        assert!(out.first().copied().unwrap_or(1.0).abs() < 1e-12);
        assert!((out.get(3).copied().unwrap_or(0.0) - 1.0).abs() < 1e-12);
    }

    /// Two channels seeded differently must not draw the same cascade of
    /// delay lengths — this is the entire point of the filter (decorrelating
    /// channels from each other), so an implementation that accidentally
    /// reused one RNG stream for every channel would fail this immediately.
    /// Compares the delay lengths directly rather than a short run of output
    /// samples: with delays drawn from 5-500 samples, two cascades can
    /// legitimately agree on their first several output samples (the longer
    /// delay simply has not produced anything yet) without agreeing at all
    /// on their parameters, so a short output-sample comparison is not a
    /// reliable falsifier here.
    #[test]
    fn different_channels_decorrelate_differently() {
        let a = Cascade::new(6, 12345, 0);
        let b = Cascade::new(6, 12345, 1);
        let delays_a: Vec<usize> = a.0.iter().map(|s| s.delay.len()).collect();
        let delays_b: Vec<usize> = b.0.iter().map(|s| s.delay.len()).collect();
        assert_ne!(delays_a, delays_b);
    }

    #[test]
    fn resolve_seed_negative_uses_fallback() {
        assert_eq!(resolve_seed(-1, 99), 99);
        assert_eq!(resolve_seed(5, 99), 5);
    }
}
