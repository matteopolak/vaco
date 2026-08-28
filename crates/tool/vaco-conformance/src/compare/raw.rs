//! C4 — `raw-exact`: full byte equality of decoded raw pixel data (plan 13
//! §1.2).
//!
//! # What it is
//!
//! The filter-tool analogue of [`crate::compare::exact`]'s C0: `ours.stdout`
//! and `theirs.stdout` are compared byte for byte, no normaliser, because
//! raw pixel data has no invocation-banner or wall-clock text to normalise
//! away in the first place. The difference from C0 is only in what
//! populated `stdout` — for a `filter`-tool case it is a plane-by-plane raw
//! frame dump built in-process by [`crate::filterexec`], not a subprocess's
//! captured stream — `evaluate` and this comparator do not need to know
//! that.
//!
//! # How to change it
//!
//! [`excerpt`] renders a byte-offset window, the same as C0's. A future pass
//! that wants pixel/row/plane-shaped diagnostics needs the frame's geometry,
//! which this mode's manifest fields do not carry (see
//! [`crate::filterexec`]'s own doc for where that lives) — passed in as an
//! argument here, not smuggled onto [`crate::case::Compare::RawExact`]
//! itself, which stays a unit variant on purpose (C4 is "are the bytes
//! equal", not "how is this shaped").

use crate::case::{Case, Verdict};
use crate::compare::{DiffReport, Pair};

/// Compare `ours.stdout` against `theirs.stdout`, byte for byte.
#[must_use]
pub fn compare(case: &Case, pair: &Pair<'_>) -> Verdict {
    let ours = &pair.ours.stdout;
    let theirs = &pair.theirs.stdout;
    if ours == theirs {
        return Verdict::Agree;
    }
    let at = ours
        .iter()
        .zip(theirs.iter())
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| ours.len().min(theirs.len()));
    Verdict::Divergence(DiffReport {
        mode: case.compare.mode_name(),
        summary: format!(
            "raw output differs at byte {at}; ours {} bytes, reference {} bytes",
            ours.len(),
            theirs.len()
        ),
        excerpt: excerpt(ours, theirs, at),
        ..DiffReport::default()
    })
}

/// A short window of both streams around the first difference, hex-rendered
/// — pixel bytes are not text, so unlike [`crate::compare::exact`]'s excerpt
/// there is no line/column to report, only an offset and a neighbourhood.
fn excerpt(ours: &[u8], theirs: &[u8], at: usize) -> String {
    const WINDOW: usize = 8;
    let start = at.saturating_sub(WINDOW);
    let hex = |buf: &[u8], from: usize| -> String {
        buf.get(from..(from + WINDOW * 2).min(buf.len()))
            .map(|w| w.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "))
            .unwrap_or_default()
    };
    format!(
        "byte {at}:\n  ours:    {}\n  theirs:  {}",
        hex(ours, start),
        hex(theirs, start)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::case::Compare;
    use crate::compare::tests::{case, obs};

    #[test]
    fn identical_raw_streams_agree() {
        let c = case(Compare::RawExact);
        let a = obs("same-bytes", Some(0));
        let b = obs("same-bytes", Some(0));
        assert!(matches!(compare(&c, &Pair::new(&a, &b)), Verdict::Agree));
    }

    #[test]
    fn one_differing_byte_is_a_divergence_not_a_pass() {
        let c = case(Compare::RawExact);
        let a = obs("aaaa", Some(0));
        let b = obs("aaba", Some(0));
        let v = compare(&c, &Pair::new(&a, &b));
        assert!(matches!(v, Verdict::Divergence(_)), "{v:?}");
    }

    #[test]
    fn different_lengths_are_a_divergence() {
        let c = case(Compare::RawExact);
        let a = obs("short", Some(0));
        let b = obs("much longer than that", Some(0));
        let v = compare(&c, &Pair::new(&a, &b));
        assert!(matches!(v, Verdict::Divergence(_)), "{v:?}");
    }
}
