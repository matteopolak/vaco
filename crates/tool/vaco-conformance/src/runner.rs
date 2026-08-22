//! The case runner (plan 13 §1.9).
//!
//! # What it is
//!
//! Takes expanded cases, runs both binaries under [`crate::run`], and produces
//! verdicts. Each case gets its own temp dir and shares no mutable state with
//! any other, which is what makes them independently re-runnable and safely
//! parallelisable later.
//!
//! # Graceful absence
//!
//! Two things can be missing, and neither is a failure:
//!
//! - **The reference.** `cargo test` must pass on a machine without `FFmpeg`
//!   (§1.5.4), so a missing reference skips every case with the message
//!   [`crate::refbin::discover`] produced.
//! - **The binary under test.** `vaco-probe` and `vaco` do not exist yet. A
//!   case whose tool is not built skips with a message naming the binary and
//!   the environment variable that would point at it.
//!
//! Skips are **counted and reported**, never silent. §1.5.4 gives a tier a skip
//! budget for exactly this reason: coverage that erodes quietly is worse than
//! coverage that was never claimed.
//!
//! # How to change it
//!
//! Parallelism goes here: cases are independent by construction, so a
//! work-stealing pool over [`Runner::run_case`] is a drop-in change. It is
//! deliberately not written yet — with no binary under test there is nothing to
//! parallelise, and an untested thread pool in the harness would be a source of
//! flakes rather than speed.

use std::path::PathBuf;
use std::time::Duration;

use crate::case::{Case, FailureKind, SkipReason, Tier, Tool, Verdict};
use crate::compare::{self, Pair};
use crate::divergence::Allowlist;
use crate::refbin::Reference;
use crate::run::{Invocation, run};

/// Where our own binaries live.
#[derive(Debug, Clone, Default)]
pub struct UnderTest {
    /// Our ffprobe equivalent.
    pub probe: Option<PathBuf>,
    /// Our ffmpeg equivalent.
    pub transcode: Option<PathBuf>,
    /// Our ffplay equivalent.
    pub play: Option<PathBuf>,
}

impl UnderTest {
    /// Locate our binaries: `VACO_BIN_*` first, then the target directory.
    #[must_use]
    pub fn discover() -> Self {
        Self {
            probe: find("VACO_BIN_PROBE", "vaco-probe"),
            transcode: find("VACO_BIN_VACO", "vaco"),
            play: find("VACO_BIN_PLAY", "vaco-play"),
        }
    }

    /// The binary for `tool`, if it is built.
    #[must_use]
    pub fn binary(&self, tool: Tool) -> Option<&PathBuf> {
        match tool {
            Tool::Probe => self.probe.as_ref(),
            Tool::Transcode => self.transcode.as_ref(),
            Tool::PlayHeadless => self.play.as_ref(),
        }
    }
}

fn find(env_key: &str, name: &str) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(env_key) {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..");
    ["debug", "release"]
        .iter()
        .map(|profile| root.join("target").join(profile).join(name))
        .find(|c| c.is_file())
}

/// One case's outcome, with everything needed to explain it.
#[derive(Debug)]
pub struct Outcome {
    /// Which case.
    pub case: Case,
    /// How it went.
    pub verdict: Verdict,
    /// The command we ran, for the failure report.
    pub ours_command: String,
    /// The command the reference ran.
    pub theirs_command: String,
}

/// Aggregate counts for a run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    /// Cases that agreed.
    pub agreed: u64,
    /// Cases whose every difference was allowlisted.
    pub allowed: u64,
    /// Cases with an unexplained difference.
    pub diverged: u64,
    /// Cases where one side failed to produce a comparable result.
    pub failed: u64,
    /// Cases that did not run.
    pub skipped: u64,
}

