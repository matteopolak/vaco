//! Shared dynamics-processing math: envelope detection and the static
//! compressor/expander curve `acompressor`/`sidechaincompress` and
//! `agate`/`sidechaingate` are both built from.
//!
//! Unlike `vaco-filter-audio-eq`'s biquad cookbook, there is no single named
//! specification for a feed-forward dynamics processor the way there is for
//! an RBJ biquad — this is standard, widely-documented DSP practice (the
//! same shape any audio DSP textbook's compressor chapter describes: an
//! envelope follower feeding a static gain-computer curve), not a port of
//! any particular implementation. The options this module reads
//! (`threshold`, `ratio`, `attack`, `release`, `knee`, `makeup`, `range`,
//! `detection`, `link`) are the reference's own (`ffmpeg -h
//! filter=acompressor`/`agate`, 2026-08-23) so the *units* match even though
//! the internal curve is this crate's own construction.
//!
//! # What is verified
//!
//! `ratio=1` (ratio disabled) and a below-threshold signal are both
//! independently checked to be gain-computer identities — see
//! `tests::ratio_one_is_identity` and `tests::below_threshold_is_untouched`.
//! These are real properties of the gain-computer curve, not a
//! re-transcription of its formula: the assertion is about output equalling
//! input, computed from first principles, not about intermediate
//! coefficients.

/// Detector mode: how the envelope reads the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Detection {
    Peak,
    Rms,
}

/// Multi-channel envelope linking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Link {
    Average,
    Maximum,
}

/// Downward (compressor/gate) or upward (expander) direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Downward,
    Upward,
}

/// A one-pole envelope follower with independent attack/release time
/// constants, the standard feed-forward detector.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Envelope {
    level: f64,
}

impl Envelope {
    /// `attack_ms`/`release_ms` become per-sample coefficients
    /// `1 - exp(-1 / (time_s * sample_rate))`, the usual RC-charge mapping
    /// from a time constant to a one-pole coefficient.
    pub(crate) fn coeff(time_ms: f64, sample_rate: f64) -> f64 {
        let time_s = (time_ms / 1000.0).max(1e-6);
        let samples = (time_s * sample_rate).max(1.0);
        1.0 - (-1.0 / samples).exp()
    }

    /// Advance the envelope by one sample of `input_magnitude` (already
    /// peak- or RMS-rectified), using `attack` when rising and `release`
    /// when falling.
    pub(crate) fn step(&mut self, input_magnitude: f64, attack: f64, release: f64) -> f64 {
        let coeff = if input_magnitude > self.level {
            attack
        } else {
            release
        };
        self.level += (input_magnitude - self.level) * coeff;
        if self.level.is_finite() {
            self.level
        } else {
            0.0
        }
    }
}

/// The static gain-computer curve: given a detector level and the
/// threshold/ratio/knee/mode, how many dB of gain to apply (before makeup).
///
/// The soft-knee shape is Giannoulis, Massberg & Reiss, "Digital Dynamic
/// Range Compressor Design — A Tutorial and Analysis" (J. Audio Eng. Soc.,
/// 2012) — a citable, independent source for the quadratic-knee formula,
/// not a transcription of any implementation. Using `d` for the signed
/// overshoot (positive once the curve should act) and `W` for the knee
/// width in dB:
///
/// * `2d <= -W`: no change (below the knee).
/// * `2|d| <= W`: `y = d + (1/ratio - 1) * (d + W/2)^2 / (2W)` (quadratic
///   knee).
/// * `2d > W`: `y = d / ratio` (full compression).
///
/// `gain_db = y - d`. `Mode::Upward` mirrors the overshoot around the
/// threshold (`d = threshold - level` instead of `level - threshold`) so the
/// same curve expands/gates below the threshold instead of compressing
/// above it, then mirrors the sign of the result back.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Curve {
    pub threshold_db: f64,
    pub ratio: f64,
    pub knee_db: f64,
    pub mode: Mode,
}

impl Curve {
    /// Gain (dB, always `<= 0` for downward and `>= 0` for upward) for a
    /// detector reading of `level_db`.
    pub(crate) fn gain_db(&self, level_db: f64) -> f64 {
        if !level_db.is_finite() {
            return 0.0;
        }
        let ratio = if self.ratio.is_finite() && self.ratio > 0.0 {
            self.ratio
        } else {
            1.0
        };
        let knee = self.knee_db.max(0.0);
        let d = match self.mode {
            Mode::Downward => level_db - self.threshold_db,
            Mode::Upward => self.threshold_db - level_db,
        };
        let y = if 2.0 * d <= -knee {
            d
        } else if knee > 0.0 && 2.0 * d.abs() <= knee {
            d + (1.0 / ratio - 1.0) * (d + knee / 2.0).powi(2) / (2.0 * knee)
        } else {
            d / ratio
        };
        let reduction = y - d;
        match self.mode {
            Mode::Downward => reduction.min(0.0),
            Mode::Upward => (-reduction).max(0.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio_one_is_identity() {
        let c = Curve {
            threshold_db: -20.0,
            ratio: 1.0,
            knee_db: 0.0,
            mode: Mode::Downward,
        };
        for level in [-40.0, -20.0, -10.0, 0.0] {
            assert!(
                (c.gain_db(level)).abs() < 1e-9,
                "level {level}: gain {}",
                c.gain_db(level)
            );
        }
    }

    #[test]
    fn below_threshold_is_untouched_downward() {
        let c = Curve {
            threshold_db: -10.0,
            ratio: 4.0,
            knee_db: 0.0,
            mode: Mode::Downward,
        };
        assert!(c.gain_db(-40.0).abs() < 1e-12);
    }

    #[test]
    fn above_threshold_is_untouched_upward() {
        let c = Curve {
            threshold_db: -10.0,
            ratio: 4.0,
            knee_db: 0.0,
            mode: Mode::Upward,
        };
        assert!(c.gain_db(0.0).abs() < 1e-12);
    }

    #[test]
    fn downward_reduces_gain_above_threshold() {
        let c = Curve {
            threshold_db: -10.0,
            ratio: 4.0,
            knee_db: 0.0,
            mode: Mode::Downward,
        };
        assert!(c.gain_db(10.0) < 0.0);
    }

    #[test]
    fn envelope_reaches_a_constant_input() {
        let mut e = Envelope::default();
        let attack = Envelope::coeff(1.0, 48_000.0);
        for _ in 0..48_000 {
            e.step(0.5, attack, attack);
        }
        assert!(
            (e.level - 0.5).abs() < 1e-3,
            "envelope settled at {}",
            e.level
        );
    }

    proptest::proptest! {
        #[test]
        fn gain_db_is_always_finite(
            level_db in -200.0f64..200.0,
            threshold_db in -200.0f64..200.0,
            ratio in -10.0f64..100.0,
            knee_db in -10.0f64..100.0,
        ) {
            for mode in [Mode::Downward, Mode::Upward] {
                let c = Curve { threshold_db, ratio, knee_db, mode };
                proptest::prop_assert!(c.gain_db(level_db).is_finite());
            }
        }
    }
}
