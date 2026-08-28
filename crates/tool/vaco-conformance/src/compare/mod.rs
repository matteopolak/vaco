//! The comparators (plan 13 §1.2).
//!
//! # What it is
//!
//! Given two [`Observation`]s and a [`Compare`] mode, produce a [`Verdict`].
//! This module owns the dispatch and the shared [`DiffReport`]; the modes
//! themselves live in submodules.
//!
//! # What is implemented, and what is a seam
//!
//! | Mode | State |
//! |---|---|
//! | C0 `exact-bytes`, C1 `exact-bytes-normalised` | implemented ([`exact`]) |
//! | C4 `raw-exact` | implemented ([`raw::compare`]) — the `filter`-tool's own byte-equality mode, fed by [`crate::filterexec`] rather than a subprocess |
//! | C5 `raw-tolerant` | implemented ([`raw::compare_tolerant`]) — `max_abs`/`max_rms` over the same raw byte stream; `max_ulp` has no meaning for `u8` pixel bytes and is a named error, not a silent no-op |
//! | C6 `structured-diff` | implemented ([`structured`]) |
//! | C7 `behavioural` | implemented (outcome class only) |
//! | C10 `quality-band` | **seam only** ([`quality`]) — the metrics are not written |
//! | C2, C3, C8, C9 | seams; they need machinery from crates that do not exist yet |
//!
//! An unimplemented mode returns [`Verdict::Skipped`] with
//! [`SkipReason::ModeUnimplemented`], never a false pass. That distinction is
//! the whole reason the seams are typed rather than left as `todo!()`: a suite
//! that declares C4 today reports "not implemented" in the run summary, and the
//! skip budget (§1.5.4) makes that visible instead of silently green.
//!
//! # How to change it
//!
//! Implementing a mode means filling in its arm here and its submodule. Do not
//! implement a mode by downgrading it — CI enforces that a case may not move
//! from C0 to C6 without an allowlist entry (§1.2 C6).

pub mod exact;
pub mod quality;
pub mod raw;
pub mod structured;

use std::fmt;

use crate::case::{Capture, Case, Compare, FailureKind, SkipReason, Verdict};
use crate::divergence::Allowlist;
use crate::run::Observation;

/// One field-level difference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDiff {
    /// Section the field belongs to, where the format has sections.
    pub section: Option<String>,
    /// The field, box path, or byte offset that differs.
    pub field: String,
    /// Our value, rendered.
    pub ours: String,
    /// The reference's value, rendered.
    pub theirs: String,
}

impl fmt::Display for FieldDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.section {
            Some(s) => write!(f, "{s}.{}", self.field)?,
            None => write!(f, "{}", self.field)?,
        }
        write!(f, ": ours={:?} theirs={:?}", self.ours, self.theirs)
    }
}

/// Everything a human needs to understand a failing case.
#[derive(Debug, Clone, Default)]
pub struct DiffReport {
    /// Which mode produced it.
    pub mode: &'static str,
    /// One line summarising the failure.
    pub summary: String,
    /// Field-level differences, when the mode produces them.
    pub fields: Vec<FieldDiff>,
    /// A rendered excerpt, when the mode is byte-oriented.
    pub excerpt: String,
    /// Differences that an allowlist entry admitted.
    pub allowed: Vec<crate::divergence::DivergenceId>,
}

impl DiffReport {
    /// Whether anything unexplained remains.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty() && self.excerpt.is_empty() && self.summary.is_empty()
    }
}

impl fmt::Display for DiffReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{} — {}", self.mode, self.summary)?;
        for d in &self.fields {
            writeln!(f, "  {d}")?;
        }
        if !self.excerpt.is_empty() {
            writeln!(f, "{}", self.excerpt)?;
        }
        if !self.allowed.is_empty() {
            let ids: Vec<&str> = self.allowed.iter().map(|d| d.0.as_str()).collect();
            writeln!(f, "  (also allowed: {})", ids.join(", "))?;
        }
        Ok(())
    }
}

/// The two sides of one case.
#[derive(Debug)]
pub struct Pair<'a> {
    /// What our binary produced.
    pub ours: &'a Observation,
    /// What the reference produced.
    pub theirs: &'a Observation,
    /// Bytes of the file our side wrote, when the case names an `{output}`
    /// token (transcode cases only). `None` both when the case wrote no file
    /// and when it was supposed to but did not — [`exact::compare`]
    /// distinguishes those by also consulting `capture = ["output-file"]`.
    pub ours_output_file: Option<&'a [u8]>,
    /// The reference's equivalent of [`Pair::ours_output_file`].
    pub theirs_output_file: Option<&'a [u8]>,
}