impl Tally {
    /// Total cases considered.
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.agreed + self.allowed + self.diverged + self.failed + self.skipped
    }

    /// Fraction of cases that did not run.
    #[must_use]
    pub fn skip_rate(&self) -> f64 {
        let total = self.total();
        if total == 0 {
            return 0.0;
        }
        self.skipped as f64 / total as f64
    }

    /// Whether the run should fail a gate.
    #[must_use]
    pub const fn is_failing(&self) -> bool {
        self.diverged > 0 || self.failed > 0
    }

    fn record(&mut self, verdict: &Verdict) {
        match verdict {
            Verdict::Agree => self.agreed += 1,
            Verdict::AllowedDivergence(_) => self.allowed += 1,
            Verdict::Divergence(_) => self.diverged += 1,
            Verdict::OursFailed(_) | Verdict::ReferenceFailed(_) => self.failed += 1,
            Verdict::Skipped(_) => self.skipped += 1,
        }
    }
}

/// Runs cases against a reference.
#[derive(Debug)]
pub struct Runner<'a> {
    /// The reference installation, if there is one.
    pub reference: Option<&'a Reference>,
    /// Why there isn't one, if there isn't.
    pub absent_reason: String,
    /// Our binaries.
    pub under_test: UnderTest,
    /// The divergence register.
    pub allowlist: &'a Allowlist,
}

impl<'a> Runner<'a> {
    /// A runner over `reference`.
    #[must_use]
    pub fn new(reference: Option<&'a Reference>, allowlist: &'a Allowlist) -> Self {
        Self {
            reference,
            absent_reason: String::new(),
            under_test: UnderTest::discover(),
            allowlist,
        }
    }

    /// Run a set of cases at `tier`, returning outcomes and a tally.
    #[must_use]
    pub fn run_all(&self, cases: &[Case], tier: Tier) -> (Vec<Outcome>, Tally) {
        let mut outcomes = Vec::new();
        let mut tally = Tally::default();
        for case in cases {
            if !case.tier.included_by(tier) {
                continue;
            }
            let outcome = self.run_case(case);
            tally.record(&outcome.verdict);
            outcomes.push(outcome);
        }
        (outcomes, tally)
    }

    /// Run one case.
    #[must_use]
    pub fn run_case(&self, case: &Case) -> Outcome {
        let skip = |reason: SkipReason| Outcome {
            case: case.clone(),
            verdict: Verdict::Skipped(reason),
            ours_command: String::new(),
            theirs_command: String::new(),
        };

        let Some(reference) = self.reference else {
            return skip(SkipReason::NoReference(if self.absent_reason.is_empty() {
                "no reference installation".to_owned()
            } else {
                self.absent_reason.clone()
            }));
        };
        let Some(ours_bin) = self.under_test.binary(case.tool) else {
            return skip(SkipReason::ToolNotBuilt(format!(
                "the `{}` binary under test is not built; set VACO_BIN_{} or \
                 `cargo build` it",
                case.tool,
                match case.tool {
                    Tool::Probe => "PROBE",
                    Tool::Transcode => "VACO",
                    Tool::PlayHeadless => "PLAY",
                }
            )));
        };
        let theirs_bin = match case.tool {
            Tool::Probe => &reference.ffprobe,
            Tool::Transcode | Tool::PlayHeadless => &reference.ffmpeg,
        };

        let dir = match tempfile::tempdir() {
            Ok(d) => d,
            Err(e) => {
                return Outcome {
                    case: case.clone(),
                    verdict: Verdict::OursFailed(FailureKind::LaunchFailed(e.to_string())),
                    ours_command: String::new(),
                    theirs_command: String::new(),
                };
            }
        };

        let mut argv = case.normalise.argv_prefix(case.tool);
        argv.extend(case.argv.iter().cloned());

        let ours_inv = Invocation::new(ours_bin, argv.clone())
            .in_dir(dir.path())
            .with_timeout(case.timeout);
        let theirs_inv = Invocation::new(theirs_bin, argv)
            .in_dir(dir.path())
            .with_timeout(case.timeout);

        let ours = match run(&ours_inv) {
            Ok(o) => o,
            Err(e) => {
                return Outcome {
                    case: case.clone(),
                    verdict: Verdict::OursFailed(FailureKind::LaunchFailed(e.to_string())),
                    ours_command: ours_inv.command_line(),
                    theirs_command: theirs_inv.command_line(),
                };
            }
        };
        let theirs = match run(&theirs_inv) {
            Ok(o) => o,
            Err(e) => {
                return Outcome {
                    case: case.clone(),
                    verdict: Verdict::ReferenceFailed(FailureKind::LaunchFailed(e.to_string())),
                    ours_command: ours_inv.command_line(),
                    theirs_command: theirs_inv.command_line(),
                };
            }
        };

        Outcome {
            verdict: compare::evaluate(
                case,
                &Pair {
                    ours: &ours,
                    theirs: &theirs,
                },
                self.allowlist,
            ),
            case: case.clone(),
            ours_command: ours_inv.command_line(),
            theirs_command: theirs_inv.command_line(),
        }
    }
}

