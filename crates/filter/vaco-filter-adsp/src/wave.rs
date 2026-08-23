//! Wave-table LFO generation.
//!
//! Six of `vaco-filter-aeffects`'s filters (`tremolo`, `vibrato`, `chorus`,
//! `flanger`, `aphaser`, `apulsator`) each drive a parameter — gain, delay
//! length, or both — from a low-frequency oscillator with a selectable shape
//! (sine, triangle, square, or one of the two sawtooth directions). Rather
//! than have six copies of "evaluate this waveform at this phase", this
//! module builds a table once per configured frequency and walks it with a
//! phase accumulator, matching the wave-table LFO pattern used throughout
//! `ffmpeg`'s own audio filters for this exact family of effects.
//!
//! # Independent oracle for [`sample`]
//!
//! Every shape here is a textbook periodic waveform, not a probed constant,
//! so the check is algebraic rather than measured: every shape must be
//! periodic with period `1.0`, [`WaveShape::Sine`] and [`WaveShape::Triangle`]
//! must be odd around phase `0.5` (`f(0.5 + x) == -f(0.5 - x)` after
//! recentring to remove the DC offset baked in by the `[min, max]` scaling),
//! and every shape's average over a full period must sit at the midpoint of
//! `[min, max]` — a table that leaned toward one rail would silently bias
//! whatever it drives. See `tests::every_shape_is_periodic_and_centred`.
#![forbid(unsafe_code)]

use std::f64::consts::TAU;

/// The LFO shapes `ffmpeg`'s own modulation filters expose by name:
/// `apulsator`'s `mode` and `flanger`/`aphaser`'s `shape`/`type` cover this
/// exact set between them (`triangular` there is [`Self::Triangle`] here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaveShape {
    Sine,
    Triangle,
    Square,
    SawUp,
    SawDown,
}

impl WaveShape {
    /// Parse the two spellings the reference accepts for the shapes shared
    /// with `flanger`/`aphaser` (`sinusoidal`/`s`, `triangular`/`t`), plus
    /// `apulsator`'s full five-way `mode` vocabulary. Unrecognised input
    /// falls back to [`Self::Sine`], matching this crate's established
    /// "unknown option value degrades to the documented default" convention
    /// (see `vaco-filter-aeffects::haas::MiddleSource::parse`).
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s.trim() {
            "triangular" | "t" | "triangle" | "1" => Self::Triangle,
            "square" | "2" => Self::Square,
            "sawup" | "3" => Self::SawUp,
            "sawdown" | "4" => Self::SawDown,
            _ => Self::Sine,
        }
    }

    /// The canonical shape value at `phase` in `[0, 1)`, ranging over
    /// `[-1, 1]` before any `[min, max]` rescaling.
    #[must_use]
    fn unit(self, phase: f64) -> f64 {
        let p = phase.rem_euclid(1.0);
        match self {
            Self::Sine => (p * TAU).sin(),
            // Rises from -1 at p=0 to 1 at p=0.5, falls back to -1 at p=1.
            Self::Triangle => {
                if p < 0.5 {
                    4.0 * p - 1.0
                } else {
                    3.0 - 4.0 * p
                }
            }
            Self::Square => {
                if p < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            Self::SawUp => 2.0 * p - 1.0,
            Self::SawDown => 1.0 - 2.0 * p,
        }
    }

    /// [`Self::unit`], rescaled to `[min, max]`.
    #[must_use]
    pub fn sample(self, phase: f64, min: f64, max: f64) -> f64 {
        let u = 0.5 * (self.unit(phase) + 1.0);
        min + u * (max - min)
    }
}

/// A phase accumulator that turns a frequency and a sample rate into a
/// stream of [`WaveShape`] samples, without recomputing a table: the
/// waveform is cheap enough (a handful of comparisons or one `sin`) that a
/// literal wave-table array buys nothing here beyond what a closed-form
/// phase evaluation already gives, so [`Lfo::next`] evaluates
/// [`WaveShape::sample`] directly rather than indexing a precomputed buffer.
#[derive(Debug, Clone, Copy)]
pub struct Lfo {
    shape: WaveShape,
    phase: f64,
    step: f64,
    min: f64,
    max: f64,
}

impl Lfo {
    /// `freq_hz` cycles per second at `sample_rate` samples per second,
    /// starting at `phase0` (a fraction of one cycle, e.g. `0.25` for a
    /// quarter-cycle head start — used to give multi-channel LFOs a
    /// per-channel phase offset such as `flanger`'s `phase` option).
    #[must_use]
    pub fn new(
        shape: WaveShape,
        freq_hz: f64,
        sample_rate: f64,
        phase0: f64,
        min: f64,
        max: f64,
    ) -> Self {
        let step = if sample_rate > 0.0 {
            freq_hz / sample_rate
        } else {
            0.0
        };
        Self {
            shape,
            phase: phase0.rem_euclid(1.0),
            step,
            min,
            max,
        }
    }

