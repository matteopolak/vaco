//! C10 — the quality-band seam (plan 13 §1.11.2).
//!
//! # What it is
//!
//! The place encoder conformance will live. **The metrics are deliberately not
//! implemented here.** This module defines the shape they must fit and returns
//! an honest skip until they exist.
//!
//! # Why a seam and not an implementation
//!
//! Plan 13 §1.11 draws the boundary: byte comparison applies to every operation
//! whose output is fully determined by its input and its declared options;
//! quality comparison applies to every operation that involves a lossy
//! encoder's rate–distortion decisions. Our AV1 encoder will never match
//! libaom's bitstream, and libaom's does not match its own across versions or
//! thread counts. Asserting bytes there produces a permanent red that everyone
//! learns to ignore, which is worse than no test.
//!
//! But the metrics themselves are real work with a legal constraint attached:
//! `tests/tiny_ssim.c` and `tests/tiny_psnr.c` are GPL and are on the hard
//! do-not-reuse list (§0.1). SSIM must be implemented from Wang, Bovik, Sheikh
//! & Simoncelli, *IEEE TIP* 13(4), 2004, and PSNR from its standard
//! definition — citing the paper, never the file. That belongs in the crate
//! that owns image metrics, not in the harness, and it is not this agent's to
//! write.
//!
//! # The seam
//!
//! ```text
//! source ──┬─▶ reference encode ──▶ ref bitstream ──▶ decode ──▶ ref recon
//!          └─▶ our encode       ──▶ our bitstream ──▶ decode ──▶ our recon
//!
//! assert:  quality(our recon, source) ≥ quality(ref recon, source) − Δq
//!          size(our bitstream)        ≤ size(ref bitstream)        × (1 + Δs)
//!          time(our encode)           ≤ time(ref encode)           × Δt
//!          reference decoder accepts our bitstream          (C8/X4)
//!          our decoder accepts the reference bitstream      (C8/X3)
//! ```
//!
//! [`Metric`] is the extension point. Register an implementation with
//! [`Registry::insert`] and C10 cases naming that metric start running; until
//! one is registered they skip, and the skip budget (§1.5.4) makes that
//! visible rather than silently green.
//!
//! # How to change it
//!
//! Implement [`Metric`] in the crate that owns the metric, register it from the
//! runner's construction site, and fill in [`compare`]'s measurement half. Do
//! not implement a metric here — the harness should not become the home of the
//! project's image mathematics.

use std::collections::BTreeMap;

use crate::case::{Case, QualityBand, SkipReason, Verdict};
use crate::compare::Pair;

/// One reconstructed signal, ready for a metric.
///
/// Deliberately format-agnostic: the runner hands the metric raw planes plus
/// their geometry, so a metric never learns about containers or codecs.
#[derive(Debug, Clone)]
pub struct Signal<'a> {
    /// Planar sample data, one entry per plane.
    pub planes: Vec<&'a [u8]>,
    /// Row stride per plane, in bytes.
    pub strides: Vec<usize>,
    /// Width in samples.
    pub width: u32,
    /// Height in rows. `1` for audio.
    pub height: u32,
    /// Bits per sample.
    pub depth: u8,
}

/// A quality metric.
///
/// Higher is better, always — a metric that is naturally "lower is better"
/// negates in its own implementation, so the band arithmetic in [`compare`]
/// has one direction and no special cases.
pub trait Metric: Send + Sync + std::fmt::Debug {
    /// The manifest-facing name, e.g. `psnr-y`, `ssim`, `opus-compare`.
    fn name(&self) -> &'static str;

    /// Score `distorted` against `source`. Higher is better.
    ///
    /// # Errors
    /// A geometry or depth mismatch the metric cannot handle.
    fn score(&self, source: &Signal<'_>, distorted: &Signal<'_>) -> Result<f64, String>;
}

/// The metrics available to C10 cases.
#[derive(Debug, Default)]
pub struct Registry {
    metrics: BTreeMap<&'static str, Box<dyn Metric>>,
}

impl Registry {
    /// An empty registry — the state the project is in today.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a metric under its own name.
    pub fn insert(&mut self, metric: Box<dyn Metric>) {
        self.metrics.insert(metric.name(), metric);
    }

    /// Look a metric up.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&dyn Metric> {
        self.metrics.get(name).map(AsRef::as_ref)
    }

    /// Every registered name, for the run report.
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        self.metrics.keys().copied().collect()
    }
}

