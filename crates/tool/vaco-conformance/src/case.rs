//! The case model (plan 13 §1.1, §1.9).
//!
//! # What it is
//!
//! `Case = (Media, Argv, Comparison) -> Verdict`. Everything the runner needs
//! to execute one comparison, and everything the reporter needs to explain it.
//!
//! # How to change it
//!
//! Adding a comparison mode means a [`Compare`] variant, a `"kebab-name"` in
//! [`Compare::from_manifest`], and an arm in `compare::evaluate`. Adding a
//! *verdict* is a much bigger deal: every reporter, every exit-code mapping and
//! the tier gating all switch on it.

use std::fmt;
use std::time::Duration;

use crate::toml::{Table, Value};

/// Which binary a case drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// `vaco-probe` against `ffprobe`.
    Probe,
    /// `vaco` against `ffmpeg`.
    Transcode,
    /// `vaco-play --headless` against `ffplay`.
    PlayHeadless,
}

impl Tool {
    /// Parse the manifest spelling.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "probe" => Some(Self::Probe),
            "transcode" => Some(Self::Transcode),
            "play-headless" => Some(Self::PlayHeadless),
            _ => None,
        }
    }

    /// The manifest spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Probe => "probe",
            Self::Transcode => "transcode",
            Self::PlayHeadless => "play-headless",
        }
    }
}

impl fmt::Display for Tool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Execution tier (plan 13 §1.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// Every PR. Must stay under four minutes.
    Smoke,
    /// Merge to `main`, and PRs touching a mapped crate.
    Core,
    /// Nightly.
    Full,
    /// Weekly and pre-release.
    Exhaustive,
    /// Run only when named explicitly.
    Manual,
}

impl Tier {
    /// Parse the manifest spelling.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "smoke" => Some(Self::Smoke),
            "core" => Some(Self::Core),
            "full" => Some(Self::Full),
            "exhaustive" => Some(Self::Exhaustive),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }

    /// The manifest spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Smoke => "smoke",
            Self::Core => "core",
            Self::Full => "full",
            Self::Exhaustive => "exhaustive",
            Self::Manual => "manual",
        }
    }

    /// Whether a case at `self` runs when the requested tier is `requested`.
    ///
    /// Tiers are cumulative up to `exhaustive`; `manual` is never included by
    /// a tier selection and must be named.
    #[must_use]
    pub fn included_by(self, requested: Self) -> bool {
        self != Self::Manual && requested != Self::Manual && self <= requested
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What is captured from a run and fed to the comparator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capture {
    /// The process's standard output.
    Stdout,
    /// The process's standard error.
    Stderr,
    /// The exit code. Always compared as a co-assertion (§1.2 C0).
    ExitCode,
    /// A file the case told the tool to write.
    OutputFile,
}

impl Capture {
    /// Parse the manifest spelling.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "stdout" => Some(Self::Stdout),
            "stderr" => Some(Self::Stderr),
            "exit-code" => Some(Self::ExitCode),
            "output-file" => Some(Self::OutputFile),
            _ => None,
        }
    }
}

/// A numeric tolerance. Never defaulted — a case that does not name one gets
/// zero (§1.2 C5).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Tolerance {
    /// Maximum absolute per-sample difference.
    pub max_abs: f64,
    /// Maximum difference in units in the last place.
    pub max_ulp: u32,
    /// Maximum root-mean-square difference over the stream.
    pub max_rms: f64,
}

impl Tolerance {
    /// Whether this tolerance permits nothing at all.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.max_abs == 0.0 && self.max_ulp == 0 && self.max_rms == 0.0
    }
}

/// The quality band an encoder case is judged against (§1.11.2, C10).
///
/// The metric implementations are deliberately **not** in this crate yet — see
/// [`crate::compare::quality`]. This type is the manifest-facing half of the
/// seam so that encoder cases can be authored and reviewed before the metrics
/// land.
#[derive(Debug, Clone, PartialEq)]
pub struct QualityBand {
    /// Metric name, e.g. `psnr-y`, `ssim`, `opus-compare`.
    pub metric: String,
    /// How far below the reference's quality we may fall.
    pub delta_q: f64,
    /// How much larger our bitstream may be, as a fraction.
    pub delta_size: f64,
    /// How much slower our encode may be, as a multiplier.
    pub delta_time: f64,
    /// Why this band is the right one. Reviewed like a tolerance (§1.12.3).
    pub justification: String,
}

