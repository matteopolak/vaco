//! `acrusher` — reduce audio bit resolution.
//!
//! `ffmpeg -h filter=acrusher` (2026-08-27): `level_in`/`level_out`
//! (0.015625 to 64, default 1), `bits` (1 to 64, default 8), `mix` (0 to 1,
//! default 0.5), `mode` (`lin`=0 default, `log`=1), `dc` (0.25 to 4, default
//! 1), `aa` (0 to 1, default 0.5), `samples` (1 to 250, default 1), `lfo`
//! (bool, default false), `lforange`/`lforate`.
//!
//! # Measured, at `dc=1, aa=0, samples=1, mode=lin` — the part that is exact
//!
//! Built a dense ramp, ran it through the reference at each `bits` from 1 to
//! 8 with `mix=0` (pure "wet"), and counted distinct output levels: a
//! `2^bits - 1` step, giving `2^(bits+1) - 1` distinct levels from `-1` to
//! `1` inclusive — confirmed for `bits` in `{1,2,3,4,8}`. That pins the
//! quantiser exactly: `wet = round(x * L) / L` with `L = 2^bits - 1`, where
//! `round` is round-half-away-from-zero (confirmed at the `x = +-0.5, bits =
//! 1` edge: `-0.5` maps to `-1` and `+0.5` maps to `+1`, matching
//! `f64::round`'s own tie-breaking, not round-half-to-even).
//!
//! `mix` was the second surprise: `mix=1` reproduces the **dry** signal
//! (scaled by `level_in`) and `mix=0` is pure **wet**, the opposite of the
//! usual "0 = dry, 1 = wet" convention — measured directly (`mix=0.25`
//! matches `0.25*dry + 0.75*wet` to the last bit, `mix=0.75` matches
//! `0.75*dry + 0.25*wet`), not assumed from the option's name. `level_in`
//! was confirmed to scale the dry path too (`mix=1, level_in=2` doubles the
//! passthrough signal exactly), so both paths read from `x * level_in`.
//!
//! # Measured, and left as a documented gap
//!
//! `dc != 1` is not a simple bias or scale on the quantiser: probing the
//! same ramp at `dc=2` produces an asymmetric grid (a `[-1, 0.25]` dead band
//! mapping entirely to `0`, then even `0.5`-spaced steps above it) that does
//! not fit any bias/scale/clamp combination tried against the `dc=1` formula.
//! `aa != 0` (the reference's own *default* is `0.5`) replaces the hard
//! staircase with a smooth curve — `aa=1, bits=1` gives a continuous
//! S-shaped function of `x`, not `round(x)/1` at all. `samples > 1` (sample
//! reduction / hold) did not show the expected "N identical consecutive
//! outputs" pattern in a `bits=8` probe, so its exact effect is unresolved
//! too. `mode=log` and `lfo`/`lforange`/`lforate` were not probed at all.
//!
//! Every one of `dc`, `aa` (beyond `0`), `samples` (beyond `1`), `mode=log`
//! and the LFO family is therefore accepted as an option (so a filtergraph
//! string setting them does not fail to parse) but has **no effect** here —
//! an honest no-op is preferable to a plausible-looking curve invented to
//! fill the gap, per this project's standing rule against shipping a guessed
//! formula. The default configuration (`aa=0.5`) is consequently *not*
//! reproduced faithfully; only `bits`/`mix`/`level_in`/`level_out` at
//! `aa=0, dc=1, samples=1, mode=lin` are.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "acrusher",
    description: "reduce audio bit resolution",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

/// `round(x * levels) / levels`, `levels = 2^bits - 1` — the measured
/// quantiser (see this module's doc). `f64::round` is round-half-away-from-
/// zero, matching the reference's own tie-breaking at `x = +-0.5, bits = 1`.
fn quantise(x: f64, bits: f64) -> f64 {
    let levels = (2f64.powf(bits.clamp(1.0, 64.0)) - 1.0).max(1.0);
    (x * levels).round() / levels
}

#[derive(Debug, Clone)]
struct Crusher {
    level_in: f64,
    level_out: f64,
    bits: f64,
    mix: f64,
}

impl FrameFilter for Crusher {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        for ch in &mut channels {
            for s in ch.iter_mut() {
                let dry = *s * self.level_in;
                let wet = quantise(dry, self.bits) * self.level_out;
                // Measured: `mix=1` is dry, `mix=0` is wet (see module doc).
                *s = self.mix.mul_add(dry - wet, wet);
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
    let filter = Crusher {
        level_in: common::f64_opt(req, &["level_in"], 1.0),
        level_out: common::f64_opt(req, &["level_out"], 1.0),
        bits: common::f64_opt(req, &["bits"], 8.0),
        mix: common::f64_opt(req, &["mix"], 0.5),
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

    /// Measured: `bits=1` has exactly three levels, `{-1, 0, 1}`.
    #[test]
    fn bits_one_has_three_levels() {
        let mut levels = std::collections::BTreeSet::new();
        let mut x = -1.0;
        while x <= 1.0 {
            levels.insert((quantise(x, 1.0) * 1e9).round() as i64);
            x += 0.001;
        }
        assert_eq!(levels.len(), 3, "{levels:?}");
    }

    /// Measured: `bits=8` has `2^9 - 1 = 511`... no — `2*(2^8-1)+1 = 511`
    /// distinct levels from `-1` to `1`. Cross-checked against the formula
    /// `2^(bits+1) - 1` the module doc states.
    #[test]
    fn level_count_matches_two_to_the_bits_plus_one_minus_one() {
        for bits in [1.0, 2.0, 3.0, 4.0] {
            let mut levels = std::collections::BTreeSet::new();
            let mut x = -1.0;
            while x <= 1.0 {
                levels.insert((quantise(x, bits) * 1e9).round() as i64);
                x += 0.0005;
            }
            #[allow(
                clippy::cast_possible_truncation,
                reason = "bits is a small option value, never near i64/f64 boundary"
            )]
            let want = (2f64.powf(bits + 1.0) - 1.0).round() as usize;
            assert_eq!(levels.len(), want, "bits={bits}: {levels:?}");
        }
    }

    /// Measured tie-break: `round-half-away-from-zero`, not to-even.
    #[test]
    fn half_rounds_away_from_zero() {
        assert!((quantise(-0.5, 1.0) - -1.0).abs() < 1e-12);
        assert!((quantise(0.5, 1.0) - 1.0).abs() < 1e-12);
        assert!(quantise(0.499_999_9, 1.0).abs() < 1e-12);
    }

    /// Measured mix blend: `mix=0.25` matches `0.25*dry + 0.75*wet` (checked
    /// against the reference at `bits=1, dc=1, aa=0`: input `-0.999` ->
    /// output `-0.99975`). `filter_frame`'s blend line is exactly this
    /// expression; `FilterContext` cannot be constructed outside
    /// `vaco-filter-core` in a unit test, so the arithmetic is checked
    /// directly rather than through the adapter.
    #[test]
    fn mix_blends_dry_and_wet_in_the_measured_direction() {
        let dry = -0.999;
        let wet = quantise(dry, 1.0);
        let mix: f64 = 0.25;
        let got = mix.mul_add(dry - wet, wet);
        assert!((got - -0.999_75).abs() < 1e-9, "{got}");
    }
}
