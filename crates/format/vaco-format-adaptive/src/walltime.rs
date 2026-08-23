//! Wall-clock timestamps, parsed without ever calling the wall clock.
//!
//! Both formats are full of ISO 8601: HLS's `#EXT-X-PROGRAM-DATE-TIME`
//! (RFC 8216 §4.4.4.6, `YYYY-MM-DDThh:mm:ss.sssZ`) and DASH's
//! `availabilityStartTime`/`publishTime`/`<S>`-adjacent live-edge maths
//! (ISO/IEC 23009-1, `xs:dateTime`), plus DASH's `xs:duration` fields
//! (`minBufferTime`, `mediaPresentationDuration`, `@d` when a manifest states
//! a segment duration that way — `PT6.006S`).
//!
//! Parsing a string that *contains* a timestamp is pure arithmetic and needs
//! no clock. What would need one — "is this live segment inside the
//! availability window *right now*" — is a real question a live-DASH/live-HLS
//! implementation has to answer, and every answer requires calling
//! [`vaco_time::unix_nanos`] rather than `std::time::SystemTime::now()`, which
//! **panics** on `wasm32-unknown-unknown` (see `vaco-time`'s crate docs).
//! [`WallClock::now`] is that one door; nothing else in either demuxer calls a
//! clock, which is what `cargo xtask time-gate` checks for.

use vaco_core::Duration;

/// A point in time as microseconds since the Unix epoch.
///
/// Not [`std::time::SystemTime`]: that type cannot be constructed from a
/// parsed epoch offset without going through `UNIX_EPOCH + Duration`, which
/// works identically everywhere but is a heavier type to carry through a
/// playlist model than a plain integer, and every consumer here
/// (`EXT-X-PROGRAM-DATE-TIME` side data, `availabilityStartTime` arithmetic)
/// wants the integer anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WallClock(pub i128);

impl WallClock {
    /// The current wall-clock time, or `None` on a target with no clock —
    /// routed through `vaco-time`, never `std::time::SystemTime::now()`.
    #[must_use]
    #[allow(
        clippy::integer_division,
        reason = "truncating nanoseconds to microseconds"
    )]
    pub fn now() -> Option<Self> {
        vaco_time::unix_nanos().map(|ns| Self((ns / 1000).cast_signed()))
    }

    /// Microseconds since the epoch as a signed 64-bit count, saturating —
    /// what a [`vaco_format_core::StreamSideData`]-shaped field would carry.
    #[must_use]
    #[allow(
        clippy::cast_lossless,
        reason = "widening i64 -> i128; `From` is not yet usable in a const fn"
    )]
    pub const fn as_micros_i64(self) -> i64 {
        if self.0 > i64::MAX as i128 {
            i64::MAX
        } else if self.0 < i64::MIN as i128 {
            i64::MIN
        } else {
            self.0 as i64
        }
    }

    /// The instant `delta` after this one.
    #[must_use]
    #[allow(
        clippy::cast_lossless,
        reason = "widening i64 -> i128; `From` is not yet usable in a const fn"
    )]
    pub const fn add_micros(self, delta: i64) -> Self {
        Self(self.0 + delta as i128)
    }

    /// Whole microseconds between two instants; positive when `self` is
    /// later than `other`.
    #[must_use]
    pub const fn since(self, other: Self) -> i128 {
        self.0 - other.0
    }
}

