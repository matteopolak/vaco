//! Byte-level line splitting that does not require the input to be valid
//! UTF-8.
//!
//! Every format in this family is line-oriented, and every parser needs to
//! walk lines before it can decide whether the file is even well-formed. Using
//! `str::lines()` would mean validating the whole file as UTF-8 first and
//! rejecting it wholesale on one bad byte in one cue's text — which is exactly
//! the byte-exact passthrough [`crate::encoding`] documents the reference
//! *not* doing. This module walks `\n`-terminated lines over `&[u8]` instead,
//! trimming a trailing `\r` the way every line-oriented text format on every
//! platform expects.

/// Split `input` into lines on `\n`, each with a trailing `\r` trimmed.
///
/// A final line with no trailing `\n` is still yielded (files without a
/// trailing newline are common and not malformed). An empty input yields no
/// lines at all, not one empty line — this matches `str::lines()`.
#[must_use]
pub fn lines(input: &[u8]) -> Vec<&[u8]> {
    if input.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, &b) in input.iter().enumerate() {
        if b == b'\n' {
            out.push(trim_cr(input.get(start..i).unwrap_or(&[])));
            start = i + 1;
        }
    }
    if start < input.len() {
        out.push(trim_cr(input.get(start..).unwrap_or(&[])));
    }
    out
}

/// Trim one trailing `\r`, if present.
#[must_use]
pub fn trim_cr(line: &[u8]) -> &[u8] {
    match line.split_last() {
        Some((b'\r', rest)) => rest,
        _ => line,
    }
}

/// Trim ASCII whitespace (space, tab, `\r`, `\n`, form feed, vertical tab)
/// from both ends of a byte slice.
///
/// A byte-level stand-in for `str::trim` used by parsers that must not assume
/// the slice is valid UTF-8.
#[must_use]
pub fn trim_ascii(input: &[u8]) -> &[u8] {
    let is_ws = |b: &u8| b.is_ascii_whitespace();
    let start = input.iter().position(|b| !is_ws(b)).unwrap_or(input.len());
    let end = input
        .iter()
        .rposition(|b| !is_ws(b))
        .map_or(start, |i| i + 1);
    input.get(start..end).unwrap_or(&[])
}

/// Split `input` into blank-line-separated blocks (one or more blank lines
/// between blocks), the grouping `SubRip`, `SubViewer` and several other formats
/// use to delimit one cue from the next.
///
/// A block never contains an internal blank line and is never itself empty:
/// runs of blank lines collapse to a single separator, and leading/trailing
/// blank lines are dropped.
#[must_use]
pub fn blocks(input: &[u8]) -> Vec<Vec<&[u8]>> {
    let mut out = Vec::new();
    let mut current: Vec<&[u8]> = Vec::new();
    for line in lines(input) {
        if trim_ascii(line).is_empty() {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Join `lines` back together with `\n`, the inverse of splitting a block
/// into lines. Used to reassemble a cue's multi-line text from the lines that
/// follow its timing line.
#[must_use]
pub fn join_lines(lines: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            out.push(b'\n');
        }
        out.extend_from_slice(line);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn lines_splits_on_lf_and_trims_cr() {
        assert_eq!(lines(b"a\r\nb\nc"), vec![b"a".as_slice(), b"b", b"c"]);
    }

    #[test]
    fn lines_of_empty_input_is_empty() {
        assert!(lines(b"").is_empty());
    }

    #[test]
    fn lines_keeps_a_trailing_line_with_no_newline() {
        assert_eq!(lines(b"a\nb"), vec![b"a".as_slice(), b"b"]);
        assert_eq!(lines(b"a\n"), vec![b"a".as_slice()]);
    }

    #[test]
    fn trim_ascii_strips_both_ends() {
        assert_eq!(trim_ascii(b"  \t hi \r\n"), b"hi");
        assert_eq!(trim_ascii(b"   "), b"");
    }

    #[test]
    fn blocks_splits_on_blank_lines_and_collapses_runs() {
        let got = blocks(b"1\nfoo\n\n\n2\nbar\nbaz\n");
        assert_eq!(
            got,
            vec![
                vec![b"1".as_slice(), b"foo"],
                vec![b"2".as_slice(), b"bar", b"baz"]
            ]
        );
    }

    #[test]
    fn blocks_of_empty_input_is_empty() {
        assert!(blocks(b"\n\n\n").is_empty());
    }

    #[test]
    fn join_lines_is_the_inverse_of_lines_for_lf_only_input() {
        let input: &[u8] = b"a\nbb\nccc";
        assert_eq!(join_lines(&lines(input)), input);
    }

    #[test]
    fn join_lines_of_empty_slice_is_empty() {
        assert!(join_lines(&[]).is_empty());
    }

    #[test]
    fn never_panics_on_arbitrary_bytes() {
        for len in 0..40 {
            let buf: Vec<u8> = (0..len).map(|i| (i * 61 % 256) as u8).collect();
            let _ = lines(&buf);
            let _ = blocks(&buf);
            let _ = trim_ascii(&buf);
        }
    }
}