/// What a C10 case measured.
///
/// Recorded in `tests/conformance/quality.lock` so the bar only ever moves in
/// our favour: CI fails on a regression beyond the band and records an
/// improvement automatically (§1.11.2).
#[derive(Debug, Clone, PartialEq)]
pub struct Measurement {
    /// Metric name.
    pub metric: String,
    /// Our score against the source.
    pub ours: f64,
    /// The reference's score against the source.
    pub theirs: f64,
    /// Our bitstream size in bytes.
    pub our_bytes: u64,
    /// The reference's bitstream size in bytes.
    pub their_bytes: u64,
    /// Our encode wall time, in seconds.
    pub our_seconds: f64,
    /// The reference's encode wall time, in seconds.
    pub their_seconds: f64,
}

impl Measurement {
    /// Whether the measurement sits inside `band`.
    ///
    /// Pure arithmetic on recorded numbers, so it is testable without an
    /// encoder, a decoder or a metric — which is the point of separating it
    /// from the measuring.
    ///
    /// # Errors
    /// Which of the three bounds was exceeded, and by how much.
    pub fn within(&self, band: &QualityBand) -> Result<(), String> {
        if self.ours < self.theirs - band.delta_q {
            return Err(format!(
                "quality {:.4} is below the reference's {:.4} by more than Δq={:.4}",
                self.ours, self.theirs, band.delta_q
            ));
        }
        let size_cap = self.their_bytes as f64 * (1.0 + band.delta_size);
        if self.our_bytes as f64 > size_cap {
            return Err(format!(
                "bitstream is {} bytes, cap is {size_cap:.0} (reference {} × 1+Δs)",
                self.our_bytes, self.their_bytes
            ));
        }
        let time_cap = self.their_seconds * band.delta_time;
        if self.our_seconds > time_cap {
            return Err(format!(
                "encode took {:.3}s, cap is {time_cap:.3}s (reference {:.3}s × Δt)",
                self.our_seconds, self.their_seconds
            ));
        }
        Ok(())
    }
}

/// Evaluate a C10 case.
///
/// Returns a skip until the metrics exist. It never returns [`Verdict::Agree`]
/// on an unmeasured case: a quality gate that passes without measuring anything
/// is worse than no gate, because it looks like coverage.
#[must_use]
pub fn compare(_case: &Case, _pair: &Pair<'_>, band: &QualityBand) -> Verdict {
    let _ = band;
    Verdict::Skipped(SkipReason::ModeUnimplemented(
        "quality-band: no quality metric is implemented yet (plan 13 §1.11.2; \
         SSIM from Wang et al. 2004, never from tiny_ssim.c)",
    ))
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "a failing expectation in a test is a failing test"
)]
mod tests {
    use super::{Measurement, Metric, Registry, Signal};
    use crate::case::QualityBand;

    fn band() -> QualityBand {
        QualityBand {
            metric: "psnr-y".to_owned(),
            delta_q: 0.5,
            delta_size: 0.05,
            delta_time: 2.0,
            justification: "example".to_owned(),
        }
    }

    fn measurement() -> Measurement {
        Measurement {
            metric: "psnr-y".to_owned(),
            ours: 42.0,
            theirs: 42.2,
            our_bytes: 1000,
            their_bytes: 1000,
            our_seconds: 1.0,
            their_seconds: 1.0,
        }
    }

    #[test]
    fn a_measurement_inside_the_band_passes() {
        measurement().within(&band()).expect("inside the band");
    }

    #[test]
    fn quality_below_the_band_fails() {
        let mut m = measurement();
        m.ours = 40.0;
        let err = m.within(&band()).expect_err("must fail");
        assert!(err.contains("quality"), "{err}");
    }

    #[test]
    fn a_larger_bitstream_beyond_the_band_fails() {
        let mut m = measurement();
        m.our_bytes = 1100;
        assert!(m.within(&band()).is_err());
    }

    #[test]
    fn a_slower_encode_beyond_the_band_fails() {
        let mut m = measurement();
        m.our_seconds = 3.0;
        assert!(m.within(&band()).is_err());
    }

    #[derive(Debug)]
    struct Constant;
    impl Metric for Constant {
        fn name(&self) -> &'static str {
            "constant"
        }
        fn score(&self, _: &Signal<'_>, _: &Signal<'_>) -> Result<f64, String> {
            Ok(1.0)
        }
    }

    #[test]
    fn the_registry_is_the_extension_point() {
        let mut r = Registry::new();
        assert!(r.names().is_empty(), "nothing is implemented yet, honestly");
        r.insert(Box::new(Constant));
        assert_eq!(r.names(), vec!["constant"]);
        assert!(r.get("constant").is_some());
        assert!(r.get("psnr-y").is_none());
    }
}
