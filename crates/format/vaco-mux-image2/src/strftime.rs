//! `-strftime 1`: expand a filename pattern against the wall clock.
//!
//! Routed through `vaco_time::unix_nanos()` rather than
//! `std::time::SystemTime::now()` directly, because the latter panics on
//! `wasm32-unknown-unknown` — see `vaco-time`'s own docs and
//! `planning/AGENT-CONSTRAINTS.md`'s wasm note. On a target with no wall
//! clock (wasm without the `web` feature), [`expand`] reports
//! [`vaco_core::Error::Unsupported`] rather than inventing a fake date.
//!
//! # Scope
//!
//! A small, explicit subset of C `strftime`: the directives below. Anything
//! else passes through literally (including the `%`), which is safer than
//! guessing and matches this crate's general approach to under-specified
//! reference behaviour — a script depending on an unimplemented directive
//! gets a wrong-but-visible filename, not a substituted wrong value.
//!
//! | Directive | Meaning |
//! |---|---|
//! | `%Y` | four-digit year |
//! | `%y` | two-digit year |
//! | `%m` | month, `01`-`12` |
//! | `%d` | day of month, `01`-`31` |
//! | `%H` | hour, `00`-`23` |
//! | `%M` | minute, `00`-`59` |
//! | `%S` | second, `00`-`59` |
//! | `%j` | day of year, `001`-`366` |
//! | `%F` | `%Y-%m-%d` |
//! | `%T` | `%H:%M:%S` |
//! | `%%` | a literal `%` |

use vaco_core::{Error, Result};

/// A broken-down UTC calendar date and time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Civil {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    day_of_year: u32,
}

/// Howard Hinnant's `civil_from_days` (public domain, `date_algorithms.html`)
/// — a closed-form Gregorian calendar conversion with no lookup table and no
/// loop, so it is exact for any `days` a `u128` nanosecond count can produce.
#[allow(
    clippy::integer_division,
    reason = "closed-form calendar conversion (Hinnant's civil_from_days): every \
              division here is exact floor arithmetic from the published algorithm, \
              not a lossy approximation"
)]
fn civil_from_unix_seconds(total_secs: i64) -> Civil {
    let seconds_of_day = total_secs.rem_euclid(86_400);
    let days = (total_secs - seconds_of_day) / 86_400;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe.cast_signed() + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };

    let hour = (seconds_of_day / 3600) as u32;
    let minute = ((seconds_of_day % 3600) / 60) as u32;
    let second = (seconds_of_day % 60) as u32;

    // Day of year via the same closed form, re-based to Jan 1 of `year`.
    let jan1 = days_from_civil(year, 1, 1);
    let day_of_year = u32::try_from(days - jan1).unwrap_or(0).saturating_add(1);

    Civil {
        year,
        month: m,
        day: d,
        hour,
        minute,
        second,
        day_of_year,
    }
}

/// The inverse of [`civil_from_unix_seconds`]'s date half: days since the
/// Unix epoch for a Gregorian `(year, month, day)`. Same source algorithm.
#[allow(
    clippy::integer_division,
    reason = "closed-form calendar conversion (Hinnant's days_from_civil): every \
              division here is exact floor arithmetic from the published algorithm, \
              not a lossy approximation"
)]
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64; // [0, 399]
    let mp = if month > 2 { month - 3 } else { month + 9 }; // [0, 11]
    let doy = (153 * mp + 2) / 5 + day - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + u64::from(doy); // [0, 146096]
    era * 146_097 + doe.cast_signed() - 719_468
}

/// Expand `pattern`'s strftime directives against the current wall clock.
///
/// # Errors
/// [`Error::Unsupported`] when this target has no wall clock.
pub fn expand_now(pattern: &str) -> Result<String> {
    let Some(nanos) = vaco_time::unix_nanos() else {
        return Err(Error::Unsupported(
            "strftime filename expansion needs a wall clock, and this target has none",
        ));
    };
    #[allow(
        clippy::integer_division,
        reason = "flooring a nanosecond count to whole seconds"
    )]
    let whole_secs = nanos / 1_000_000_000;
    let secs = i64::try_from(whole_secs).unwrap_or(i64::MAX);
    Ok(expand(pattern, civil_from_unix_seconds(secs)))
}

