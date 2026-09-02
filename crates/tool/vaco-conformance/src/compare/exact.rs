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
use crate::compare::{DiffReport, FieldDiff, Pair, wants};

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
            // Every differing line, not just the first: `exact-bytes`'s own
            // byte comparison above still decides pass/fail (this report is
            // reached only once that has already found *a* difference), but
            // stopping the *report* at the first byte hid every further
            // divergence in the same case behind it -- a missing field
            // shifts every following line, so the true count of what a fix
            // needs to move was invisible until the first one was already
            // fixed and run again. `line_diffs` is a real (LCS) line-level
            // diff, not a positional zip, so one inserted/removed line does
            // not cascade into reporting every line after it as changed too.
            fields: line_diffs(&ours, &theirs),
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

/// Every line one side has that the other does not, or has differently — a
/// real (LCS) alignment, not a zip of the two line lists by position.
///
/// A positional zip breaks the moment one side has an extra or a missing
/// line: everything after it lines up one position off, and a single
/// removed field would be reported as every subsequent line "changing" even
/// though most of them did not. Aligning on the longest common subsequence
/// first means an insertion or deletion is reported as exactly that one
/// line, and everything the two sides do share in order is recognised as
/// shared, however far apart the one real difference pushes it.
///
/// Ordering is deterministic (the LCS of two fixed inputs is not unique in
/// general, but this table's backtrack always prefers "diagonal" — both
/// lines equal — over either single-sided step, which is what makes running
/// it twice on the same two inputs produce the same alignment both times,
/// the property a diffable, stable report needs).
///
/// `O(n*m)` in line count. Every case this runs against is one command's
/// `-show_streams`/`-show_format` output — at most a few hundred lines —
/// not a corpus-scale text, so the quadratic table is the right tool, not a
/// reason to reach for a `diff` crate D10 has not reviewed.
#[must_use]
pub fn line_diffs(ours: &[u8], theirs: &[u8]) -> Vec<FieldDiff> {
    let a: Vec<&str> = std::str::from_utf8(ours)
        .map(|s| s.lines().collect())
        .unwrap_or_default();
    let b: Vec<&str> = std::str::from_utf8(theirs)
        .map(|s| s.lines().collect())
        .unwrap_or_default();
    let (n, m) = (a.len(), b.len());

    // `lcs[i][j]` = length of the LCS of `a[i..]` and `b[j..]`.
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            let Some(row) = lcs.get(i + 1) else { continue };
            let diag = row.get(j + 1).copied().unwrap_or(0);
            let Some(cur_row) = lcs.get_mut(i) else { continue };
            if a.get(i) == b.get(j) {
                if let Some(slot) = cur_row.get_mut(j) {
                    *slot = diag.saturating_add(1);
                }
            } else {
                let up = cur_row.get(j + 1).copied().unwrap_or(0);
                let Some(down_row) = lcs.get(i + 1) else { continue };
                let down = down_row.get(j).copied().unwrap_or(0);
                if let Some(slot) = lcs.get_mut(i).and_then(|r| r.get_mut(j)) {
                    *slot = up.max(down);
                }
            }
        }
    }

    let mut out = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    // 1-based, matching `line_col`'s convention -- these numbers are meant
    // to be read next to a text editor's own line numbers.
    let (mut ours_line, mut theirs_line) = (1usize, 1usize);
    while i < n && j < m {
        if a.get(i) == b.get(j) {
            i += 1;
            j += 1;
            ours_line += 1;
            theirs_line += 1;
            continue;
        }
        let here = lcs.get(i).and_then(|r| r.get(j)).copied().unwrap_or(0);
        let skip_a = lcs.get(i + 1).and_then(|r| r.get(j)).copied().unwrap_or(0);
        let skip_b = lcs.get(i).and_then(|r| r.get(j + 1)).copied().unwrap_or(0);
        if skip_a == here && skip_a >= skip_b {
            out.push(FieldDiff {
                section: None,
                field: format!("line {ours_line}"),
                ours: a.get(i).unwrap_or(&"").to_string(),
                theirs: "<absent>".to_owned(),
            });
            i += 1;
            ours_line += 1;
        } else {
            out.push(FieldDiff {
                section: None,
                field: format!("line {theirs_line}"),
                ours: "<absent>".to_owned(),
                theirs: b.get(j).unwrap_or(&"").to_string(),
            });
            j += 1;
            theirs_line += 1;
        }
    }
    for line in a.get(i..).unwrap_or_default() {
        out.push(FieldDiff {
            section: None,
            field: format!("line {ours_line}"),
            ours: (*line).to_string(),
            theirs: "<absent>".to_owned(),
        });
        ours_line += 1;
    }
    for line in b.get(j..).unwrap_or_default() {
        out.push(FieldDiff {
            section: None,
            field: format!("line {theirs_line}"),
            ours: "<absent>".to_owned(),
            theirs: (*line).to_string(),
        });
        theirs_line += 1;
    }
    merge_adjacent_replacements(out)
}

