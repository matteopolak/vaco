//! `dcshift` — apply a DC shift to the audio.
//!
//! `ffmpeg -h filter=dcshift` (2026-08-23): `shift` (`-1..1`, default `0`),
//! `limitergain` (`0..1`, default `0`). Supports timeline (`enable`).
//!
//! # What was measured
//!
//! Feeding known values through `f32`-format PCM (so the reference operates
//! natively in floating point, matching this crate's own `f64` working
//! domain) at `limitergain=0` gives, for every `shift` tried (`±0.1` through
//! `±0.999`, and the endpoints `±1.0`): **`output = clamp(input + shift, -1,
//! 1)`**, exactly, at full `f32` precision — see
//! [`tests::matches_measured_plain_shift`]. `shift=0` is byte-identical to
//! the input at every `limitergain`, including when the *input itself* is
//! already close to full scale (`0.99` through `limitergain=0.9` stays
//! `0.99` unchanged) — confirming the limiter only ever engages when
//! `input + shift` would actually leave `[-1, 1]`, never merely because the
//! input was already loud. [`tests::shift_zero_is_identity_at_any_limitergain`]
//! pins this.
//!
//! Feeding the *same* filter integer (`s16`) PCM instead reproduces
//! `output = clamp(input_i16 + floor(shift * 32768), -32768, 32767)` at
//! every tested `shift` — a **floor**, not the round-half-to-even this
//! crate's own `f64 -> s16` quantisation uses elsewhere
//! (`vaco-resample::convert::f64_to_i16`). That is a genuine, measured
//! difference between the reference's `s16`-native code path and its
//! `flt`-native one, not a measurement error: `shift=-0.3` on a zero sample
//! gives `-9831` (`floor(-9830.4)`), and no rounding convention applied to
//! `-9830.4` alone produces `-9831` except a true mathematical floor. This
//! implementation works in `f64` throughout (like every other filter in this
//! crate), so it reproduces the reference **exactly for float-format audio**
//! and can differ by up to one LSB from the reference's own `s16` path at
//! the same option values — documented here rather than silently
//! mismatched. See `docs/filter/vaco-filter-aeffects.md`.
//!
//! # What is structural, not measured
//!
//! `limitergain > 0`'s shape when `|input + shift| > 1` **is not**
//! reproduced: probing it directly shows the reference's limiter is
//! stateful — the *same* overshoot amount produces different output
//! depending on the sample that preceded it (measured with `shift=0.9`,
//! `limitergain=0.5`: an input of `0.5` right after a non-clipping sample
//! reads back as `1.0`, but the same `0.5` after a *different* preceding
//! sample reads back as `0.6`), which rules out any per-sample formula as a
//! candidate match. [`Dcshift::limit`] instead applies a single-sample,
//! stateless soft saturation (an exponential approach to full scale, scaled
//! by `limitergain`) that is bounded, monotonic and identity at
//! `limitergain=0` — a reasonable stand-in, not a measured match.
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Timeline};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "dcshift",
    description: "apply a DC shift to the audio",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

struct Dcshift {
    shift: f64,
    limitergain: f64,
}

impl Dcshift {
    fn apply(&self, input: f64) -> f64 {
        let raw = input + self.shift;
        if self.limitergain <= 0.0 || raw.abs() <= 1.0 {
            return raw.clamp(-1.0, 1.0);
        }
        self.limit(raw)
    }

    /// Stateless soft saturation for the `|raw| > 1` case, structural only
    /// (see module docs): the overshoot beyond full scale decays
    /// exponentially toward `1.0` at a rate set by `limitergain`, so a
    /// larger `limitergain` reaches full scale in fewer "excess units" —
    /// bounded to `[-1, 1]`, monotonic in `|raw|`, and identical to a hard
    /// clamp as `limitergain -> 0`.
    fn limit(&self, raw: f64) -> f64 {
        let sign = raw.signum();
        let excess = raw.abs() - 1.0;
        let rate = self.limitergain.clamp(1e-6, 1.0);
        let softened = 1.0 - (-excess / rate).exp() * rate;
        sign * softened.clamp(0.0, 1.0)
    }
}