impl<'a> Pair<'a> {
    /// A pair with no output file captured — the common case for `probe`.
    #[must_use]
    pub const fn new(ours: &'a Observation, theirs: &'a Observation) -> Self {
        Self {
            ours,
            theirs,
            ours_output_file: None,
            theirs_output_file: None,
        }
    }
}

/// Compare one case's observations.
///
/// The exit code is a co-assertion on **every byte-comparing mode** (§1.2
/// C0: "exit codes, always, as a co-assertion"), checked before the
/// mode-specific comparison: a case where the two binaries disagree about
/// success has already failed, whatever the bytes say.
///
/// `behavioural` (C7) is deliberately exempted from *this* literal check.
/// §1.2 C7 itself is defined as comparing "the class of the outcome... not
/// the message text", and [`outcome_class`] exists precisely to be coarser
/// than a numeric exit code — two independent codebases essentially never
/// choose the same integer for "I rejected this input" (measured: `vaco`
/// and `ffmpeg` produced 183, 218, 234, 0 across a ten-case suite with no
/// two failing codes matching by anything but coincidence). A pre-check that
/// demanded literal equality first would make every C7 case fail on exactly
/// the inputs C7 exists to cover, and would make `outcome_class`'s whole
/// reason for existing unreachable code. `behavioural` still receives the
/// exit codes — [`behavioural`] reads them via [`outcome_class`] — it is
/// only the *literal-equality short circuit* that does not apply to it.
#[must_use]
pub fn evaluate(case: &Case, pair: &Pair<'_>, allow: &Allowlist) -> Verdict {
    if pair.ours.timed_out {
        return Verdict::OursFailed(FailureKind::Timeout);
    }
    if pair.theirs.timed_out {
        return Verdict::ReferenceFailed(FailureKind::Timeout);
    }
    if !matches!(case.compare, Compare::Behavioural) && pair.ours.exit != pair.theirs.exit {
        return Verdict::Divergence(DiffReport {
            mode: case.compare.mode_name(),
            summary: format!(
                "exit codes differ: ours {:?}, reference {:?}",
                pair.ours.exit, pair.theirs.exit
            ),
            ..DiffReport::default()
        });
    }

    match &case.compare {
        Compare::ExactBytes { captures } | Compare::ExactBytesNormalised { captures } => {
            exact::compare(case, pair, captures)
        }
        Compare::StructuredDiff { writer } => structured::compare(case, pair, allow, writer),
        Compare::Behavioural => behavioural(case, pair),
        Compare::QualityBand { band } => quality::compare(case, pair, band),
        Compare::RawExact => raw::compare(case, pair),
        Compare::RawTolerant { tolerance, .. } => raw::compare_tolerant(case, pair, tolerance),
        Compare::ContainerStructure { .. }
        | Compare::FrameHash { .. }
        | Compare::CrossDecode { .. }
        | Compare::ThreeWay { .. } => {
            Verdict::Skipped(SkipReason::ModeUnimplemented(case.compare.mode_name()))
        }
    }
}

/// C7 — outcome class only.
fn behavioural(case: &Case, pair: &Pair<'_>) -> Verdict {
    let ours = outcome_class(pair.ours);
    let theirs = outcome_class(pair.theirs);
    if ours == theirs {
        return Verdict::Agree;
    }
    Verdict::Divergence(DiffReport {
        mode: case.compare.mode_name(),
        summary: "outcome class differs".to_owned(),
        fields: vec![FieldDiff {
            section: None,
            field: "outcome".to_owned(),
            ours: ours.to_owned(),
            theirs: theirs.to_owned(),
        }],
        ..DiffReport::default()
    })
}

/// The class of an outcome: accepted, rejected, or crashed. Deliberately not
/// the message text — our error prose is ours (§1.3.2 `stderr-class`).
#[must_use]
pub fn outcome_class(obs: &Observation) -> &'static str {
    if obs.timed_out {
        return "timeout";
    }
    match obs.exit {
        Some(0) if obs.stdout.is_empty() => "accepted-empty",
        Some(0) => "accepted",
        Some(_) => "rejected",
        None => "signalled",
    }
}

/// Whether a capture set asks for a given stream.
#[must_use]
pub fn wants(captures: &[Capture], c: Capture) -> bool {
    captures.contains(&c)
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "a failing expectation in a test is a failing test"
)]
mod tests {
    use super::{Pair, evaluate, outcome_class};
    use crate::case::{Capture, Case, CaseId, Compare, Tier, Tool, Verdict};
    use crate::divergence::Allowlist;
    use crate::normalise::Chain;
    use crate::run::Observation;
    use std::time::Duration;

