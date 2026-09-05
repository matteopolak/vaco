//! `-segment_list_type`: `flat`, `csv`, `m3u8`/`hls`, `ext`, `ffconcat`.
//!
//! Measured (`ffmpeg -h muxer=segment`): the named values are `flat` (0),
//! `csv` (1), `m3u8`/`hls` (2), `ext` (3), `ffconcat` (4), with an unnamed
//! default of `-1`. This crate's default ([`SegmentListType::Flat`]) is a
//! judgement call, not a probe of the reference's `-1` — no observation
//! distinguished "unset" from "flat" behaviourally in the time this crate
//! had to probe with.
//!
//! `m3u8` is the one format with real external structure (an HLS media
//! playlist, RFC 8216) and is implemented against that structure directly.
//! `csv`'s and `ext`'s exact column layout were not independently
//! confirmed against the reference — see [`SegmentListType::Csv`] and
//! [`SegmentListType::Ext`] for the columns this crate assumes. `ffconcat`
//! reuses [`crate::concat::script`]'s own grammar for the write side, which
//! gives it a real round-trip property test: every ffconcat list this crate
//! writes parses back with [`crate::concat::script::parse`].

use core::fmt::Write as _;
use vaco_core::Duration;

/// One completed segment, everything a list line needs.
#[derive(Debug, Clone, PartialEq)]
pub struct SegmentRecord {
    pub filename: String,
    /// Exact time from the start of the whole file.
    pub start_time: Duration,
    /// This segment's exact span.
    pub duration: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SegmentListType {
    #[default]
    Flat,
    Csv,
    M3u8,
    Ext,
    Ffconcat,
}

impl SegmentListType {
    /// Parse `-segment_list_type`'s named values (`flat`, `csv`, `m3u8`,
    /// `hls` as a synonym for `m3u8`, `ext`, `ffconcat`).
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "flat" => Some(Self::Flat),
            "csv" => Some(Self::Csv),
            "m3u8" | "hls" => Some(Self::M3u8),
            "ext" => Some(Self::Ext),
            "ffconcat" => Some(Self::Ffconcat),
            _ => None,
        }
    }
}

/// Render a whole segment list. `finished` is whether the file is complete
/// (the `m3u8` type appends `#EXT-X-ENDLIST` only once it is, matching HLS's
/// own convention for a non-live playlist).
#[must_use]
pub fn render(kind: SegmentListType, records: &[SegmentRecord], finished: bool) -> String {
    match kind {
        SegmentListType::Flat => {
            let mut out = String::new();
            for r in records {
                out.push_str(&r.filename);
                out.push('\n');
            }
            out
        }
        SegmentListType::Csv => {
            let mut out = String::new();
            for r in records {
                let _ = writeln!(
                    out,
                    "{},{},{}",
                    r.filename,
                    format_decimal_seconds(r.start_time, 6),
                    r.start_time.checked_add(r.duration).map_or_else(
                        || format_decimal_seconds(r.start_time, 6),
                        |end| { format_decimal_seconds(end, 6) }
                    )
                );
            }
            out
        }
        SegmentListType::Ext => {
            let mut out = String::new();
            for r in records {
                let _ = writeln!(
                    out,
                    "{},{}",
                    r.filename,
                    format_decimal_seconds(r.duration, 6)
                );
            }
            out
        }
        SegmentListType::M3u8 => render_m3u8(records, finished),
        SegmentListType::Ffconcat => render_ffconcat(records),
    }
}

fn render_m3u8(records: &[SegmentRecord], finished: bool) -> String {
    let target = records
        .iter()
        .map(|r| ceil_seconds(r.duration))
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    out.push_str("#EXTM3U\n");
    out.push_str("#EXT-X-VERSION:3\n");
    let _ = writeln!(out, "#EXT-X-TARGETDURATION:{target}");
    out.push_str("#EXT-X-MEDIA-SEQUENCE:0\n");
    for r in records {
        let _ = writeln!(out, "#EXTINF:{},", format_decimal_seconds(r.duration, 6));
        out.push_str(&r.filename);
        out.push('\n');
    }
    if finished {
        out.push_str("#EXT-X-ENDLIST\n");
    }
    out
}

