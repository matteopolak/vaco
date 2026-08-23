//! C0 / C1 — byte equality (plan 13 §1.2).
//!
//! # What it is
//!
//! The strongest comparison the harness has, and the D5 acceptance criterion
//! for every `vaco-probe` writer: identical bytes, including field order,
//! spacing, escaping and `N/A` handling.
//!
//! # How it works
//!
//! The declared output normalisers run first (that is the only difference
//! between C0 and C1 — C1 exists so a reviewer sees the chain named in the
//! manifest). Then the streams are compared byte for byte. On a difference the
//! report names the byte offset, the line, and the column, and prints a short
//! window either side, because "42 bytes differ" is not something anyone can
//! act on.
//!
//! `capture = ["output-file"]` compares the file a transcode case wrote via an
//! `{output}` token (see [`crate::runner::Runner::run_case`]), not a captured
//! stream. It is always compared as raw bytes with **no** output normaliser
//! applied — every declared [`crate::normalise::Output`] variant is
//! text-shaped (line endings, float spelling, stderr severity), and running
//! one over an arbitrary container's bytes would coincidentally rewrite real
//! data rather than hide a meaningless difference.
//!
//! # How to change it
//!
//! [`excerpt`] is the part worth tuning. Widen the window, do not soften the
//! comparison.

use crate::case::{Capture, Case, Verdict};
use crate::compare::{DiffReport, Pair, wants};

/// Compare the declared captures byte for byte.
#[must_use]
pub fn compare(case: &Case, pair: &Pair<'_>, captures: &[Capture]) -> Verdict {
    let mode = case.compare.mode_name();
    for (capture, ours_raw, theirs_raw, label) in [
        (
            Capture::Stdout,
            &pair.ours.stdout,
            &pair.theirs.stdout,
            "stdout",
        ),
        (
            Capture::Stderr,
            &pair.ours.stderr,
            &pair.theirs.stderr,
            "stderr",
        ),
    ] {
        if !wants(captures, capture) {
            continue;
        }
        let ours = normalised(case, ours_raw);
        let theirs = normalised(case, theirs_raw);
        if ours == theirs {
            continue;
        }
        let at = first_difference(&ours, &theirs);
        let (line, col) = line_col(&ours, at);
        return Verdict::Divergence(DiffReport {
            mode,
            summary: format!(
                "{label} differs at byte {at} (line {line}, column {col}); \
                 ours {} bytes, reference {} bytes",
                ours.len(),
                theirs.len()
            ),
            excerpt: excerpt(&ours, &theirs, at),
            ..DiffReport::default()
        });
    }

    if wants(captures, Capture::OutputFile)
        && let Some(report) = compare_output_file(mode, pair)
    {
        return Verdict::Divergence(report);
    }

    Verdict::Agree
}

/// The `output-file` half of [`compare`], split out because it reasons about
/// presence as well as content: a case can declare `output-file` without
/// either side having written one (nothing to compare — not this
/// comparator's business, `exit-code` already covers "did it run"), but a
/// case where **one** side wrote a file and the other did not is exactly the
/// silent-success failure mode §6 of `planning/CONFORMANCE-FINDINGS.md`
/// records: exit 0, a plausible summary, and no file.
fn compare_output_file(mode: &'static str, pair: &Pair<'_>) -> Option<DiffReport> {
    match (pair.ours_output_file, pair.theirs_output_file) {
        (None, None) => None,
        (Some(ours), Some(theirs)) if ours == theirs => None,
        (Some(ours), Some(theirs)) => {
            let at = first_difference(ours, theirs);
            Some(DiffReport {
                mode,
                summary: format!(
                    "output file differs at byte {at}; ours {} bytes, reference {} bytes",
                    ours.len(),
                    theirs.len()
                ),
                excerpt: excerpt(ours, theirs, at),
                ..DiffReport::default()
            })
        }
        (None, Some(theirs)) => Some(DiffReport {
            mode,
            summary: format!(
                "we wrote no output file; the reference wrote {} bytes",
                theirs.len()
            ),
            ..DiffReport::default()
        }),
        (Some(ours), None) => Some(DiffReport {
            mode,
            summary: format!(
                "the reference wrote no output file; we wrote {} bytes",
                ours.len()
            ),
            ..DiffReport::default()
        }),
    }
}

fn normalised(case: &Case, raw: &[u8]) -> Vec<u8> {
    if case.normalise.output.is_empty() {
        return raw.to_vec();
    }
    let text = String::from_utf8_lossy(raw);
    case.normalise.apply_output(&text).into_bytes()
}

/// Offset of the first differing byte, or the length of the shorter stream.
#[must_use]
pub fn first_difference(a: &[u8], b: &[u8]) -> usize {
    a.iter()
        .zip(b.iter())
        .position(|(x, y)| x != y)
        .unwrap_or_else(|| a.len().min(b.len()))
}

/// 1-based line and column of `offset`.
#[must_use]
#[expect(
    clippy::naive_bytecount,
    reason = "this runs once per failing case, not per byte of a stream; a \
              dependency on `bytecount` for it would not clear D10"
)]
pub fn line_col(data: &[u8], offset: usize) -> (usize, usize) {
    let upto = data.get(..offset.min(data.len())).unwrap_or_default();
    let line = 1 + upto.iter().filter(|b| **b == b'\n').count();
    let col = 1 + upto.iter().rev().take_while(|b| **b != b'\n').count();
    (line, col)
}