    pub(super) fn obs(stdout: &str, exit: Option<i32>) -> Observation {
        Observation {
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
            exit,
            timed_out: false,
            truncated: false,
            wall: Duration::from_millis(1),
        }
    }

    pub(super) fn case(compare: Compare) -> Case {
        Case {
            id: CaseId::new("t", "m", &[]),
            tool: Tool::Probe,
            media: Vec::new(),
            argv: Vec::new(),
            compare,
            normalise: Chain::default(),
            requires: Vec::new(),
            timeout: Duration::from_secs(5),
            tier: Tier::Smoke,
        }
    }

    fn empty_allowlist() -> Allowlist {
        Allowlist::parse("schema = 1\n", "2026-08-21").expect("empty register loads")
    }

    #[test]
    fn identical_output_agrees() {
        let c = case(Compare::ExactBytes {
            captures: vec![Capture::Stdout, Capture::ExitCode],
        });
        let a = obs("hello", Some(0));
        let b = obs("hello", Some(0));
        let v = evaluate(&c, &Pair::new(&a, &b), &empty_allowlist());
        assert!(matches!(v, Verdict::Agree), "{}", v.label());
    }

    #[test]
    fn differing_exit_codes_fail_every_mode() {
        let c = case(Compare::Behavioural);
        let a = obs("", Some(0));
        let b = obs("", Some(1));
        let v = evaluate(&c, &Pair::new(&a, &b), &empty_allowlist());
        assert!(v.is_failure());
    }

    #[test]
    fn a_byte_comparing_mode_demands_the_exact_same_exit_code() {
        // Two different *failing* codes: C0's literal co-assertion still
        // applies here, unlike C7 below.
        let c = case(Compare::ExactBytes {
            captures: vec![Capture::Stdout],
        });
        let a = obs("", Some(183));
        let b = obs("", Some(234));
        let v = evaluate(&c, &Pair::new(&a, &b), &empty_allowlist());
        assert!(v.is_failure(), "{}", v.label());
    }

    #[test]
    fn behavioural_agrees_when_both_sides_reject_with_different_codes() {
        // Measured on the transcode remux matrix: `vaco` and `ffmpeg` refuse
        // the same known-incompatible (input, output) pairs but with
        // unrelated numeric exit codes (183, 218, 234, ...). §1.2 C7 exists
        // to be coarser than exact exit-code equality — outcome class
        // (accepted / rejected / signalled) is the thing being compared, not
        // the literal integer.
        let c = case(Compare::Behavioural);
        let a = obs("", Some(183));
        let b = obs("", Some(234));
        let v = evaluate(&c, &Pair::new(&a, &b), &empty_allowlist());
        assert!(matches!(v, Verdict::Agree), "{}", v.label());
    }

    #[test]
    fn behavioural_still_diverges_across_the_accept_reject_boundary() {
        let c = case(Compare::Behavioural);
        let a = obs("", Some(0));
        let b = obs("", Some(183));
        let v = evaluate(&c, &Pair::new(&a, &b), &empty_allowlist());
        assert!(v.is_failure(), "{}", v.label());
    }

    #[test]
    fn unimplemented_modes_skip_rather_than_pass() {
        // `raw-exact` moved out of this list once `filterexec`/`raw::compare`
        // implemented it (see `compare::raw`'s own tests for its coverage
        // now) — `frame-hash` is still a genuine seam.
        let c = case(Compare::FrameHash {
            variant: "framecrc".to_owned(),
        });
        let a = obs("x", Some(0));
        let b = obs("y", Some(0));
        let v = evaluate(&c, &Pair::new(&a, &b), &empty_allowlist());
        assert_eq!(
            v.label(),
            "skipped",
            "an unimplemented mode must never pass"
        );
        assert!(!v.is_failure());
    }

    #[test]
    fn behavioural_ignores_prose_but_not_class() {
        let c = case(Compare::Behavioural);
        let mut a = obs("", Some(1));
        a.stderr = b"our wording".to_vec();
        let mut b = obs("", Some(1));
        b.stderr = b"their wording".to_vec();
        let v = evaluate(&c, &Pair::new(&a, &b), &empty_allowlist());
        assert!(matches!(v, Verdict::Agree));
        assert_eq!(outcome_class(&a), "rejected");
    }

    #[test]
    fn a_timeout_on_our_side_is_our_failure() {
        let c = case(Compare::Behavioural);
        let mut a = obs("", None);
        a.timed_out = true;
        let b = obs("", Some(0));
        let v = evaluate(&c, &Pair::new(&a, &b), &empty_allowlist());
        assert_eq!(v.label(), "OURS-FAILED");
    }
}