/// Quote `s` for a concat-script `file` directive: wrapped in `'...'`, with
/// any embedded `'` written as the shell-style `'\''` (close, escaped
/// literal quote, reopen) — the one construct
/// [`crate::concat::script`]'s reader actually decodes back to a literal
/// quote, per its own module docs on how backslash and quoting interact.
fn quote_concat_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

fn render_ffconcat(records: &[SegmentRecord]) -> String {
    let mut out = String::new();
    out.push_str("ffconcat version 1.0\n");
    for r in records {
        out.push_str("file ");
        out.push_str(&quote_concat_path(&r.filename));
        out.push('\n');
        let _ = writeln!(out, "duration {}", format_decimal_seconds(r.duration, 6));
    }
    out
}

fn ceil_seconds(duration: Duration) -> u64 {
    let (numerator, denominator) = duration.as_ratio();
    if numerator <= 0 {
        return 0;
    }
    let whole = numerator.div_euclid(denominator);
    let rounded = whole.saturating_add(i128::from(numerator.rem_euclid(denominator) != 0));
    u64::try_from(rounded).unwrap_or(u64::MAX)
}

/// Render an exact duration as a decimal field without binary floating point.
#[allow(
    clippy::integer_division,
    reason = "decimal rendering intentionally rounds the exact ratio at its text boundary"
)]
fn format_decimal_seconds(duration: Duration, minimum_fractional_digits: usize) -> String {
    const MAX_FRACTIONAL_DIGITS: usize = 15;

    let (numerator, denominator) = duration.as_ratio();
    let negative = numerator < 0;
    let numerator = numerator.unsigned_abs();
    let denominator = denominator as u128;
    let mut whole = numerator / denominator;
    let remainder = numerator % denominator;
    let mut scale = 1_u128;
    let mut fractional_digits = 0;
    while fractional_digits < MAX_FRACTIONAL_DIGITS {
        let Some(next_scale) = scale.checked_mul(10) else {
            break;
        };
        if remainder.checked_mul(next_scale).is_none() {
            break;
        }
        scale = next_scale;
        fractional_digits += 1;
    }
    let scaled = remainder * scale;
    let mut fraction = scaled / denominator;
    let discarded = scaled % denominator;
    if discarded >= denominator - discarded {
        fraction = fraction.saturating_add(1);
    }
    if fraction == scale {
        whole = whole.saturating_add(1);
        fraction = 0;
    }

    let mut digits = if fractional_digits == 0 {
        String::new()
    } else {
        format!("{fraction:0fractional_digits$}")
    };
    while digits.len() > minimum_fractional_digits && digits.ends_with('0') {
        digits.pop();
    }
    while digits.len() < minimum_fractional_digits {
        digits.push('0');
    }
    format!("{}{whole}.{digits}", if negative { "-" } else { "" })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use crate::concat::script;

    fn records() -> Vec<SegmentRecord> {
        vec![
            SegmentRecord {
                filename: "out0.ts".to_owned(),
                start_time: Duration::ZERO,
                duration: Duration::from_micros(2_000_000),
            },
            SegmentRecord {
                filename: "out1.ts".to_owned(),
                start_time: Duration::from_micros(2_000_000),
                duration: Duration::from_micros(1_500_000),
            },
        ]
    }

    #[test]
    fn flat_is_one_filename_per_line() {
        assert_eq!(
            render(SegmentListType::Flat, &records(), true),
            "out0.ts\nout1.ts\n"
        );
    }

    #[test]
    fn m3u8_has_the_expected_hls_shape() {
        let out = render(SegmentListType::M3u8, &records(), true);
        assert!(out.starts_with("#EXTM3U\n"));
        assert!(out.contains("#EXT-X-TARGETDURATION:2\n"));
        assert!(out.contains("#EXTINF:2.000000,\nout0.ts\n"));
        assert!(out.trim_end().ends_with("#EXT-X-ENDLIST"));
    }

    #[test]
    fn m3u8_omits_endlist_while_not_finished() {
        let out = render(SegmentListType::M3u8, &records(), false);
        assert!(!out.contains("ENDLIST"));
    }

    #[test]
    fn m3u8_keeps_decimal_digits_beyond_microseconds() {
        let records = vec![
            SegmentRecord {
                filename: "tiny.ts".to_owned(),
                start_time: Duration::ZERO,
                duration: Duration::from_fraction(7, 1_000_000_000).unwrap(),
            },
            SegmentRecord {
                filename: "clock.ts".to_owned(),
                start_time: Duration::from_fraction(7, 1_000_000_000).unwrap(),
                duration: Duration::from_fraction(1, 28_224_000).unwrap(),
            },
        ];
        let out = render(SegmentListType::M3u8, &records, true);
        assert!(
            out.contains("#EXTINF:0.000000007,\ntiny.ts\n"),
            "submicrosecond duration was rounded away:\n{out}"
        );
        assert!(
            out.contains("#EXTINF:0.000000035430839,\nclock.ts\n"),
            "awkward time-base denominator passed through a binary float:\n{out}"
        );
    }

    #[test]
    fn decimal_list_types_preserve_a_submicrosecond_segment_span() {
        let records = vec![SegmentRecord {
            filename: "tiny.ts".to_owned(),
            start_time: Duration::ZERO,
            duration: Duration::from_fraction(7, 1_000_000_000).unwrap(),
        }];

        assert_eq!(
            render(SegmentListType::Csv, &records, true),
            "tiny.ts,0.000000,0.000000007\n"
        );
        assert_eq!(
            render(SegmentListType::Ext, &records, true),
            "tiny.ts,0.000000007\n"
        );
        let ffconcat = render(SegmentListType::Ffconcat, &records, true);
        let parsed = script::parse(&ffconcat, true).unwrap();
        assert!(parsed.lines.iter().any(|line| {
            line.directive
                == script::Directive::Duration(Duration::from_fraction(7, 1_000_000_000).unwrap())
        }));
    }

    #[test]
    fn ffconcat_list_round_trips_through_the_concat_parser() {
        let out = render(SegmentListType::Ffconcat, &records(), true);
        let parsed = script::parse(&out, true).unwrap();
        let files: Vec<&str> = parsed
            .lines
            .iter()
            .filter_map(|l| match &l.directive {
                script::Directive::File(p) => Some(p.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(files, vec!["out0.ts", "out1.ts"]);
    }

    #[test]
    fn ffconcat_list_round_trips_a_filename_with_an_embedded_quote() {
        let records = vec![SegmentRecord {
            filename: "out's segment.ts".to_owned(),
            start_time: Duration::ZERO,
            duration: Duration::from_micros(2_000_000),
        }];
        let out = render(SegmentListType::Ffconcat, &records, true);
        let parsed = script::parse(&out, true).unwrap();
        let files: Vec<&script::Directive> = parsed
            .lines
            .iter()
            .map(|l| &l.directive)
            .filter(|d| matches!(d, script::Directive::File(_)))
            .collect();
        assert_eq!(
            files,
            vec![&script::Directive::File("out's segment.ts".to_owned())]
        );
    }

    #[test]
    fn list_type_names_parse() {
        assert_eq!(SegmentListType::parse("flat"), Some(SegmentListType::Flat));
        assert_eq!(SegmentListType::parse("hls"), Some(SegmentListType::M3u8));
        assert_eq!(SegmentListType::parse("m3u8"), Some(SegmentListType::M3u8));
        assert_eq!(SegmentListType::parse("nonsense"), None);
    }
}