fn expand(pattern: &str, c: Civil) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(pattern.len());
    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        // `write!` into a `String` cannot fail; the result is deliberately
        // discarded rather than propagated.
        let _ = match chars.next() {
            Some('Y') => write!(out, "{:04}", c.year),
            Some('y') => write!(out, "{:02}", c.year.rem_euclid(100)),
            Some('m') => write!(out, "{:02}", c.month),
            Some('d') => write!(out, "{:02}", c.day),
            Some('H') => write!(out, "{:02}", c.hour),
            Some('M') => write!(out, "{:02}", c.minute),
            Some('S') => write!(out, "{:02}", c.second),
            Some('j') => write!(out, "{:03}", c.day_of_year),
            Some('F') => write!(out, "{:04}-{:02}-{:02}", c.year, c.month, c.day),
            Some('T') => write!(out, "{:02}:{:02}:{:02}", c.hour, c.minute, c.second),
            Some('%') | None => {
                out.push('%');
                Ok(())
            }
            Some(other) => {
                out.push('%');
                out.push(other);
                Ok(())
            }
        };
    }
    out
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::integer_division,
    reason = "test code; the division mirrors the exact floor arithmetic under test"
)]
mod tests {
    use super::*;

    fn civil(year: i64, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> Civil {
        let secs = days_from_civil(year, month, day) * 86_400
            + i64::from(hour) * 3600
            + i64::from(minute) * 60
            + i64::from(second);
        civil_from_unix_seconds(secs)
    }

    #[test]
    fn epoch_is_1970_01_01() {
        let c = civil_from_unix_seconds(0);
        assert_eq!(
            (c.year, c.month, c.day, c.hour, c.minute, c.second),
            (1970, 1, 1, 0, 0, 0)
        );
        assert_eq!(c.day_of_year, 1);
    }

    #[test]
    fn a_known_date_round_trips() {
        // 2024-03-05 06:07:08 UTC, cross-checked against `date -u -d@...`.
        let secs = 1_709_618_828i64;
        let c = civil_from_unix_seconds(secs);
        assert_eq!((c.year, c.month, c.day), (2024, 3, 5));
        assert_eq!((c.hour, c.minute, c.second), (6, 7, 8));
    }

    #[test]
    fn leap_day_survives_the_round_trip() {
        let c = civil(2024, 2, 29, 12, 0, 0);
        assert_eq!((c.year, c.month, c.day), (2024, 2, 29));
    }

    #[test]
    fn expand_substitutes_every_documented_directive() {
        let c = civil(2024, 3, 5, 6, 7, 8);
        assert_eq!(expand("%Y%m%d-%H%M%S", c), "20240305-060708");
        assert_eq!(expand("%F_%T", c), "2024-03-05_06:07:08");
        assert_eq!(expand("%y", c), "24");
        assert_eq!(expand("100%%", c), "100%");
    }

    #[test]
    fn unrecognised_directive_passes_through_literally() {
        let c = civil(2024, 1, 1, 0, 0, 0);
        assert_eq!(expand("%q", c), "%q");
    }

    #[test]
    fn expand_never_panics_on_a_trailing_percent() {
        let c = civil(2024, 1, 1, 0, 0, 0);
        assert_eq!(expand("out%", c), "out%");
    }

    proptest::proptest! {
        /// Every `total_secs` (including negative — pre-1970) decodes to a
        /// valid calendar date whose own `days_from_civil` maps back to the
        /// same day, for the whole `i32`-scale range this format cares about.
        #[test]
        fn civil_from_unix_seconds_never_panics(secs in -100_000_000_000i64..100_000_000_000i64) {
            let c = civil_from_unix_seconds(secs);
            let seconds_of_day = secs.rem_euclid(86_400);
            let days = (secs - seconds_of_day) / 86_400;
            let back = days_from_civil(c.year, c.month, c.day);
            proptest::prop_assert_eq!(back, days);
        }
    }
}
