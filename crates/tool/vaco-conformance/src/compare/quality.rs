//! C10 — quality-band comparison (plan 13 §1.11.2; work package X-04, `#253`).
//!
//! # What it is
//!
//! Encoder conformance: instead of comparing bitstream bytes (meaningless
//! for a lossy encoder's own rate–distortion search — see plan 13 §1.11),
//! both sides' *reconstructed* output is scored against the common source
//! with a [`Metric`], and the comparison is a band around the reference's
//! own score plus bounds on bitstream size and encode time.
//!
//! ```text
//! source ──┬─▶ reference encode ──▶ ref bitstream ──▶ decode ──▶ ref recon
//!          └─▶ our encode       ──▶ our bitstream ──▶ decode ──▶ our recon
//!
//! assert:  quality(our recon, source) ≥ quality(ref recon, source) − Δq
//!          size(our bitstream)        ≤ size(ref bitstream)        × (1 + Δs)
//!          time(our encode)           ≤ time(ref encode)           × Δt
//! ```
//!
//! (`reference decoder accepts our bitstream` / `our decoder accepts the
//! reference bitstream` are C8/X4 and C8/X3 respectively — a different mode,
//! not this one.)
//!
//! # What is implemented, and what is still a seam
//!
//! [`Metric`] is the extension point; [`default_registry`] wires up the real
//! implementations in `vaco_conformance::metrics` (PSNR, SSIM, a spectral
//! distance for audio — see that module's own docs, including why VMAF is a
//! named cut rather than a silent one). [`compare`] uses that registry and,
//! when a [`Pair`] carries [`QualitySignals`] (via
//! [`crate::compare::Pair::with_signals`]), measures for real: it computes
//! both sides' score against the shared source, checks the bitstream-size
//! and encode-time bounds from `pair`'s own [`crate::run::Observation::wall`]
//! and output-file lengths, and returns [`Verdict::Agree`] or a
//! [`Verdict::Divergence`] carrying the numbers that failed.
//!
//! **What is still a seam**: nothing in this crate decodes a bitstream back
//! to raw samples yet, so a case whose [`Pair`] has no attached
//! [`QualitySignals`] still skips honestly — see
//! [`crate::compare::Pair::signals`]'s own docs for exactly what a caller
//! needs to supply to turn that skip into a real measurement.

use std::collections::BTreeMap;

use crate::case::{Case, QualityBand, SkipReason, Verdict};
use crate::compare::{DiffReport, Pair};

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

/// The three decoded signals a C10 measurement needs (the seam's own
/// diagram: `source`, `our recon`, `ref recon`), attached to a
/// [`crate::compare::Pair`] via `Pair::with_signals`.
#[derive(Debug, Clone)]
pub struct QualitySignals<'a> {
    /// The original, undistorted media both encoders were given.
    pub source: Signal<'a>,
    /// Our encoder's bitstream, decoded back to raw samples.
    pub ours: Signal<'a>,
    /// The reference encoder's bitstream, decoded back to raw samples.
    pub theirs: Signal<'a>,
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

/// The metrics `compare` measures with when a case names one and does not
/// supply its own [`Registry`]. Built fresh per call — a `BTreeMap` of a
/// handful of zero-sized [`Metric`] impls costs nothing measurable next to
/// running an actual decode, and a `'static` registry would need interior
/// mutability for no benefit, since nothing here ever needs to swap a
/// metric out at runtime.
#[must_use]
pub fn default_registry() -> Registry {
    let mut registry = Registry::new();
    registry.insert(Box::new(crate::metrics::Psnr::y()));
    registry.insert(Box::new(crate::metrics::Psnr::average()));
    registry.insert(Box::new(crate::metrics::Ssim));
    registry.insert(Box::new(crate::metrics::SpectralDistance));
    registry
}

/// Evaluate a C10 case against [`default_registry`].
///
/// Skips honestly (never [`Verdict::Agree`]) in the two cases that are not
/// yet measurable: `band.metric` names something [`default_registry`] does
/// not have (VMAF, most prominently — see `crate::metrics`' own docs for why
/// it is cut), or `pair` carries no [`QualitySignals`] yet because nothing
/// upstream has decoded a bitstream back to raw samples for this case. A
/// quality gate that passes without measuring anything is worse than no
/// gate, because it looks like coverage.
#[must_use]
pub fn compare(case: &Case, pair: &Pair<'_>, band: &QualityBand) -> Verdict {
    compare_with(case, pair, band, &default_registry())
}

/// [`compare`], taking an explicit [`Registry`] — the seam a caller with its
/// own metric set (or a test double) uses instead of [`default_registry`].
#[must_use]
pub fn compare_with(case: &Case, pair: &Pair<'_>, band: &QualityBand, registry: &Registry) -> Verdict {
    let Some(metric) = registry.get(&band.metric) else {
        return Verdict::Skipped(SkipReason::ModeUnimplemented(
            "quality-band: band names a metric this build has not registered \
             (see vaco_conformance::compare::quality::default_registry)",
        ));
    };
    let Some(signals) = &pair.signals else {
        return Verdict::Skipped(SkipReason::ModeUnimplemented(
            "quality-band: no decoded signal attached to this Pair yet — \
             see Pair::with_signals",
        ));
    };

    let ours = match metric.score(&signals.source, &signals.ours) {
        Ok(v) => v,
        Err(e) => return divergence(case, format!("scoring our recon: {e}")),
    };
    let theirs = match metric.score(&signals.source, &signals.theirs) {
        Ok(v) => v,
        Err(e) => return divergence(case, format!("scoring reference recon: {e}")),
    };

    let measurement = Measurement {
        metric: band.metric.clone(),
        ours,
        theirs,
        our_bytes: pair.ours_output_file.map_or(0, |b| b.len() as u64),
        their_bytes: pair.theirs_output_file.map_or(0, |b| b.len() as u64),
        our_seconds: pair.ours.wall.as_secs_f64(),
        their_seconds: pair.theirs.wall.as_secs_f64(),
    };

    match measurement.within(band) {
        Ok(()) => Verdict::Agree,
        Err(reason) => divergence(case, reason),
    }
}

fn divergence(case: &Case, summary: String) -> Verdict {
    Verdict::Divergence(DiffReport {
        mode: case.compare.mode_name(),
        summary,
        ..DiffReport::default()
    })
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