/// A readable window either side of the divergence.
#[must_use]
pub fn excerpt(ours: &[u8], theirs: &[u8], at: usize) -> String {
    const WINDOW: usize = 48;
    let start = at.saturating_sub(WINDOW);
    let render = |data: &[u8]| -> String {
        let end = (at + WINDOW).min(data.len());
        let slice = data.get(start..end).unwrap_or_default();
        String::from_utf8_lossy(slice)
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    };
    format!(
        "    ours   | {}\n    theirs | {}\n    {}^ byte {at}",
        render(ours),
        render(theirs),
        " ".repeat(at - start + 11)
    )
}

#[cfg(test)]
#[expect(
    clippy::panic,
    reason = "a failing assertion in a test is a failing test"
)]
mod tests {
    use super::{first_difference, line_col};
    use crate::case::{Capture, Compare, Verdict};
    use crate::compare::Pair;
    use crate::compare::tests::{case, obs};

    #[test]
    fn locates_the_first_differing_byte() {
        assert_eq!(first_difference(b"abcd", b"abXd"), 2);
        assert_eq!(first_difference(b"abc", b"abcd"), 3);
        assert_eq!(first_difference(b"", b"a"), 0);
    }

    #[test]
    fn reports_line_and_column() {
        assert_eq!(line_col(b"a\nbb\nccc", 0), (1, 1));
        assert_eq!(line_col(b"a\nbb\nccc", 2), (2, 1));
        assert_eq!(line_col(b"a\nbb\nccc", 6), (3, 2));
    }

    #[test]
    fn a_single_byte_difference_is_reported_precisely() {
        let c = case(Compare::ExactBytes {
            captures: vec![Capture::Stdout],
        });
        let a = obs("format_name=mov\n", Some(0));
        let b = obs("format_name=mp4\n", Some(0));
        match super::compare(&c, &Pair::new(&a, &b), &[Capture::Stdout]) {
            Verdict::Divergence(report) => {
                assert!(report.summary.contains("byte 13"), "{}", report.summary);
                assert!(report.excerpt.contains("ours"), "{}", report.excerpt);
            }
            other => panic!("expected a divergence, got {}", other.label()),
        }
    }

    #[test]
    fn a_capture_that_is_not_declared_is_not_compared() {
        let c = case(Compare::ExactBytes {
            captures: vec![Capture::Stdout],
        });
        let mut a = obs("same", Some(0));
        a.stderr = b"ours".to_vec();
        let mut b = obs("same", Some(0));
        b.stderr = b"theirs".to_vec();
        let v = super::compare(&c, &Pair::new(&a, &b), &[Capture::Stdout]);
        assert!(matches!(v, Verdict::Agree));
    }

    #[test]
    fn declared_normalisers_run_before_the_comparison() {
        let mut c = case(Compare::ExactBytesNormalised {
            captures: vec![Capture::Stdout],
        });
        c.normalise.output = vec![crate::normalise::Output::LineEndings];
        let a = obs("a\r\nb\r\n", Some(0));
        let b = obs("a\nb\n", Some(0));
        let v = super::compare(&c, &Pair::new(&a, &b), &[Capture::Stdout]);
        assert!(matches!(v, Verdict::Agree), "normalisation must apply");
    }

    #[test]
    fn identical_output_files_agree() {
        let c = case(Compare::ExactBytes {
            captures: vec![Capture::OutputFile],
        });
        let a = obs("", Some(0));
        let b = obs("", Some(0));
        let mut pair = Pair::new(&a, &b);
        pair.ours_output_file = Some(b"same bytes");
        pair.theirs_output_file = Some(b"same bytes");
        let v = super::compare(&c, &pair, &[Capture::OutputFile]);
        assert!(matches!(v, Verdict::Agree), "{v:?}");
    }

    #[test]
    fn differing_output_files_diverge() {
        let c = case(Compare::ExactBytes {
            captures: vec![Capture::OutputFile],
        });
        let a = obs("", Some(0));
        let b = obs("", Some(0));
        let mut pair = Pair::new(&a, &b);
        pair.ours_output_file = Some(b"ours");
        pair.theirs_output_file = Some(b"theirs");
        match super::compare(&c, &pair, &[Capture::OutputFile]) {
            Verdict::Divergence(report) => {
                assert!(
                    report.summary.contains("output file differs"),
                    "{}",
                    report.summary
                );
            }
            other => panic!("expected a divergence, got {}", other.label()),
        }
    }

    #[test]
    fn a_file_only_one_side_wrote_is_the_silent_success_failure_mode() {
        let c = case(Compare::ExactBytes {
            captures: vec![Capture::OutputFile],
        });
        let a = obs("", Some(0));
        let b = obs("", Some(0));
        let mut pair = Pair::new(&a, &b);
        pair.theirs_output_file = Some(b"reference wrote this");
        // ours_output_file stays None: exit 0, no file — exactly finding #6.
        match super::compare(&c, &pair, &[Capture::OutputFile]) {
            Verdict::Divergence(report) => {
                assert!(
                    report.summary.contains("we wrote no output file"),
                    "{}",
                    report.summary
                );
            }
            other => panic!("expected a divergence, got {}", other.label()),
        }
    }

    #[test]
    fn a_case_that_names_no_output_file_on_either_side_is_not_this_comparators_business() {
        let c = case(Compare::ExactBytes {
            captures: vec![Capture::OutputFile],
        });
        let a = obs("", Some(0));
        let b = obs("", Some(0));
        let v = super::compare(&c, &Pair::new(&a, &b), &[Capture::OutputFile]);
        assert!(matches!(v, Verdict::Agree), "{v:?}");
    }
}