/// The ten comparison modes (plan 13 §1.2, §1.10, §1.11.2).
#[derive(Debug, Clone, PartialEq)]
pub enum Compare {
    /// C0 — full byte equality after no normalisation.
    ExactBytes {
        /// What is compared.
        captures: Vec<Capture>,
    },
    /// C1 — C0 with a non-empty normalisation chain, named separately so a
    /// reviewer sees the permitted blindness at a glance.
    ExactBytesNormalised {
        /// What is compared.
        captures: Vec<Capture>,
    },
    /// C2 — our own container walker's tree, compared node by node.
    ContainerStructure {
        /// Which walker to use, e.g. `isobmff`.
        walker: String,
    },
    /// C3 — per-frame digests of decoded output.
    FrameHash {
        /// `pipe-hash` (C3a, preferred) or `framecrc`/`framemd5` (C3b).
        variant: String,
    },
    /// C4 — full byte equality of the decoded raw stream.
    RawExact,
    /// C5 — C4 with an explicit, justified tolerance.
    RawTolerant {
        /// The permitted error.
        tolerance: Tolerance,
        /// The spec clause that defines it.
        justification: String,
    },
    /// C6 — section-tree diff with the divergence allowlist consulted.
    StructuredDiff {
        /// Which writer's output is being parsed, e.g. `default`, `json`.
        writer: String,
    },
    /// C7 — outcome class only: exit code, accept/reject, error category.
    Behavioural,
    /// C8 — the interoperability matrix.
    CrossDecode {
        /// Which of `x1`..`x4` to run.
        legs: Vec<String>,
    },
    /// C9 — native / external / reference lattice (D11).
    ThreeWay {
        /// The pairwise comparison applied within the lattice.
        inner: Box<Compare>,
    },
    /// C10 — encoder quality band. Bitstreams are not compared at all.
    QualityBand {
        /// The declared band.
        band: Box<QualityBand>,
    },
}

