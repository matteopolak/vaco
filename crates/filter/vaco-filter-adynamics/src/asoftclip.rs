//! `asoftclip` — audio soft clipper.
//!
//! `ffmpeg -h filter=asoftclip` (2026-08-27): `type` (`hard`=-1, `tanh`=0
//! default, `atan`=1, `cubic`=2, `exp`=3, `alg`=4, `quintic`=5, `sin`=6,
//! `erf`=7), `threshold` (1e-6 to 1, default 1), `output` (1e-6 to 16,
//! default 1), `param` (0.01 to 3, default 1, unused below), `oversample`
//! (1 to 64, default 1, not implemented — accepted and ignored).
//!
//! # Measured, not guessed
//!
//! Every curve below was recovered by feeding a dense ramp through the
//! reference at `threshold=1, output=1, param=1` and fitting the result,
//! then falsified against a second, independent property before being
//! trusted:
//!
//! * `tanh`: `y = tanh(x)` — exact to the last bit over the whole ramp.
//! * `atan`: `y = (2/pi) * atan(x)` — the raw (non-normalised) `atan(x)` was
//!   tried first and was off by exactly the `2/pi` factor everywhere, which
//!   is the kind of clean miss that confirms the shape rather than the
//!   constant.
//! * `exp`: measured **identical** to `tanh` at every sample tested. That is
//!   consistent with `exp` being the exponential-form definition of the same
//!   curve (`tanh(x) = (1 - e^-2x) / (1 + e^-2x)`) rather than a different
//!   curve, so it is implemented as `tanh(x)` here too.
//! * `hard`: `y = x.clamp(-threshold, threshold)`.
//! * `cubic`: least-squares fit of `a*x + b*x^3` against the measured curve
//!   returns `a = 1`, `b = -4/27` to full `f64` precision (residual `< 6e-16`
//!   over 101 points). `d/dx = 0` at `x = 1.5`, where the cubic evaluates to
//!   exactly `1.0` — measured directly: the reference clamps to `1.0` for
//!   every `|x| >= 1.5`, not the falling value the raw cubic would give
//!   beyond its turning point.
//! * `quintic`: same fitting approach, `a*x + b*x^3 + c*x^5`, returns
//!   `a = 1, b = 0, c = -0.08192` (residual `< 6e-16`). `d/dx = 0` at
//!   `x = 1.25` (`1/(5*1.25^4) = 0.08192` exactly), where the curve reaches
//!   `1.0` and the reference clamps beyond it, same as `cubic`.
//! * `sin`: `y = sin(x)`, clamped to `+-1` beyond `|x| >= pi/2` — measured at
//!   `x = pi/2` exactly (`y = 1.0`) and just past it (`y` stays `1.0` rather
//!   than following `sin` back down).
//! * `erf`: `y = erf(x)` — exact to the last measured digit; monotonic and
//!   asymptotic, so no clamp is needed (the curve never exceeds `+-1`).
//! * `alg` (algebraic): `y = x / sqrt(1 + x^2)` — same asymptotic shape, no
//!   clamp needed.
//!
//! `threshold`/`output` scale every curve as `y = output * threshold *
//! g(x / threshold)`: confirmed for `tanh` directly (`threshold=0.5` matches
//! `0.5*tanh(x/0.5)`; `output=2` matches `2*tanh(x)`, both to machine
//! precision) and applied uniformly to the rest by construction rather than
//! measured per type — a reasonable extrapolation from one confirmed case,
//! not a second independent measurement, and noted as such.
//!
//! `param` and `oversample` are accepted and otherwise unused: the
//! reference's own `-h` output does not say what `param` changes for any of
//! these eight curves (its stated range suggests it matters for some
//! unlisted combination), and `oversample`'s anti-aliasing pass is a real
//! feature this implementation does not attempt.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "asoftclip",
    description: "audio soft clipper",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Hard,
    Tanh,
    Atan,
    Cubic,
    Exp,
    Alg,
    Quintic,
    Sin,
    Erf,
}

