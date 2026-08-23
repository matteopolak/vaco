//! `MPlayer` subtitles (`.sub`, `MPsub`) — demux only, `FORMAT=TIME` variant.
//!
//! Measured (D17), and the least obvious grammar in this crate: each
//! blank-line-separated block starts with a `gap duration` line in seconds
//! ([`vaco_format_subtitle::time::parse_seconds`]), and **both fields are
//! relative to the previous cue's end**, not to the start of the file. Two
//! consecutive blocks reading the identical `"1.0 2.0"` measure as `[1.0,
//! 3.0]` and `[4.0, 6.0]` — the second block's start (4.0) is only reachable
//! by adding its gap (1.0) to the *first* block's end (3.0), not to 0. A
//! parser that treated these as absolute timestamps would silently compress
//! every cue after the first toward the start of the file.
//!
//! `MPsub` also has a `FORMAT=FRAME` variant (frame-relative rather than
//! seconds-relative) that this parser does not implement — a file opening
//! with anything other than `FORMAT=TIME` produces no cues rather than wrong
//! ones.

use vaco_codec_core::CodecId;
use vaco_core::{Duration, Result};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{Demuxer, DemuxerDesc, ParserProvider};
use vaco_format_subtitle::Cue;
use vaco_format_subtitle::time::parse_seconds;
use vaco_io::MediaSource;

use crate::engine::{self, DEMUX_FLAGS};

/// Drop a leading `FORMAT=TIME` line, if present; `None` if the file opens
/// with a different (unsupported) `FORMAT=` declaration.
fn body_after_time_header(text: &str) -> Option<&str> {
    let first = text.lines().next().unwrap_or("").trim();
    if !first.to_ascii_uppercase().starts_with("FORMAT=") {
        // No header at all is tolerated (some samples omit it); treat the
        // whole file as the body.
        return Some(text);
    }
    if !first.eq_ignore_ascii_case("FORMAT=TIME") {
        return None;
    }
    match text.find('\n') {
        Some(pos) => Some(text.get(pos.saturating_add(1)..).unwrap_or("")),
        None => Some(""),
    }
}

fn parse(bytes: &[u8]) -> Vec<Cue> {
    let text = String::from_utf8_lossy(bytes);
    let Some(body) = body_after_time_header(&text) else {
        return Vec::new();
    };
    let mut cues = Vec::new();
    let mut prev_end = Duration::ZERO;
    for block in body.split("\n\n") {
        let mut lines = block.lines();
        let Some(timing) = lines.next() else { continue };
        let mut fields = timing.split_whitespace();
        let (Some(gap_s), Some(dur_s)) = (fields.next(), fields.next()) else {
            continue;
        };
        let (Some(gap), Some(dur)) = (parse_seconds(gap_s), parse_seconds(dur_s)) else {
            continue;
        };
        let start = Duration::from_micros(prev_end.as_micros().saturating_add(gap.as_micros()));
        let end = Duration::from_micros(start.as_micros().saturating_add(dur.as_micros()));
        let body_lines: Vec<&str> = lines.collect();
        cues.push(Cue::new(start, end, body_lines.join("\n").into_bytes()));
        prev_end = end;
    }
    cues
}

/// Content probe: the `FORMAT=TIME` header, or — absent it — at least one
/// block whose first line is two whitespace-separated
/// [`vaco_format_subtitle::time::parse_seconds`] fields.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let text = String::from_utf8_lossy(data.buf);
    if text
        .lines()
        .next()
        .is_some_and(|l| l.trim().eq_ignore_ascii_case("FORMAT=TIME"))
    {
        return ProbeScore::MAGIC_CHECKED;
    }
    let hits = text
        .split("\n\n")
        .filter(|b| {
            let mut f = b.lines().next().unwrap_or("").split_whitespace();
            matches!(
                (f.next().map(parse_seconds), f.next().map(parse_seconds)),
                (Some(Some(_)), Some(Some(_)))
            )
        })
        .count();
    if hits > 0 {
        ProbeScore::weak(u8::try_from(hits.min(20)).unwrap_or(20))
    } else {
        ProbeScore::from_extension(data, &["sub"])
    }
}

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    engine::open_generic(src, Some(CodecId::Text), parse)
}

/// The demuxer descriptor. `CodecId::Text` — the generic codec, matching the
/// reference's own measured `codec_name=text` for this format.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "mpsub",
    long_name: "MPlayer subtitles",
    extensions: &["sub"],
    mime_types: &[],
    flags: DEMUX_FLAGS,
    probe,
    open: open_demuxer,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn gaps_are_relative_to_the_previous_cues_end() {
        let cues = parse(b"FORMAT=TIME\n1.0 2.0\nHello world\n\n1.0 2.0\nSecond\n");
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start, Duration::from_micros(1_000_000));
        assert_eq!(cues[0].end, Duration::from_micros(3_000_000));
        assert_eq!(cues[1].start, Duration::from_micros(4_000_000));
        assert_eq!(cues[1].end, Duration::from_micros(6_000_000));
    }

    #[test]
    fn an_unsupported_format_header_yields_no_cues() {
        assert!(parse(b"FORMAT=FRAME\n1 2\nHello\n").is_empty());
    }

    #[test]
    fn probe_rejects_plain_prose() {
        assert_eq!(
            probe(&ProbeData::new(b"Just a sentence.\n\nAnother one.\n")),
            ProbeScore::NONE
        );
    }
}