/// The default per-case budget when a suite does not name one.
pub const DEFAULT_CASE_TIMEOUT: Duration = crate::run::DEFAULT_TIMEOUT;

#[cfg(test)]
#[expect(
    clippy::expect_used,
    clippy::panic,
    reason = "a failing expectation in a test is a failing test"
)]
mod tests {
    use super::{Runner, Tally, UnderTest};
    use crate::case::{Capture, Case, CaseId, Compare, Tier, Tool, Verdict};
    use crate::divergence::Allowlist;
    use crate::normalise::Chain;
    use std::time::Duration;

    fn a_case() -> Case {
        Case {
            id: CaseId::new("s", "m", &[]),
            tool: Tool::Probe,
            media: Vec::new(),
            argv: vec!["-show_pixel_formats".to_owned()],
            compare: Compare::ExactBytes {
                captures: vec![Capture::Stdout],
            },
            normalise: Chain::default(),
            requires: Vec::new(),
            timeout: Duration::from_secs(5),
            tier: Tier::Smoke,
        }
    }

    #[test]
    fn no_reference_skips_rather_than_fails() {
        let allow = Allowlist::parse("schema = 1\n", "2026-08-21").expect("loads");
        let runner = Runner::new(None, &allow);
        let outcome = runner.run_case(&a_case());
        assert!(matches!(outcome.verdict, Verdict::Skipped(_)));
        assert!(!outcome.verdict.is_failure(), "absence is not failure");
    }

    #[test]
    fn a_skip_message_tells_the_contributor_what_to_do() {
        let allow = Allowlist::parse("schema = 1\n", "2026-08-21").expect("loads");
        let mut runner = Runner::new(None, &allow);
        runner.absent_reason = "install FFmpeg 8.1".to_owned();
        match runner.run_case(&a_case()).verdict {
            Verdict::Skipped(reason) => assert!(reason.to_string().contains("install FFmpeg")),
            other => panic!("expected a skip, got {}", other.label()),
        }
    }

    #[test]
    fn tier_filtering_excludes_higher_tiers() {
        let allow = Allowlist::parse("schema = 1\n", "2026-08-21").expect("loads");
        let runner = Runner::new(None, &allow);
        let mut full = a_case();
        full.tier = Tier::Full;
        let (outcomes, tally) = runner.run_all(&[a_case(), full], Tier::Smoke);
        assert_eq!(outcomes.len(), 1);
        assert_eq!(tally.total(), 1);
    }

    #[test]
    fn a_tally_knows_when_it_is_failing() {
        let mut t = Tally {
            agreed: 10,
            skipped: 2,
            ..Tally::default()
        };
        assert!(!t.is_failing());
        assert!((t.skip_rate() - 2.0 / 12.0).abs() < 1e-9);
        t.diverged = 1;
        assert!(t.is_failing());
    }

    #[test]
    fn binary_discovery_never_panics_when_nothing_is_built() {
        let u = UnderTest::discover();
        // Whatever the answer is, asking must be safe.
        let _ = u.binary(Tool::Probe);
        let _ = u.binary(Tool::Transcode);
        let _ = u.binary(Tool::PlayHeadless);
    }
}