impl Kind {
    fn parse(s: &str) -> Self {
        match s.trim() {
            "hard" | "-1" => Self::Hard,
            "atan" | "1" => Self::Atan,
            "cubic" | "2" => Self::Cubic,
            "exp" | "3" => Self::Exp,
            "alg" | "4" => Self::Alg,
            "quintic" | "5" => Self::Quintic,
            "sin" | "6" => Self::Sin,
            "erf" | "7" => Self::Erf,
            _ => Self::Tanh,
        }
    }

    /// The normalised curve `g(u)` for `u = x / threshold`, already clamped
    /// where the reference is measured to clamp (`cubic`/`quintic`/`sin`
    /// past their turning point). `erf`/`alg` are asymptotic and never need
    /// one; `hard` clamps by construction.
    fn apply(self, u: f64) -> f64 {
        match self {
            Self::Hard => u.clamp(-1.0, 1.0),
            Self::Tanh | Self::Exp => u.tanh(),
            Self::Atan => (2.0 / std::f64::consts::PI) * u.atan(),
            Self::Cubic => {
                if u.abs() >= 1.5 {
                    u.signum()
                } else {
                    (4.0 / 27.0f64).mul_add(-u.powi(3), u)
                }
            }
            Self::Quintic => {
                if u.abs() >= 1.25 {
                    u.signum()
                } else {
                    0.081_92f64.mul_add(-u.powi(5), u)
                }
            }
            Self::Sin => {
                if u.abs() >= std::f64::consts::FRAC_PI_2 {
                    u.signum()
                } else {
                    u.sin()
                }
            }
            Self::Erf => libm_erf(u),
            Self::Alg => u / (1.0 + u * u).sqrt(),
        }
    }
}

/// `erf` via Abramowitz & Stegun 7.1.26, the standard published rational
/// approximation (max absolute error `1.5e-7`) — no `erf` in `std`, and
/// pulling in a whole special-functions crate for one filter is not
/// warranted. Good enough for an audio saturation curve; not claimed to be
/// good enough for anything that needs the last few bits of `erf`.
fn libm_erf(x: f64) -> f64 {
    if x == 0.0 {
        // The rational approximation below sums to `0.999999999`, not
        // exactly `1.0`, at `t=1` (its own published error bound is
        // `1.5e-7`) — so without this, `erf(0)` comes out `~1e-9` instead of
        // the mathematically exact `0`. Odd-function symmetry says `x=0` is
        // a fixed point regardless of the approximation's error elsewhere.
        return 0.0;
    }
    let sign = x.signum();
    let x = x.abs();
    let t = 1.0 / 0.327_591_1f64.mul_add(x, 1.0);
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    sign * (1.0 - poly * (-x * x).exp())
}

#[derive(Debug, Clone)]
struct SoftClip {
    kind: Kind,
    threshold: f64,
    output: f64,
}