impl Compare {
    /// The manifest spelling.
    #[must_use]
    pub const fn mode_name(&self) -> &'static str {
        match self {
            Self::ExactBytes { .. } => "exact-bytes",
            Self::ExactBytesNormalised { .. } => "exact-bytes-normalised",
            Self::ContainerStructure { .. } => "container-structure",
            Self::FrameHash { .. } => "frame-hash",
            Self::RawExact => "raw-exact",
            Self::RawTolerant { .. } => "raw-tolerant",
            Self::StructuredDiff { .. } => "structured-diff",
            Self::Behavioural => "behavioural",
            Self::CrossDecode { .. } => "cross-decode",
            Self::ThreeWay { .. } => "three-way",
            Self::QualityBand { .. } => "quality-band",
        }
    }

    /// Build a comparison from a manifest `[compare]` table.
    ///
    /// # Errors
    /// An unknown mode, or a mode whose required parameters are missing. A
    /// `raw-tolerant` without a `justification` is rejected here rather than in
    /// review, because §1.2 C5 says a hand-waved tolerance is not acceptable
    /// and a machine check is cheaper than a reviewer's attention.
    pub fn from_manifest(t: &Table) -> Result<Self, String> {
        let mode = t
            .get("mode")
            .and_then(Value::as_str)
            .ok_or("[compare] needs a `mode`")?;
        let captures = || -> Result<Vec<Capture>, String> {
            let listed = t
                .get("capture")
                .and_then(Value::as_str_array)
                .unwrap_or_else(|| vec!["stdout".to_owned(), "exit-code".to_owned()]);
            let mut out = Vec::new();
            for name in listed {
                out.push(Capture::parse(&name).ok_or_else(|| format!("unknown capture `{name}`"))?);
            }
            Ok(out)
        };
        match mode {
            "exact-bytes" => Ok(Self::ExactBytes {
                captures: captures()?,
            }),
            "exact-bytes-normalised" => Ok(Self::ExactBytesNormalised {
                captures: captures()?,
            }),
            "container-structure" => Ok(Self::ContainerStructure {
                walker: t
                    .get("walker")
                    .and_then(Value::as_str)
                    .ok_or("container-structure needs a `walker`")?
                    .to_owned(),
            }),
            "frame-hash" => Ok(Self::FrameHash {
                variant: t
                    .get("variant")
                    .and_then(Value::as_str)
                    .unwrap_or("pipe-hash")
                    .to_owned(),
            }),
            "raw-exact" => Ok(Self::RawExact),
            "raw-tolerant" => {
                let tol = t.get("tolerance").and_then(Value::as_table);
                let justification = t
                    .get("justification")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_owned();
                if justification.is_empty() {
                    return Err(
                        "raw-tolerant needs a `justification` naming the clause that \
                         defines the tolerance (§1.2 C5)"
                            .to_owned(),
                    );
                }
                let tolerance = Tolerance {
                    max_abs: tol
                        .and_then(|t| t.get("max_abs"))
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0),
                    max_ulp: tol
                        .and_then(|t| t.get("max_ulp"))
                        .and_then(Value::as_int)
                        .and_then(|v| u32::try_from(v).ok())
                        .unwrap_or(0),
                    max_rms: tol
                        .and_then(|t| t.get("max_rms"))
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0),
                };
                Ok(Self::RawTolerant {
                    tolerance,
                    justification,
                })
            }
            "structured-diff" => Ok(Self::StructuredDiff {
                writer: t
                    .get("writer")
                    .and_then(Value::as_str)
                    .unwrap_or("default")
                    .to_owned(),
            }),
            "behavioural" => Ok(Self::Behavioural),
            "cross-decode" => Ok(Self::CrossDecode {
                legs: t
                    .get("legs")
                    .and_then(Value::as_str_array)
                    .unwrap_or_else(|| vec!["x1".into(), "x2".into(), "x3".into(), "x4".into()]),
            }),
            "three-way" => {
                let mut inner_table = t.clone();
                let inner_mode = t
                    .get("inner")
                    .and_then(Value::as_str)
                    .ok_or("three-way needs an `inner` mode")?
                    .to_owned();
                inner_table.insert("mode".to_owned(), Value::String(inner_mode));
                inner_table.remove("inner");
                Ok(Self::ThreeWay {
                    inner: Box::new(Self::from_manifest(&inner_table)?),
                })
            }
            "quality-band" => {
                let band = t
                    .get("band")
                    .and_then(Value::as_table)
                    .ok_or("quality-band needs a `[compare.band]`")?;
                let justification = band
                    .get("justification")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_owned();
                if justification.is_empty() {
                    return Err(
                        "quality-band needs a `justification`; a band is reviewed like a \
                         tolerance (§1.11.2)"
                            .to_owned(),
                    );
                }
                Ok(Self::QualityBand {
                    band: Box::new(QualityBand {
                        metric: band
                            .get("metric")
                            .and_then(Value::as_str)
                            .ok_or("quality-band needs a `metric`")?
                            .to_owned(),
                        delta_q: band.get("delta_q").and_then(Value::as_f64).unwrap_or(0.0),
                        delta_size: band
                            .get("delta_size")
                            .and_then(Value::as_f64)
                            .unwrap_or(0.0),
                        delta_time: band
                            .get("delta_time")
                            .and_then(Value::as_f64)
                            .unwrap_or(1.0),
                        justification,
                    }),
                })
            }
            other => Err(format!("unknown comparison mode `{other}`")),
        }
    }
}

/// A stable, human-typeable case identifier (§1.5.2).
///
/// `suite/media/axis=value,axis=value`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CaseId(String);

impl CaseId {
    /// Build an id from its parts. Axis pairs are emitted in the order the
    /// manifest declares them, which is what makes the id stable.
    #[must_use]
    pub fn new(suite: &str, media: &str, axes: &[(String, String)]) -> Self {
        let mut s = format!("{suite}/{media}/");
        for (i, (name, value)) in axes.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(name);
            s.push('=');
            s.push_str(value);
        }
        Self(s)
    }

    /// The id as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The suite this case belongs to.
    #[must_use]
    pub fn suite(&self) -> &str {
        self.0.split('/').next().unwrap_or_default()
    }
}

