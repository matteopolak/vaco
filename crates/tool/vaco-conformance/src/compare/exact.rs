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
    Verdict::Agree
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
        match super::compare(
            &c,
            &Pair {
                ours: &a,
                theirs: &b,
            },
            &[Capture::Stdout],
        ) {
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
        let v = super::compare(
            &c,
            &Pair {
                ours: &a,
                theirs: &b,
            },
            &[Capture::Stdout],
        );
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
        let v = super::compare(
            &c,
            &Pair {
                ours: &a,
                theirs: &b,
            },
            &[Capture::Stdout],
        );
        assert!(matches!(v, Verdict::Agree), "normalisation must apply");
    }
}
