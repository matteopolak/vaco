//! `-strftime`'s filename expansion: a deliberately small subset.
//!
//! # Why `vaco-time` and not `std::time`
//!
//! `SystemTime::now()`/`Instant::now()` panic on `wasm32-unknown-unknown` —
//! this crate's brief calls that out specifically. [`vaco_time::unix_nanos`]
//! is the one door through, returning `None` there instead of panicking; a
//! segment created on that target with `-strftime` gets the literal pattern
//! back unexpanded rather than a crash (see [`expand_now`]).
//!
//! # Supported specifiers
//!
//! `%Y %m %d %H %M %S %%`, computed from UTC (there is no timezone database
//! reachable from this crate, and the reference itself uses the local
//! timezone via libc — a difference this crate accepts rather than adding a
//! timezone dependency for). Every other `%x` passes through literally,
//! matching [`crate::segment::pattern::expand_index`]'s own leniency.
//!
//! The civil-calendar arithmetic is Howard Hinnant's `civil_from_days`
//! (a widely published, public-domain algorithm for proleptic-Gregorian
//! days-since-epoch <-> (y, m, d); not reproduced from any specific
//! implementation's source, just the well-known closed-form arithmetic). It
//! is exact-integer sexagesimal/calendar arithmetic throughout — every `/`
//! below is a place-value or calendar-epoch division by a literal constant,
//! never a computed quotient standing in for a float — which is what earns
//! it the same `integer_division` exemption `vaco_core::parse`'s duration
//! formatter already documents for the identical reason.

use core::fmt::Write as _;

/// UTC `(year, month, day, hour, minute, second)` from a Unix timestamp in
/// nanoseconds.
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "exact sexagesimal decomposition against literal constants (1e9, 86400, 3600, 60), not a computed quotient"
)]
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    reason = "a Unix timestamp in seconds fits i64 for every date this century either side of 1970, which is the whole domain `-strftime` filenames are used over"
)]
#[allow(
    clippy::many_single_char_names,
    reason = "h, m, s, y, d are hours/minutes/seconds/year/day, the conventional civil-calendar names"
)]
pub fn civil_from_unix_nanos(nanos: u128) -> (i64, u32, u32, u32, u32, u32) {
    let secs = (nanos / 1_000_000_000) as i64;
    let days = secs.div_euclid(86_400);
    let of_day = secs.rem_euclid(86_400);
    let (h, m, s) = (
        (of_day / 3600) as u32,
        ((of_day / 60) % 60) as u32,
        (of_day % 60) as u32,
    );
    let (y, mo, d) = civil_from_days(days);
    (y, mo, d, h, m, s)
}

/// Days-since-epoch (1970-01-01) to `(year, month, day)`, proleptic
/// Gregorian. `days` may be negative.
#[allow(
    clippy::integer_division,
    reason = "Hinnant's civil_from_days: every division is by a literal calendar constant (146097-day eras, 365-day years, the 153-day 5-month cycle), not a computed quotient"
)]
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "yoe/doy/mp/d/m are bounded ([0,399], [0,365], [0,11], [1,31], [1,12] respectively, per Hinnant's algorithm) well inside u32/i64"
)]
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe.cast_signed() + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Expand `%Y %m %d %H %M %S %%` in `pattern` using `nanos` (Unix time, UTC).
#[must_use]
pub fn expand(pattern: &str, nanos: u128) -> String {
    let (y, mo, d, h, mi, s) = civil_from_unix_nanos(nanos);
    let mut out = String::with_capacity(pattern.len() + 8);
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('%') => {
                chars.next();
                out.push('%');
            }
            Some('Y') => {
                chars.next();
                let _ = write!(out, "{y:04}");
            }
            Some('m') => {
                chars.next();
                let _ = write!(out, "{mo:02}");
            }
            Some('d') => {
                chars.next();
                let _ = write!(out, "{d:02}");
            }
            Some('H') => {
                chars.next();
                let _ = write!(out, "{h:02}");
            }
            Some('M') => {
                chars.next();
                let _ = write!(out, "{mi:02}");
            }
            Some('S') => {
                chars.next();
                let _ = write!(out, "{s:02}");
            }
            _ => out.push('%'),
        }
    }
    out
}

/// [`expand`] using the current wall-clock time, or `pattern` unchanged if
/// [`vaco_time::unix_nanos`] returns `None` (no clock available — wasm).
#[must_use]
pub fn expand_now(pattern: &str) -> String {
    vaco_time::unix_nanos().map_or_else(|| pattern.to_owned(), |n| expand(pattern, n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_epoch_second_decomposes_correctly() {
        // 2024-01-15 12:34:56 UTC = 1705322096
        let nanos = 1_705_322_096_000_000_000u128;
        assert_eq!(civil_from_unix_nanos(nanos), (2024, 1, 15, 12, 34, 56));
    }

    #[test]
    fn epoch_zero_is_1970_01_01() {
        assert_eq!(civil_from_unix_nanos(0), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn expand_formats_every_supported_specifier() {
        let nanos = 1_705_322_096_000_000_000u128;
        assert_eq!(
            expand("seg-%Y%m%d-%H%M%S.ts", nanos),
            "seg-20240115-123456.ts"
        );
    }

    #[test]
    fn literal_percent_and_unknown_specifiers_pass_through() {
        let nanos = 0u128;
        assert_eq!(expand("100%% %q done", nanos), "100% %q done");
    }
}
