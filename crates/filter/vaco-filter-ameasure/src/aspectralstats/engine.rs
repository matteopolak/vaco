//! Pure spectral-feature formulas over a magnitude spectrum.
//!
//! No FFT, no frame, no filter machinery — a function of `(magnitude,
//! frequency)` pairs (plus the previous frame's magnitude, for `flux`), so
//! the module doc's oracle claims can be checked directly against
//! synthetic spectra without decoding a single audio sample. See the
//! parent module's doc for the published (Peeters 2004) definitions this
//! implements and the one deliberate deviation (95% `rolloff`, since the
//! reference does not document its own percentage).

/// The thirteen named measures `aspectralstats` reports.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Measures {
    pub(crate) mean: f64,
    pub(crate) variance: f64,
    pub(crate) centroid: f64,
    pub(crate) spread: f64,
    pub(crate) skewness: f64,
    pub(crate) kurtosis: f64,
    pub(crate) entropy: f64,
    pub(crate) flatness: f64,
    pub(crate) crest: f64,
    pub(crate) flux: f64,
    pub(crate) slope: f64,
    pub(crate) decrease: f64,
    pub(crate) rolloff: f64,
}

/// Compute every measure from one frame's magnitude spectrum `mag` and its
/// per-bin frequencies `freqs` (same length), plus the previous frame's
/// magnitude spectrum for `flux` (`None` on the first frame, or when the
/// bin count changed).
pub(crate) fn measures(mag: &[f64], freqs: &[f64], prev: Option<&[f64]>) -> Measures {
    let n = mag.len().min(freqs.len());
    if n == 0 {
        return Measures::default();
    }
    let sum: f64 = mag.iter().take(n).sum();
    let mean = sum / n as f64;

    let variance = mag.iter().take(n).map(|&x| (x - mean).powi(2)).sum::<f64>() / n as f64;

    let centroid = if sum > 0.0 {
        mag.iter()
            .zip(freqs.iter())
            .take(n)
            .map(|(&x, &f)| x * f)
            .sum::<f64>()
            / sum
    } else {
        0.0
    };

    let spread_sq = if sum > 0.0 {
        mag.iter()
            .zip(freqs.iter())
            .take(n)
            .map(|(&x, &f)| x * (f - centroid).powi(2))
            .sum::<f64>()
            / sum
    } else {
        0.0
    };
    let spread = spread_sq.max(0.0).sqrt();

    let (skewness, kurtosis) = if sum > 0.0 && spread > 1e-12 {
        let m3 = mag
            .iter()
            .zip(freqs.iter())
            .take(n)
            .map(|(&x, &f)| x * (f - centroid).powi(3))
            .sum::<f64>()
            / sum;
        let m4 = mag
            .iter()
            .zip(freqs.iter())
            .take(n)
            .map(|(&x, &f)| x * (f - centroid).powi(4))
            .sum::<f64>()
            / sum;
        (m3 / spread.powi(3), m4 / spread.powi(4))
    } else {
        (0.0, 0.0)
    };

    let entropy = if sum > 0.0 {
        -mag.iter()
            .take(n)
            .filter(|&&x| x > 0.0)
            .map(|&x| {
                let p = x / sum;
                p * p.log2()
            })
            .sum::<f64>()
    } else {
        0.0
    };

    // Geometric mean via the log domain, so a single zero bin does not force
    // the whole product to zero before it can even be computed.
    let log_sum: f64 = mag
        .iter()
        .take(n)
        .map(|&x| if x > 0.0 { x.ln() } else { -700.0 })
        .sum();
    let geo_mean = (log_sum / n as f64).exp();
    let flatness = if mean > 1e-300 { geo_mean / mean } else { 0.0 };

    let peak = mag.iter().take(n).copied().fold(0.0f64, f64::max);
    let crest = if mean > 1e-300 { peak / mean } else { 0.0 };

    let flux = match prev {
        Some(p) if p.len() == n => mag
            .iter()
            .zip(p.iter())
            .take(n)
            .map(|(&x, &y)| (x - y).powi(2))
            .sum::<f64>()
            .sqrt(),
        _ => 0.0,
    };

    // Least-squares slope of magnitude against frequency.
    let mean_f = freqs.iter().take(n).sum::<f64>() / n as f64;
    let mut cov = 0.0;
    let mut var_f = 0.0;
    for (&x, &f) in mag.iter().zip(freqs.iter()).take(n) {
        cov += (f - mean_f) * (x - mean);
        var_f += (f - mean_f).powi(2);
    }
    let slope = if var_f > 1e-300 { cov / var_f } else { 0.0 };

    // Spectral decrease: relative decrease of amplitude for bins after the
    // first, each weighted by 1/k.
    let mut decrease_num = 0.0;
    let mut decrease_den = 0.0;
    let first = mag.first().copied().unwrap_or(0.0);
    for (k, &x) in mag.iter().enumerate().take(n).skip(1) {
        decrease_num += (x - first) / k as f64;
        decrease_den += x;
    }
    let decrease = if decrease_den.abs() > 1e-300 {
        decrease_num / decrease_den
    } else {
        0.0
    };

    // Rolloff: the lowest bin frequency at or beyond which 95% of the
    // spectrum's energy (sum of magnitude, not power) has accumulated.
    let mut rolloff = freqs.last().copied().unwrap_or(0.0);
    if sum > 0.0 {
        let target = 0.95 * sum;
        let mut acc = 0.0;
        for (&x, &f) in mag.iter().zip(freqs.iter()).take(n) {
            acc += x;
            if acc >= target {
                rolloff = f;
                break;
            }
        }
    } else {
        rolloff = 0.0;
    }

    Measures {
        mean,
        variance,
        centroid,
        spread,
        skewness,
        kurtosis,
        entropy,
        flatness,
        crest,
        flux,
        slope,
        decrease,
        rolloff,
    }
}