/// Fold an adjacent (delete, insert) or (insert, delete) pair produced at
/// the same point in the walk above into one "changed" entry with both real
/// values, rather than two entries that each show one side as `<absent>`.
/// A real edit at one line reads as a single line changing, not as a line
/// vanishing right next to an unrelated one appearing — the same line
/// number recurring with opposite `<absent>` sides is exactly a replacement
/// in disguise.
fn merge_adjacent_replacements(diffs: Vec<FieldDiff>) -> Vec<FieldDiff> {
    let mut out: Vec<FieldDiff> = Vec::new();
    let mut pending: Option<FieldDiff> = None;
    for d in diffs {
        let Some(prev) = pending.take() else {
            pending = Some(d);
            continue;
        };
        let prev_is_delete = prev.theirs == "<absent>";
        let prev_is_insert = prev.ours == "<absent>";
        let merges = (prev_is_delete && d.ours == "<absent>" && d.theirs != "<absent>")
            || (prev_is_insert && d.theirs == "<absent>" && d.ours != "<absent>");
        if merges {
            out.push(if prev_is_delete {
                FieldDiff { theirs: d.theirs, ..prev }
            } else {
                FieldDiff { ours: d.ours, ..prev }
            });
        } else {
            out.push(prev);
            pending = Some(d);
        }
    }
    if let Some(last) = pending {
        out.push(last);
    }
    out
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
    use super::{first_difference, line_col, line_diffs};
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

    /// A missing/inserted line must not cascade into reporting every line
    /// after it as "changed" -- the exact failure mode a positional zip has
    /// and an LCS alignment does not.
    #[test]
    fn one_inserted_line_reports_as_exactly_one_line_not_a_cascade() {
        let ours = b"a=1\nb=2\nd=4\n";
        let theirs = b"a=1\nb=2\nc=3\nd=4\n";
        let diffs = line_diffs(ours, theirs);
        assert_eq!(diffs.len(), 1, "{diffs:?}");
        let Some(d) = diffs.first() else {
            panic!("expected exactly one diff, got {diffs:?}")
        };
        assert!(d.theirs.contains("c=3"), "{diffs:?}");
        assert_eq!(d.ours, "<absent>");
    }

    #[test]
    fn one_removed_line_reports_as_exactly_one_line() {
        let ours = b"a=1\nb=2\nc=3\nd=4\n";
        let theirs = b"a=1\nb=2\nd=4\n";
        let diffs = line_diffs(ours, theirs);
        assert_eq!(diffs.len(), 1, "{diffs:?}");
        let Some(d) = diffs.first() else {
            panic!("expected exactly one diff, got {diffs:?}")
        };
        assert!(d.ours.contains("c=3"), "{diffs:?}");
        assert_eq!(d.theirs, "<absent>");
    }

    #[test]
    fn a_changed_line_reports_both_values_not_an_insert_plus_a_delete() {
        let ours = b"a=1\nb=2\nc=3\n";
        let theirs = b"a=1\nb=9\nc=3\n";
        let diffs = line_diffs(ours, theirs);
        assert_eq!(diffs.len(), 1, "{diffs:?}");
        let Some(d) = diffs.first() else {
            panic!("expected exactly one diff, got {diffs:?}")
        };
        assert_eq!(d.ours, "b=2");
        assert_eq!(d.theirs, "b=9");
    }

    #[test]
    fn identical_lines_report_no_diffs() {
        assert!(line_diffs(b"a=1\nb=2\n", b"a=1\nb=2\n").is_empty());
    }

    #[test]
    fn running_the_same_two_inputs_twice_produces_the_same_alignment() {
        let ours = b"a=1\nx=9\nb=2\ny=9\nc=3\n";
        let theirs = b"a=1\nb=2\nc=3\n";
        let first = line_diffs(ours, theirs);
        let second = line_diffs(ours, theirs);
        assert_eq!(first, second);
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
