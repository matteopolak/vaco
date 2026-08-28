//! ITU-R BS.1770-4 "K-weighting": a two-stage pre-filter (a high shelf
//! modelling head diffraction, then a high-pass approximating the outer/
//! middle-ear response) applied to each channel before summing weighted
//! mean-square power into a loudness estimate. Shared by [`crate::ebur128`]
//! and [`crate::replaygain`], which both scan the same K-weighted, gated
//! loudness.
//!
//! # Provenance
//!
//! The two stages are the standard Robert Bristow-Johnson "Audio EQ
//! Cookbook" biquad designs — [`vaco_filter_adsp::biquad`] builds the
//! sections, this module only supplies the `(f0, Q, gain)` working point
//! ITU-R BS.1770-4 specifies for its K-weighting curve: a +4 dB high shelf
//! around 1.68 kHz cascaded with a high-pass around 38 Hz. Recomputing
//! coefficients from `(f0, Q, gain)` at the link's actual sample rate —
//! rather than hard-coding the reference's own printed 48 kHz coefficient
//! table — means this needs no internal resample step and is correct at
//! any input rate, not just 48 kHz.
//!
//! This module used to carry its own copy of the cookbook formulas and
//! biquad state, written before `vaco-filter-adsp::biquad` existed as a
//! shared, reusable home for them (D19). It now builds on that module
//! directly instead of duplicating it a fourth time.
//!
//! **Oracle**, per `docs/filter/vaco-filter-aanalysis.md`: not a second
//! transcription of the same coefficients. [`crate::ebur128`]'s tests check
//! the frequency-response *shape* this filter must have (attenuating well
//! below 38 Hz, near-unity around 1 kHz, +4 dB well above the shelf) against
//! the closed-form loudness formula `-0.691 + 10*log10(mean square)`, and a
//! calibrated sine at a computed amplitude must read the LUFS value that
//! formula predicts — a physical property, not a numerical coincidence with
//! this module's own design equations.

use vaco_filter_adsp::biquad::{Coeffs, State, WidthType, highpass, highshelf};

/// High-shelf design point (ITU-R BS.1770-4's "stage 1" pre-filter).
const SHELF_F0: f64 = 1681.9745;
const SHELF_Q: f64 = 0.707_175;
const SHELF_GAIN_DB: f64 = 3.999_844;

/// High-pass design point (the "RLB" weighting curve, BS.1770-4 "stage 2").
const HP_F0: f64 = 38.13547;
const HP_Q: f64 = 0.500_327;

/// One channel's K-weighting cascade.
#[derive(Debug, Clone, Copy)]
pub(crate) struct KWeight {
    shelf: Coeffs,
    hp: Coeffs,
    shelf_state: State,
    hp_state: State,
}

impl KWeight {
    pub(crate) fn new(fs: f64) -> Self {
        let fs = fs.max(1.0);
        Self {
            shelf: highshelf(fs, SHELF_F0, WidthType::QFactor, SHELF_Q, SHELF_GAIN_DB),
            hp: highpass(fs, HP_F0, WidthType::QFactor, HP_Q),
            shelf_state: State::default(),
            hp_state: State::default(),
        }
    }

    pub(crate) fn process(&mut self, x: f64) -> f64 {
        let y = self.shelf_state.process(&self.shelf, x);
        self.hp_state.process(&self.hp, y)
    }

    pub(crate) fn reset(&mut self) {
        self.shelf_state = State::default();
        self.hp_state = State::default();
    }
}

/// Channel weighting `G_i` (ITU-R BS.1770-4 Table 1): `1.0` for front and
/// centre channels, `~1.41` (+1.5 dB) for surround/side/back, `0` (excluded
/// entirely) for LFE.
pub(crate) fn channel_weight(ch: Option<vaco_chlayout::Channel>) -> f64 {
    use vaco_chlayout::Channel;
    match ch {
        Some(Channel::LowFrequency | Channel::LowFrequency2) => 0.0,
        Some(
            Channel::BackLeft
            | Channel::BackRight
            | Channel::BackCenter
            | Channel::SideLeft
            | Channel::SideRight
            | Channel::SurroundDirectLeft
            | Channel::SurroundDirectRight,
        ) => 1.41,
        _ => 1.0,
    }
}

/// `-0.691 + 10*log10(z)`, the BS.1770-4 loudness map from a (possibly
/// channel-weighted) mean square `z` to LUFS. `z <= 0` maps to `f64::MIN`
/// rather than `-inf`, so callers can compare it without special-casing
/// infinities.
pub(crate) fn loudness_from_z(z: f64) -> f64 {
    if z > 0.0 {
        -0.691 + 10.0 * z.log10()
    } else {
        f64::MIN
    }
}

#[cfg(test)]
mod tests {
    use super::{KWeight, loudness_from_z};

    /// The shelf pushes a high-frequency tone's response up: feed the same
    /// unit-amplitude sample count of a very-high-frequency-relative signal
    /// (approximated by an alternating +1/-1 sequence, i.e. Nyquist) versus
    /// a near-DC one, and the Nyquist case must come out louder in the
    /// K-weighted domain. This checks the filter's *shape*, not a
    /// coefficient table.
    #[test]
    fn high_frequencies_are_weighted_louder_than_low_ones() {
        let fs = 48_000.0;
        let mut hi = KWeight::new(fs);
        let mut lo = KWeight::new(fs);
        let mut hi_energy = 0.0;
        let mut lo_energy = 0.0;
        for n in 0..2000 {
            // Nyquist-rate alternation: the highest frequency representable.
            let x_hi = if n % 2 == 0 { 1.0 } else { -1.0 };
            // A very slow ramp-like low frequency component.
            let x_lo = (2.0 * std::f64::consts::PI * 20.0 * f64::from(n) / fs).sin();
            let y_hi = hi.process(x_hi);
            let y_lo = lo.process(x_lo);
            if n > 200 {
                hi_energy += y_hi * y_hi;
                lo_energy += y_lo * y_lo;
            }
        }
        assert!(
            hi_energy > lo_energy,
            "expected the near-Nyquist tone to read louder after K-weighting: \
             hi={hi_energy} lo={lo_energy}"
        );
    }

    #[test]
    fn reset_clears_filter_memory() {
        let mut kw = KWeight::new(48_000.0);
        for _ in 0..100 {
            kw.process(1.0);
        }
        kw.reset();
        // Immediately after reset, a single sample's response should match
        // a freshly constructed filter's response to the same sample.
        let mut fresh = KWeight::new(48_000.0);
        assert!((kw.process(0.5) - fresh.process(0.5)).abs() < 1e-12);
    }

    #[test]
    fn loudness_map_matches_the_closed_form() {
        // A mean square of 0.5 (e.g. a full-scale sine's) maps to
        // -0.691 + 10*log10(0.5), computed independently here.
        let expected = -0.691 + 10.0 * 0.5f64.log10();
        assert!((loudness_from_z(0.5) - expected).abs() < 1e-12);
    }
}