#[cfg(test)]
mod tests {
    use super::measures;

    /// All energy in one bin: the centroid is that bin's frequency and the
    /// spread around it is exactly zero — a property of the *definition*,
    /// checked without a second FFT anywhere in sight.
    #[test]
    fn a_single_bin_spectrum_has_that_bins_centroid_and_zero_spread() {
        let mag = vec![0.0, 0.0, 5.0, 0.0, 0.0];
        let freqs = vec![0.0, 100.0, 200.0, 300.0, 400.0];
        let m = measures(&mag, &freqs, None);
        assert!((m.centroid - 200.0).abs() < 1e-9);
        assert!(m.spread.abs() < 1e-9);
    }

    /// A perfectly flat spectrum has flatness == 1 (geometric mean equals
    /// arithmetic mean only when every value is equal) and the lowest
    /// possible crest factor for a nonzero spectrum, `1.0`.
    #[test]
    fn a_flat_spectrum_has_unit_flatness_and_unit_crest() {
        let mag = vec![3.0; 8];
        let freqs: Vec<f64> = (0..8).map(|i| f64::from(i) * 100.0).collect();
        let m = measures(&mag, &freqs, None);
        assert!((m.flatness - 1.0).abs() < 1e-9, "flatness={}", m.flatness);
        assert!((m.crest - 1.0).abs() < 1e-9, "crest={}", m.crest);
    }

    /// A spectrum symmetric around its centroid has zero skewness — the
    /// third standardized moment of a symmetric distribution is exactly
    /// zero by definition.
    #[test]
    fn a_symmetric_spectrum_has_zero_skewness() {
        let mag = vec![1.0, 3.0, 5.0, 3.0, 1.0];
        let freqs = vec![0.0, 100.0, 200.0, 300.0, 400.0];
        let m = measures(&mag, &freqs, None);
        assert!(m.skewness.abs() < 1e-9, "skewness={}", m.skewness);
    }

    /// Identical consecutive spectra have zero flux; the first frame (no
    /// `prev`) also reports zero rather than an undefined value.
    #[test]
    fn identical_spectra_have_zero_flux() {
        let mag = vec![1.0, 2.0, 3.0];
        let freqs = vec![0.0, 100.0, 200.0];
        let first = measures(&mag, &freqs, None);
        assert!(first.flux.abs() < 1e-15);
        let second = measures(&mag, &freqs, Some(&mag));
        assert!(second.flux.abs() < 1e-15);
    }

    #[test]
    fn silence_produces_finite_defaults_not_nan_or_panics() {
        let mag = vec![0.0; 6];
        let freqs: Vec<f64> = (0..6).map(|i| f64::from(i) * 100.0).collect();
        let m = measures(&mag, &freqs, None);
        assert!(m.mean.is_finite());
        assert!(m.centroid.is_finite());
        assert!(m.flatness.is_finite());
        assert!(m.crest.is_finite());
    }
}
