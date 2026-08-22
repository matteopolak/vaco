//! Run reporting (plan 13 §1.5.2, §1.4.3).
//!
//! # What it is
//!
//! Turning verdicts into something a human acts on. The design constraint is
//! stated in the plan and it is worth repeating: *reproduction friction is the
//! main reason differential harnesses get ignored*. Every failure therefore
//! ends with the exact single-case command that re-runs it, and both argument
//! vectors, before any of the diff detail.
//!
//! # How to change it
//!
//! [`render_run`] is the human format. A machine format (JSON for CI
//! annotations, HTML for `just conformance-report`) belongs beside it, not
//! instead of it — the terminal output is what people read while working.

use std::fmt::Write as _;

use crate::case::Verdict;
use crate::divergence::Allowlist;
use crate::extract::TableReport;
use crate::refbin::Reference;
use crate::runner::{Outcome, Tally};

/// The header every run prints: which oracle, and whether it gates.
#[must_use]
pub fn render_reference(reference: Option<&Reference>, absent: &str) -> String {
    let Some(r) = reference else {
        return format!("reference: ABSENT — {absent}\n");
    };
    let mut s = format!(
        "reference: {} ({})\n  binary:  {}\n  channel: {} ({})\n",
        r.version,
        r.banner,
        r.ffmpeg.display(),
        r.channel,
        if r.gates() {
            "gating"
        } else {
            "advisory — failures are reported, not blocking"
        }
    );
    if !r.gates() {
        let _ = write!(
            s,
            "  NOTE: this is not the pinned gating version. Results are advisory.\n\
             \x20       Set VACO_CONFORMANCE_STRICT=1 to make that a hard error.\n"
        );
    }
    s
}

/// The whole run, human-readable.
#[must_use]
pub fn render_run(outcomes: &[Outcome], tally: Tally, allow: &Allowlist) -> String {
    let mut s = String::new();
    for outcome in outcomes {
        match &outcome.verdict {
            Verdict::Agree | Verdict::AllowedDivergence(_) => {}
            Verdict::Skipped(reason) => {
                let _ = writeln!(s, "skip  {}  ({reason})", outcome.case.id);
            }
            Verdict::Divergence(report) => {
                let _ = writeln!(s, "\nFAIL  {}", outcome.case.id);
                let _ = writeln!(s, "  reproduce: {}", outcome.case.reproduction());
                let _ = writeln!(s, "  ours:      {}", outcome.ours_command);
                let _ = writeln!(s, "  reference: {}", outcome.theirs_command);
                let _ = write!(s, "{report}");
            }
            Verdict::OursFailed(kind) => {
                let _ = writeln!(s, "\nFAIL  {}  (ours {kind})", outcome.case.id);
                let _ = writeln!(s, "  reproduce: {}", outcome.case.reproduction());
            }
            Verdict::ReferenceFailed(kind) => {
                let _ = writeln!(
                    s,
                    "\nFAIL  {}  (reference {kind} — usually means the case is wrong)",
                    outcome.case.id
                );
                let _ = writeln!(s, "  reproduce: {}", outcome.case.reproduction());
            }
        }
    }
    let _ = write!(s, "\n{}", render_tally(tally));
    let _ = write!(s, "{}", render_allowlist_health(allow, tally));
    s
}

/// The counts line.
#[must_use]
pub fn render_tally(t: Tally) -> String {
    format!(
        "{} cases: {} agreed, {} allowed, {} diverged, {} failed, {} skipped ({:.1}% skip rate)\n",
        t.total(),
        t.agreed,
        t.allowed,
        t.diverged,
        t.failed,
        t.skipped,
        t.skip_rate() * 100.0
    )
}