impl fmt::Display for CaseId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A media input a case consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaRef {
    /// Manifest-local identifier, used in the case id.
    pub id: String,
    /// `corpus://…`, `suite://…`, or a path relative to the manifest.
    pub source: String,
    /// Free-form selectors.
    pub tags: Vec<String>,
    /// Arguments that **synthesise** this media with the reference binary,
    /// instead of fetching it.
    ///
    /// The output path is appended by the runner, so a manifest writes only the
    /// input and encoding half:
    ///
    /// ```toml
    /// generate = ["-f", "lavfi", "-i", "testsrc=size=320x240:rate=25:d=1",
    ///             "-c:v", "mpeg4", "-f", "mp4"]
    /// ```
    ///
    /// This is what makes the harness runnable **today**, before the corpus
    /// machinery of QA-04/X-05 exists — and it is squarely inside D6's rule,
    /// which is that expected values may be *generated fresh from the reference
    /// at test time and discarded*. Nothing FFmpeg-derived enters the
    /// repository: the bytes live in a temporary directory for the length of
    /// one run.
    ///
    /// It is also the honest option for a differential harness. A committed
    /// media file is a fixture whose provenance somebody has to defend; a file
    /// synthesised by a command visible in the manifest defends itself.
    pub generate: Option<Vec<String>>,
}

impl MediaRef {
    /// The on-disk name this media takes inside a case's working directory.
    ///
    /// Derived from the `source` so a suite controls the extension — which
    /// matters more than it looks, because extension-based format guessing is
    /// part of what these cases are testing.
    #[must_use]
    pub fn file_name(&self) -> String {
        let tail = self.source.rsplit('/').next().unwrap_or(&self.source);
        if tail.is_empty() {
            self.id.clone()
        } else {
            tail.to_owned()
        }
    }
}

/// One executable comparison.
#[derive(Debug, Clone)]
pub struct Case {
    /// Stable identifier.
    pub id: CaseId,
    /// Which binary pair to drive.
    pub tool: Tool,
    /// Inputs. Empty for source filters.
    pub media: Vec<MediaRef>,
    /// Tool-neutral argument vector; the runner prepends the binary.
    pub argv: Vec<String>,
    /// How the outputs are compared.
    pub compare: Compare,
    /// Named normalisers, in application order.
    pub normalise: crate::normalise::Chain,
    /// Features the tool under test must have, else the case skips.
    pub requires: Vec<String>,
    /// Wall-clock budget.
    pub timeout: Duration,
    /// Which tier runs this case.
    pub tier: Tier,
}

impl Case {
    /// The reproduction command printed with every failure (§1.5.2).
    #[must_use]
    pub fn reproduction(&self) -> String {
        format!("just conformance-run '{}'", self.id)
    }
}

/// Why a case did not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// No reference installation.
    NoReference(String),
    /// The media is not present and the run is offline.
    MediaMissing(String),
    /// The tool under test lacks a declared feature.
    FeatureMissing(String),
    /// Our binary for this tool has not been built yet.
    ToolNotBuilt(String),
    /// The reference lacks the component, detected by probing its listings.
    ReferenceLacksComponent(String),
    /// The comparison mode exists but its machinery does not yet.
    ModeUnimplemented(&'static str),
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoReference(why) => write!(f, "no reference: {why}"),
            Self::MediaMissing(m) => write!(f, "media not available offline: {m}"),
            Self::FeatureMissing(x) => write!(f, "we do not have `{x}`"),
            Self::ToolNotBuilt(what) => write!(f, "{what}"),
            Self::ReferenceLacksComponent(x) => write!(f, "reference lacks `{x}`"),
            Self::ModeUnimplemented(m) => write!(f, "comparison mode `{m}` not implemented yet"),
        }
    }
}

/// How a run went wrong on one side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureKind {
    /// Exceeded the wall-clock budget.
    Timeout,
    /// Exited non-zero where the case expected success.
    UnexpectedExit(Option<i32>),
    /// Failed to start at all.
    LaunchFailed(String),
}

impl fmt::Display for FailureKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => f.write_str("timed out"),
            Self::UnexpectedExit(code) => write!(f, "exited {code:?}"),
            Self::LaunchFailed(e) => write!(f, "failed to launch: {e}"),
        }
    }
}

/// The outcome of one case.
#[derive(Debug, Clone)]
pub enum Verdict {
    /// Outputs matched under the declared mode.
    Agree,
    /// Outputs differed, and every difference matched an allowlist entry.
    AllowedDivergence(Vec<crate::divergence::DivergenceId>),
    /// Outputs differed and at least one difference is unexplained.
    Divergence(crate::compare::DiffReport),
    /// Our side failed.
    OursFailed(FailureKind),
    /// The reference failed. Usually means the case is wrong.
    ReferenceFailed(FailureKind),
    /// The case did not run.
    Skipped(SkipReason),
}