/// Days from the civil epoch (1970-01-01) to `(y, m, d)`, using Howard
/// Hinnant's `days_from_civil` algorithm — proleptic Gregorian, valid for
/// every year an ISO 8601 timestamp can name, with no floating point and no
/// library dependency (no date/time crate is declared in
/// `[workspace.dependencies]`, so this is written out rather than adopted).
///
/// Every `/` below is Hinnant's algorithm exactly as published: truncating
/// integer division on values already normalised to be non-negative within
/// their era, not an approximation of a fraction.
#[allow(
    clippy::integer_division,
    reason = "Hinnant's days_from_civil: truncating division is the algorithm, not an approximation"
)]
const fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = (m as i64 + 9) % 12; // [0, 11], Mar=0 .. Feb=11
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Parse an ISO 8601 / RFC 3339 date-time: `YYYY-MM-DDThh:mm:ss[.fff…][Z|±hh:mm]`.
///
/// Accepts a space in place of `T` (seen in the wild from `strftime`-derived
/// tooling) and a bare fractional-second count of any length, truncating
/// beyond microsecond precision rather than rounding — timestamps in these
/// two formats are informational, and an off-by-one-microsecond value from
/// truncation is never the difference between two segments choosing
/// differently.
///
/// Returns `None` for anything that does not fit the grammar, rather than a
/// best-effort partial read: a demuxer that gets `EXT-X-PROGRAM-DATE-TIME`
/// wrong should have no timestamp, not a wrong one silently offset.
#[must_use]
pub fn parse_iso8601_datetime(s: &str) -> Option<WallClock> {
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    if s.as_bytes().get(4) != Some(&b'-') {
        return None;
    }
    let month: u32 = s.get(5..7)?.parse().ok()?;
    if s.as_bytes().get(7) != Some(&b'-') {
        return None;
    }
    let day: u32 = s.get(8..10)?.parse().ok()?;
    let sep = s.as_bytes().get(10)?;
    if *sep != b'T' && *sep != b't' && *sep != b' ' {
        return None;
    }
    let hour: i64 = s.get(11..13)?.parse().ok()?;
    if s.as_bytes().get(13) != Some(&b':') {
        return None;
    }
    let minute: i64 = s.get(14..16)?.parse().ok()?;
    if s.as_bytes().get(16) != Some(&b':') {
        return None;
    }
    let second: i64 = s.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&month) || day == 0 || day > 31 {
        return None;
    }

    let mut rest = s.get(19..)?;
    let mut micros: i64 = 0;
    if let Some(frac) = rest.strip_prefix('.') {
        let digits_len = frac
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(frac.len());
        let digits = frac.get(..digits_len)?;
        rest = frac.get(digits_len..)?;
        // Left-justify into 6 digits (microseconds), truncating past that.
        let mut buf = *b"000000";
        for (slot, byte) in buf.iter_mut().zip(digits.bytes().take(6)) {
            *slot = byte;
        }
        micros = std::str::from_utf8(&buf).ok()?.parse().ok()?;
    }

    let offset_seconds: i64 = if rest.is_empty() || rest.eq_ignore_ascii_case("z") {
        0
    } else {
        let sign = match rest.as_bytes().first()? {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        let tail = rest.get(1..)?;
        let (oh, om) = tail.split_once(':').or_else(|| tail.split_at_checked(2))?;
        let oh: i64 = oh.parse().ok()?;
        let om: i64 = om.parse().ok()?;
        sign * (oh * 3600 + om * 60)
    };

    let days = days_from_civil(year, month, day);
    let secs_of_day = hour * 3600 + minute * 60 + second - offset_seconds;
    let total_micros = i128::from(days * 86_400 + secs_of_day) * 1_000_000 + i128::from(micros);
    Some(WallClock(total_micros))
}

/// Format a [`WallClock`] as `EXT-X-PROGRAM-DATE-TIME`/`xs:dateTime` expects:
/// `YYYY-MM-DDThh:mm:ss.ffffffZ`, always UTC, always microsecond precision.
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "splitting a micros-since-epoch count into calendar fields; every division is exact by construction (each divisor evenly bounds the field above it)"
)]
pub fn format_iso8601_datetime(t: WallClock) -> String {
    let total_micros = t.0;
    let micros_of_day = total_micros.rem_euclid(86_400_000_000);
    let days_i128 = (total_micros - micros_of_day) / 86_400_000_000;
    let days = i64::try_from(days_i128).unwrap_or(if days_i128 < 0 { i64::MIN } else { i64::MAX });
    let (y, m, d) = civil_from_days(days);
    let secs_of_day = micros_of_day / 1_000_000;
    let micros = micros_of_day % 1_000_000;
    let hh = secs_of_day / 3600;
    let mm = (secs_of_day % 3600) / 60;
    let ss = secs_of_day % 60;
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}.{micros:06}Z")
}

/// Inverse of [`days_from_civil`], Hinnant's `civil_from_days`.
#[allow(
    clippy::integer_division,
    reason = "Hinnant's civil_from_days: truncating division is the algorithm, not an approximation"
)]
const fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m: i64 = if mp < 10 { mp + 3 } else { mp - 9 };
    let m = m as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Parse an `xs:duration` value: `PT#H#M#S`, `P#DT#H#M#S`, or any prefix of
/// that (DASH manifests use `PT` almost exclusively — `minBufferTime`,
/// `@d`-as-duration — but `mediaPresentationDuration` legitimately carries
/// days on a long VOD asset).
///
/// Years and months are refused (returns `None`): their length is
/// calendar-dependent, which a fixed-length [`Duration`] cannot represent
/// exactly, and no field either format actually uses needs them.
#[must_use]
pub fn parse_iso8601_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    let s = s.strip_prefix('P')?;
    let (date_part, time_part) = match s.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (s, None),
    };
    let mut micros: i64 = 0;
    let mut rest = date_part;
    while let Some((num, tag, tail)) = take_component(rest) {
        match tag {
            'D' => micros = micros.saturating_add(days_to_micros(num)),
            'W' => micros = micros.saturating_add(days_to_micros(num * 7.0)),
            _ => return None,
        }
        rest = tail;
    }
    if let Some(time_part) = time_part {
        let mut rest = time_part;
        while let Some((num, tag, tail)) = take_component(rest) {
            micros = match tag {
                'H' => micros.saturating_add((num * 3_600_000_000.0) as i64),
                'M' => micros.saturating_add((num * 60_000_000.0) as i64),
                'S' => micros.saturating_add((num * 1_000_000.0) as i64),
                _ => return None,
            };
            rest = tail;
        }
    }
    Some(Duration::from_micros(micros))
}