/// Mechanisms 4 and 6 of §1.4.3, reported every run.
#[must_use]
pub fn render_allowlist_health(allow: &Allowlist, tally: Tally) -> String {
    let mut s = String::new();
    let counts = allow.live_counts();
    if !counts.is_empty() {
        let parts: Vec<String> = counts.iter().map(|(cat, n)| format!("{cat}={n}")).collect();
        let _ = writeln!(s, "divergence register: {}", parts.join(" "));
    }
    let dead = allow.dead_entries();
    if !dead.is_empty() {
        let _ = writeln!(
            s,
            "  {} entries suppressed nothing this run (candidates for deletion):",
            dead.len()
        );
        for e in dead {
            let _ = writeln!(s, "    {} {} (review_by {})", e.id, e.title, e.review_by);
        }
    }
    for (entry, share) in allow.blast_radius(tally.total(), 0.02) {
        let _ = writeln!(
            s,
            "  {} suppresses {:.1}% of this run — scope may be too broad",
            entry.id,
            share * 100.0
        );
    }
    s
}

/// The table-extractor section.
#[must_use]
pub fn render_tables(reports: &[TableReport]) -> String {
    let mut s = String::new();
    let mut findings = 0_usize;
    for r in reports {
        let _ = write!(s, "{r}");
        findings += r.finding_count();
    }
    let _ = writeln!(
        s,
        "\n{findings} unexplained finding(s) across {} table check(s)",
        reports.len()
    );
    s
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "a failing expectation in a test is a failing test"
)]
mod tests {
    use super::{render_reference, render_run, render_tally};
    use crate::case::{Capture, Case, CaseId, Compare, Tier, Tool, Verdict};
    use crate::compare::DiffReport;
    use crate::divergence::Allowlist;
    use crate::normalise::Chain;
    use crate::runner::{Outcome, Tally};
    use std::time::Duration;

    fn outcome(verdict: Verdict) -> Outcome {
        Outcome {
            case: Case {
                id: CaseId::new(
                    "probe-listings",
                    "none",
                    &[("writer".into(), "json".into())],
                ),
                tool: Tool::Probe,
                media: Vec::new(),
                argv: Vec::new(),
                compare: Compare::ExactBytes {
                    captures: vec![Capture::Stdout],
                },
                normalise: Chain::default(),
                requires: Vec::new(),
                timeout: Duration::from_secs(1),
                tier: Tier::Smoke,
            },
            verdict,
            ours_command: "vaco-probe -of json".to_owned(),
            theirs_command: "ffprobe -of json".to_owned(),
        }
    }

    #[test]
    fn a_failure_leads_with_the_reproduction_command() {
        let allow = Allowlist::parse("schema = 1\n", "2026-08-21").expect("loads");
        let out = render_run(
            &[outcome(Verdict::Divergence(DiffReport {
                mode: "exact-bytes",
                summary: "stdout differs at byte 3".to_owned(),
                ..DiffReport::default()
            }))],
            Tally {
                diverged: 1,
                ..Tally::default()
            },
            &allow,
        );
        let repro_line = out
            .lines()
            .position(|l| l.contains("just conformance-run"))
            .expect("the reproduction command is printed");
        let detail_line = out
            .lines()
            .position(|l| l.contains("differs at byte"))
            .expect("the detail is printed");
        assert!(
            repro_line < detail_line,
            "reproduction must come before the detail"
        );
    }

    #[test]
    fn agreeing_cases_are_not_printed_one_by_one() {
        let allow = Allowlist::parse("schema = 1\n", "2026-08-21").expect("loads");
        let out = render_run(
            &[outcome(Verdict::Agree)],
            Tally {
                agreed: 1,
                ..Tally::default()
            },
            &allow,
        );
        assert!(!out.contains("probe-listings/none"), "{out}");
        assert!(out.contains("1 agreed"));
    }

    #[test]
    fn an_absent_reference_is_explained_not_hidden() {
        let s = render_reference(None, "install FFmpeg 8.1");
        assert!(s.contains("ABSENT"));
        assert!(s.contains("install FFmpeg 8.1"));
    }

    #[test]
    fn the_tally_line_reports_the_skip_rate() {
        let s = render_tally(Tally {
            agreed: 3,
            skipped: 1,
            ..Tally::default()
        });
        assert!(s.contains("25.0% skip rate"), "{s}");
    }
}