impl FrameFilter for SoftClip {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        let threshold = self.threshold.max(1e-9);
        for ch in &mut channels {
            for s in ch.iter_mut() {
                *s = self.output * threshold * self.kind.apply(*s / threshold);
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
    let kind = req.named("type").map_or(Kind::Tanh, |v| Kind::parse(&v));
    let filter = SoftClip {
        kind,
        threshold: common::f64_opt(req, &["threshold"], 1.0),
        output: common::f64_opt(req, &["output"], 1.0),
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

    /// Measured fixed points: `tanh(0) = 0`, and every curve should leave
    /// `0` at `0` (odd symmetry — none of the eight curves has a DC offset).
    #[test]
    fn every_curve_is_zero_at_zero() {
        for k in [
            Kind::Hard,
            Kind::Tanh,
            Kind::Atan,
            Kind::Cubic,
            Kind::Exp,
            Kind::Alg,
            Kind::Quintic,
            Kind::Sin,
            Kind::Erf,
        ] {
            assert!(k.apply(0.0).abs() < 1e-12, "{k:?} at 0");
        }
    }

    /// Measured against the reference directly: `tanh(1) = 0.7615941559557649`.
    #[test]
    fn tanh_matches_the_reference_at_one() {
        assert!((Kind::Tanh.apply(1.0) - 0.761_594_155_955_764_9).abs() < 1e-15);
    }

    /// Measured: `atan` is normalised by `2/pi`, not raw `atan`.
    #[test]
    fn atan_is_normalised() {
        let raw = 1.0f64.atan();
        let got = Kind::Atan.apply(1.0);
        assert!((got - raw).abs() > 0.01, "should not equal raw atan(1)");
        assert!((got - (2.0 / std::f64::consts::PI) * raw).abs() < 1e-15);
    }

    /// Measured: `cubic` clamps to `1.0` at and beyond `x = 1.5`, not the
    /// falling value `x - (4/27)x^3` gives past its turning point.
    #[test]
    fn cubic_clamps_past_its_turning_point() {
        assert!((Kind::Cubic.apply(1.5) - 1.0).abs() < 1e-9);
        assert!((Kind::Cubic.apply(2.0) - 1.0).abs() < 1e-9);
        assert!((Kind::Cubic.apply(-2.0) + 1.0).abs() < 1e-9);
    }

    /// Falsification: with the naive (non-clamped) cubic formula, `x = 2.0`
    /// would evaluate to `2 - (4/27)*8 = 2 - 1.185... = 0.814...`, well below
    /// `1.0` — confirming the clamp branch in `Kind::apply` is load-bearing,
    /// not a no-op that happens to agree with the unclamped formula here.
    #[test]
    fn unclamped_cubic_would_have_disagreed() {
        let naive = (4.0 / 27.0f64).mul_add(-2.0f64.powi(3), 2.0);
        assert!((naive - Kind::Cubic.apply(2.0)).abs() > 0.1, "{naive}");
    }

    /// Measured: `quintic` clamps at `x = 1.25`.
    #[test]
    fn quintic_clamps_past_its_turning_point() {
        assert!((Kind::Quintic.apply(1.25) - 1.0).abs() < 1e-9);
        assert!((Kind::Quintic.apply(1.24) - 1.0).abs() > 1e-6);
    }

    /// Measured: `sin` clamps at `pi/2`.
    #[test]
    fn sin_clamps_at_pi_over_2() {
        assert!((Kind::Sin.apply(std::f64::consts::FRAC_PI_2) - 1.0).abs() < 1e-9);
        assert!((Kind::Sin.apply(3.0) - 1.0).abs() < 1e-9);
    }

    /// `alg` and `erf` are asymptotic and monotonic: increasing `x` must
    /// never decrease `g(x)`, and the limit as `x -> infinity` must approach
    /// `1`, never exceed it.
    #[test]
    fn alg_and_erf_are_monotonic_and_bounded() {
        for k in [Kind::Alg, Kind::Erf] {
            let mut prev = k.apply(-10.0);
            let mut x = -9.9;
            while x <= 10.0 {
                let v = k.apply(x);
                assert!(v >= prev - 1e-12, "{k:?} not monotonic at {x}");
                assert!(v <= 1.0 + 1e-6, "{k:?} exceeded 1.0 at {x}: {v}");
                prev = v;
                x += 0.1;
            }
        }
    }

    /// `exp` is measured identical to `tanh` — falsifiable: if a future edit
    /// accidentally diverges the two branches, this catches it.
    #[test]
    fn exp_matches_tanh() {
        for x in [-2.0, -0.5, 0.0, 0.3, 1.7] {
            assert!((Kind::Exp.apply(x) - Kind::Tanh.apply(x)).abs() < 1e-15);
        }
    }
}