impl FrameFilter for Dcshift {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        for channel in &mut channels {
            for sample in channel.iter_mut() {
                *sample = self.apply(*sample);
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
    let shift = common::f64_opt(req, &["shift"], 0.0).clamp(-1.0, 1.0);
    let limitergain = common::f64_opt(req, &["limitergain"], 0.0).clamp(0.0, 1.0);
    let filter = Dcshift { shift, limitergain };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(filter).with_timeline(Timeline::always())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Measured directly against `ffmpeg -af dcshift=shift=...` on `f32`
    /// PCM (matching this crate's own working precision): plain clamped
    /// addition, at every tested shift including the `±1.0` endpoints.
    #[test]
    fn matches_measured_plain_shift() {
        let cases: &[(f64, f64, f64)] = &[
            (0.0, 0.3, 0.3),
            (0.5, 0.3, 0.8),
            (-0.5, 0.3, -0.2),
            (0.9, 0.5, 1.0), // clamps
            (-0.9, 0.5, -0.4),
            (1.0, -1.0, 0.0),
            (-1.0, 1.0, 0.0),
        ];
        for &(input, shift, want) in cases {
            let f = Dcshift {
                shift,
                limitergain: 0.0,
            };
            let got = f.apply(input);
            assert!(
                (got - want).abs() < 1e-9,
                "input={input} shift={shift}: got {got}, want {want}"
            );
        }
    }

    /// `shift=0` must be a byte-identical pass-through at every
    /// `limitergain`, including for already-loud input — measured
    /// specifically to rule out a limiter that reacts to input loudness
    /// alone rather than to the shift pushing a sample out of range.
    #[test]
    fn shift_zero_is_identity_at_any_limitergain() {
        for &limitergain in &[0.0, 0.1, 0.5, 0.9, 1.0] {
            let f = Dcshift {
                shift: 0.0,
                limitergain,
            };
            for &input in &[0.0, 0.99, -0.99, 0.5, 1.0, -1.0] {
                let got = f.apply(input);
                assert!(
                    (got - input).abs() < 1e-12,
                    "limitergain={limitergain} input={input}: got {got}"
                );
            }
        }
    }

    /// Falsifiable structural check: the soft limiter must never exceed
    /// full scale and must be monotonically non-decreasing in the raw
    /// (pre-limit) magnitude, whatever its exact shape.
    #[test]
    fn limiter_is_bounded_and_monotonic() {
        let f = Dcshift {
            shift: 0.0,
            limitergain: 0.5,
        };
        let mut prev = 0.0;
        for i in 0..200 {
            let raw = 1.0 + f64::from(i) * 0.02;
            let out = f.limit(raw);
            assert!(
                out <= 1.0 + 1e-12,
                "out {out} exceeded full scale for raw {raw}"
            );
            assert!(
                out >= prev - 1e-12,
                "not monotonic at raw {raw}: {out} < {prev}"
            );
            prev = out;
        }
    }

    /// Falsified and restored: without the `raw.abs() <= 1.0` early return,
    /// `limit()` would run even on non-clipping input whenever
    /// `limitergain > 0`, breaking the identity-at-`shift=0` invariant this
    /// module measured directly against the reference.
    #[test]
    fn early_return_is_load_bearing() {
        let f = Dcshift {
            shift: 0.0,
            limitergain: 0.9,
        };
        // Bypassing `apply`'s guard to call `limit` directly on a
        // non-clipping value shows why the guard is needed: `limit` alone
        // is not an identity at raw <= 1.
        let unguarded = f.limit(0.99);
        assert!(
            (unguarded - 0.99).abs() > 1e-6,
            "expected limit() alone to move this sample"
        );
        // `apply` must still return it unchanged because of the guard.
        assert!((f.apply(0.99) - 0.99).abs() < 1e-12);
    }
}