fn days_to_micros(days: f64) -> i64 {
    (days * 86_400_000_000.0) as i64
}

/// Split one `<number><letter>` component off the front of an `xs:duration`
/// fragment, returning the parsed number, the tag letter, and the remainder.
fn take_component(s: &str) -> Option<(f64, char, &str)> {
    if s.is_empty() {
        return None;
    }
    let end = s.find(|c: char| !c.is_ascii_digit() && c != '.')?;
    let num: f64 = s.get(..end)?.parse().ok()?;
    let tag = s.get(end..)?.chars().next()?;
    let tail = s.get(end + tag.len_utf8()..)?;
    Some((num, tag, tail))
}

/// Format a [`Duration`] as `xs:duration`: `PT#.###S`. Not attempted for
/// negative durations — an `xs:duration` may syntactically carry a leading
/// `-`, but nothing this workspace writes should ever produce one.
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "splitting a microsecond count into whole seconds and a fractional remainder for text formatting"
)]
pub fn format_iso8601_duration(d: Duration) -> String {
    let micros = d.as_micros().max(0);
    let whole = micros / 1_000_000;
    let frac = micros % 1_000_000;
    if frac == 0 {
        format!("PT{whole}S")
    } else {
        format!("PT{whole}.{frac:06}S")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn parses_the_hls_program_date_time_example() {
        // RFC 8216 §4.4.4.6's own example.
        let t = parse_iso8601_datetime("2010-02-19T14:54:23.031+08:00").unwrap();
        // Same instant, re-expressed in UTC by hand: 14:54:23.031+08:00 is
        // 06:54:23.031Z.
        assert_eq!(format_iso8601_datetime(t), "2010-02-19T06:54:23.031000Z");
    }

    #[test]
    fn parses_a_bare_z_and_no_fraction() {
        let t = parse_iso8601_datetime("2020-01-01T00:00:00Z").unwrap();
        assert_eq!(format_iso8601_datetime(t), "2020-01-01T00:00:00.000000Z");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_iso8601_datetime("not a date").is_none());
        assert!(parse_iso8601_datetime("2020-13-40T00:00:00Z").is_none());
    }

    #[test]
    fn epoch_round_trips() {
        let t = WallClock(0);
        assert_eq!(format_iso8601_datetime(t), "1970-01-01T00:00:00.000000Z");
        assert_eq!(parse_iso8601_datetime("1970-01-01T00:00:00Z").unwrap(), t);
    }

    #[test]
    fn duration_pt_h_m_s() {
        let d = parse_iso8601_duration("PT1H2M3.5S").unwrap();
        assert_eq!(d.as_micros(), (3600 + 120 + 3) * 1_000_000 + 500_000);
    }

    #[test]
    fn duration_seconds_only_matches_dash_segment_durations() {
        // A real `@duration`/`@d`-as-xs:duration value seen from `ffmpeg -f
        // dash`.
        let d = parse_iso8601_duration("PT6.006S").unwrap();
        assert_eq!(d.as_micros(), 6_006_000);
    }

    #[test]
    fn duration_with_days() {
        let d = parse_iso8601_duration("P1DT1S").unwrap();
        assert_eq!(d.as_micros(), 86_400_000_000 + 1_000_000);
    }

    #[test]
    fn years_and_months_are_refused() {
        assert!(parse_iso8601_duration("P1Y").is_none());
        assert!(parse_iso8601_duration("P1M").is_none());
    }

    #[test]
    fn format_duration_round_trips_through_parse() {
        let d = Duration::from_micros(6_006_000);
        assert_eq!(format_iso8601_duration(d), "PT6.006000S");
        assert_eq!(parse_iso8601_duration(&format_iso8601_duration(d)), Some(d));
    }

    proptest::proptest! {
        #[test]
        fn datetime_round_trips_at_microsecond_precision(micros in -60_000_000_000_000_000i128..60_000_000_000_000_000i128) {
            let t = WallClock(micros);
            let s = format_iso8601_datetime(t);
            let back = parse_iso8601_datetime(&s).unwrap();
            proptest::prop_assert_eq!(back, t);
        }
    }
}
