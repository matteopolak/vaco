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

/// One completed segment, everything a list line needs.
#[derive(Debug, Clone, PartialEq)]
pub struct SegmentRecord {
    pub filename: String,
    /// Seconds from the start of the whole file.
    pub start_time: f64,
    /// Seconds, this segment's own span.
    pub duration: f64,
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
                    "{},{:.6},{:.6}",
                    r.filename,
                    r.start_time,
                    r.start_time + r.duration
                );
            }
            out
        }
        SegmentListType::Ext => {
            let mut out = String::new();
            for r in records {
                let _ = writeln!(out, "{},{:.6}", r.filename, r.duration);
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
        .map(|r| r.duration.ceil() as u64)
        .max()
        .unwrap_or(0);
    let mut out = String::new();
    out.push_str("#EXTM3U\n");
    out.push_str("#EXT-X-VERSION:3\n");
    let _ = writeln!(out, "#EXT-X-TARGETDURATION:{target}");
    out.push_str("#EXT-X-MEDIA-SEQUENCE:0\n");
    for r in records {
        let _ = writeln!(out, "#EXTINF:{:.6},", r.duration);
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
        let _ = writeln!(out, "duration {:.6}", r.duration);
    }
    out
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
                start_time: 0.0,
                duration: 2.0,
            },
            SegmentRecord {
                filename: "out1.ts".to_owned(),
                start_time: 2.0,
                duration: 1.5,
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
            start_time: 0.0,
            duration: 2.0,
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