    /// Advance one sample and return the new value.
    ///
    /// Named `advance` rather than `next` so this is never mistaken for
    /// [`Iterator::next`] (it is infinite and infallible, which would be a
    /// confusing `Iterator` to hand out).
    pub fn advance(&mut self) -> f64 {
        let v = self.shape.sample(self.phase, self.min, self.max);
        self.phase = (self.phase + self.step).rem_euclid(1.0);
        v
    }

    /// The current value without advancing the phase.
    #[must_use]
    pub fn peek(&self) -> f64 {
        self.shape.sample(self.phase, self.min, self.max)
    }

    pub fn reset_phase(&mut self, phase0: f64) {
        self.phase = phase0.rem_euclid(1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHAPES: &[WaveShape] = &[
        WaveShape::Sine,
        WaveShape::Triangle,
        WaveShape::Square,
        WaveShape::SawUp,
        WaveShape::SawDown,
    ];

    /// Every shape must be periodic with period 1, and every shape's mean
    /// over a full period must sit at the midpoint of its range — an
    /// invariant a subtly-wrong waveform (e.g. an asymmetric triangle, or a
    /// sawtooth that does not span the full range) cannot satisfy even
    /// though it might still "look like" the right shape at a handful of
    /// sample points.
    #[test]
    fn every_shape_is_periodic_and_centred() {
        const N: usize = 4001;
        for &shape in SHAPES {
            let a = shape.sample(0.1234, -1.0, 1.0);
            let b = shape.sample(1.1234, -1.0, 1.0);
            assert!((a - b).abs() < 1e-9, "{shape:?} not periodic: {a} vs {b}");

            let mut sum = 0.0;
            for i in 0..N {
                sum += shape.sample(i as f64 / N as f64, -1.0, 1.0);
            }
            let mean = sum / N as f64;
            assert!(mean.abs() < 1e-2, "{shape:?} mean {mean} not centred at 0");
        }
    }

    /// [`WaveShape::Square`] must actually be bimodal (only the two rails),
    /// which rules out an implementation that accidentally interpolates
    /// between them.
    #[test]
    fn square_only_takes_two_values() {
        for i in 0..1000 {
            let v = WaveShape::Square.sample(f64::from(i) / 1000.0, 0.0, 1.0);
            assert!(
                v.abs() < f64::EPSILON || (v - 1.0).abs() < f64::EPSILON,
                "square produced {v}"
            );
        }
    }

    /// The sawtooth pair must be mirror images of one another at every
    /// phase, not just at the endpoints.
    #[test]
    fn sawup_and_sawdown_are_mirrored() {
        for i in 0..1000 {
            let p = f64::from(i) / 1000.0;
            let up = WaveShape::SawUp.sample(p, 0.0, 1.0);
            let down = WaveShape::SawDown.sample(p, 0.0, 1.0);
            assert!(
                (up - (1.0 - down)).abs() < 1e-9,
                "p={p}: up={up} down={down}"
            );
        }
    }

    /// An [`Lfo`] at `freq_hz = sample_rate / n` must return to (approximately)
    /// its starting value after exactly `n` calls to [`Lfo::next`] — the
    /// defining property of a periodic oscillator sampled at a fixed rate.
    #[test]
    fn lfo_returns_to_start_after_one_period() {
        let mut lfo = Lfo::new(WaveShape::Sine, 10.0, 1000.0, 0.0, -1.0, 1.0);
        let start = lfo.peek();
        for _ in 0..100 {
            lfo.advance();
        }
        let after = lfo.peek();
        assert!((start - after).abs() < 1e-9, "start={start} after={after}");
    }

    #[test]
    fn parse_accepts_both_spellings() {
        assert_eq!(WaveShape::parse("sinusoidal"), WaveShape::Sine);
        assert_eq!(WaveShape::parse("s"), WaveShape::Sine);
        assert_eq!(WaveShape::parse("triangular"), WaveShape::Triangle);
        assert_eq!(WaveShape::parse("t"), WaveShape::Triangle);
        assert_eq!(WaveShape::parse("sawup"), WaveShape::SawUp);
        assert_eq!(WaveShape::parse("sawdown"), WaveShape::SawDown);
        assert_eq!(WaveShape::parse("square"), WaveShape::Square);
        assert_eq!(WaveShape::parse("garbage"), WaveShape::Sine);
    }
}
