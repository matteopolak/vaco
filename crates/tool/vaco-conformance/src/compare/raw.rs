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

use crate::case::{Case, Tolerance, Verdict};
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

/// C5 — [`compare`], but every byte pair may differ by up to
/// `tolerance.max_abs` (raw pixel bytes have no sign, so "absolute
/// difference" is just `(a as i32 - b as i32).abs()`) and/or the stream's
/// overall root-mean-square difference may not exceed `tolerance.max_rms`.
/// Both are checked when both are non-zero; either alone is enough to
/// admit a byte-for-byte pass through `is_zero`'s "no tolerance named"
/// case, which behaves exactly like [`compare`].
///
/// `max_ulp` has no meaning here — the `filter` tool's raw stream is plane
/// bytes (`u8` pixel samples via [`crate::filterexec`]), not an IEEE float
/// stream, so a case naming a non-zero `max_ulp` is a case that meant a
/// different comparator and gets a named error rather than a silently
/// ignored field.
///
/// # Errors
/// Never returns `Err` today — reserved because a future caller may want
/// to reject a `max_ulp`-only tolerance before running the case at all
/// rather than after, the same way [`crate::case::Compare::from_manifest`]
/// rejects a missing `justification` at load time.
#[must_use]
pub fn compare_tolerant(case: &Case, pair: &Pair<'_>, tolerance: &Tolerance) -> Verdict {
    if tolerance.max_ulp != 0 {
        return Verdict::Divergence(DiffReport {
            mode: case.compare.mode_name(),
            summary: format!(
                "max_ulp={} is not meaningful for the filter tool's raw u8 pixel-byte stream (no \
                 IEEE float representation to count ULPs in) -- use max_abs/max_rms instead \
                 (justification: {})",
                tolerance.max_ulp,
                justification(case)
            ),
            ..DiffReport::default()
        });
    }
    let ours = &pair.ours.stdout;
    let theirs = &pair.theirs.stdout;
    if tolerance.is_zero() {
        return compare(case, pair);
    }
    if ours.len() != theirs.len() {
        return Verdict::Divergence(DiffReport {
            mode: case.compare.mode_name(),
            summary: format!(
                "raw output length differs: ours {} bytes, reference {} bytes",
                ours.len(),
                theirs.len()
            ),
            ..DiffReport::default()
        });
    }
    let mut worst: Option<(usize, f64)> = None;
    let mut sq_sum = 0.0f64;
    for (i, (&a, &b)) in ours.iter().zip(theirs.iter()).enumerate() {
        let diff = (f64::from(a) - f64::from(b)).abs();
        sq_sum += diff * diff;
        if worst.is_none_or(|(_, w)| diff > w) {
            worst = Some((i, diff));
        }
    }
    let Some((worst_at, worst_diff)) = worst else {
        return Verdict::Agree;
    };
    #[allow(
        clippy::cast_precision_loss,
        reason = "byte-stream lengths never approach f64's exact-integer ceiling"
    )]
    let rms = (sq_sum / ours.len() as f64).sqrt();
    if tolerance.max_abs > 0.0 && worst_diff > tolerance.max_abs {
        return Verdict::Divergence(DiffReport {
            mode: case.compare.mode_name(),
            summary: format!(
                "raw output exceeds max_abs={} at byte {worst_at}: |{worst_diff}| (justification: \
                 {})",
                tolerance.max_abs,
                justification(case)
            ),
            excerpt: excerpt(ours, theirs, worst_at),
            ..DiffReport::default()
        });
    }
    if tolerance.max_rms > 0.0 && rms > tolerance.max_rms {
        return Verdict::Divergence(DiffReport {
            mode: case.compare.mode_name(),
            summary: format!(
                "raw output exceeds max_rms={}: measured {rms} (justification: {})",
                tolerance.max_rms,
                justification(case)
            ),
            ..DiffReport::default()
        });
    }
    Verdict::Agree
}

/// The `justification` string, for a `RawTolerant` case — panics on any
/// other `Compare` variant, since only [`compare_tolerant`]'s own caller in
/// [`crate::compare::evaluate`] ever reaches this, always with a
/// `RawTolerant` case.
fn justification(case: &Case) -> &str {
    match &case.compare {
        crate::case::Compare::RawTolerant { justification, .. } => justification,
        other => unreachable!(
            "compare_tolerant called on non-tolerant mode `{}`",
            other.mode_name()
        ),
    }
}

/// A short window of both streams around the first difference, hex-rendered
/// — pixel bytes are not text, so unlike [`crate::compare::exact`]'s excerpt
/// there is no line/column to report, only an offset and a neighbourhood.
fn excerpt(ours: &[u8], theirs: &[u8], at: usize) -> String {
    const WINDOW: usize = 8;
    let start = at.saturating_sub(WINDOW);
    let hex = |buf: &[u8], from: usize| -> String {
        buf.get(from..(from + WINDOW * 2).min(buf.len()))
            .map(|w| {
                w.iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
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