impl Verdict {
    /// Whether this verdict fails a gating run.
    #[must_use]
    pub const fn is_failure(&self) -> bool {
        matches!(
            self,
            Self::Divergence(_) | Self::OursFailed(_) | Self::ReferenceFailed(_)
        )
    }

    /// A one-word label for reports.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Agree => "agree",
            Self::AllowedDivergence(_) => "allowed",
            Self::Divergence(_) => "DIVERGED",
            Self::OursFailed(_) => "OURS-FAILED",
            Self::ReferenceFailed(_) => "REF-FAILED",
            Self::Skipped(_) => "skipped",
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "a failing expectation in a test is a failing test"
)]
mod tests {
    use super::{CaseId, Compare, Tier, Tool};
    use crate::toml;

    #[test]
    fn tiers_are_cumulative_and_manual_is_opt_in() {
        assert!(Tier::Smoke.included_by(Tier::Core));
        assert!(Tier::Core.included_by(Tier::Full));
        assert!(!Tier::Full.included_by(Tier::Core));
        assert!(!Tier::Manual.included_by(Tier::Exhaustive));
        assert!(!Tier::Smoke.included_by(Tier::Manual));
    }

    #[test]
    fn case_ids_are_stable_and_reproducible() {
        let id = CaseId::new(
            "probe-isobmff",
            "h264-aac-30f",
            &[
                ("writer".to_owned(), "json".to_owned()),
                ("sections".to_owned(), "all".to_owned()),
            ],
        );
        assert_eq!(
            id.as_str(),
            "probe-isobmff/h264-aac-30f/writer=json,sections=all"
        );
        assert_eq!(id.suite(), "probe-isobmff");
    }

    fn compare_of(text: &str) -> Result<Compare, String> {
        let doc = toml::parse(text).map_err(|e| e.to_string())?;
        let t = doc
            .get("compare")
            .and_then(toml::Value::as_table)
            .ok_or("no [compare]")?;
        Compare::from_manifest(t)
    }

    #[test]
    fn exact_bytes_defaults_to_stdout_and_exit_code() {
        let c = compare_of("[compare]\nmode = \"exact-bytes\"\n").expect("parses");
        match c {
            Compare::ExactBytes { captures } => assert_eq!(captures.len(), 2),
            other => panic!("wrong mode: {}", other.mode_name()),
        }
    }

    #[test]
    fn a_hand_waved_tolerance_is_rejected() {
        let err =
            compare_of("[compare]\nmode = \"raw-tolerant\"\n[compare.tolerance]\nmax_abs = 1.0\n")
                .expect_err("must be rejected");
        assert!(err.contains("justification"), "{err}");
    }

    #[test]
    fn a_justified_tolerance_is_accepted() {
        let c = compare_of(
            "[compare]\nmode = \"raw-tolerant\"\njustification = \"RFC 6716 §6\"\n\
             [compare.tolerance]\nmax_abs = 1.0\n",
        )
        .expect("parses");
        match c {
            Compare::RawTolerant { tolerance, .. } => {
                assert!((tolerance.max_abs - 1.0).abs() < f64::EPSILON);
                assert!(!tolerance.is_zero());
            }
            other => panic!("wrong mode: {}", other.mode_name()),
        }
    }

    #[test]
    fn three_way_wraps_its_inner_mode() {
        let c =
            compare_of("[compare]\nmode = \"three-way\"\ninner = \"raw-exact\"\n").expect("parses");
        match c {
            Compare::ThreeWay { inner } => assert_eq!(inner.mode_name(), "raw-exact"),
            other => panic!("wrong mode: {}", other.mode_name()),
        }
    }

    #[test]
    fn quality_band_needs_a_justification() {
        let err =
            compare_of("[compare]\nmode = \"quality-band\"\n[compare.band]\nmetric = \"psnr-y\"\n")
                .expect_err("must be rejected");
        assert!(err.contains("justification"), "{err}");
    }

    #[test]
    fn tool_names_round_trip() {
        for t in [Tool::Probe, Tool::Transcode, Tool::PlayHeadless] {
            assert_eq!(Tool::parse(t.as_str()), Some(t));
        }
    }
}
