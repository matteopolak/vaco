//! `VPlayer` (`.txt`) — demux only.
//!
//! `HH:MM:SS:text` per line — whole seconds, start-only (see
//! [`vaco_format_subtitle::time::parse_vplayer_time`]). The text may itself
//! contain colons, so a line is split on at most the first three, not on
//! every colon. A cue's end is its successor's start; the last cue gets zero
//! duration (see [`crate::lrc`]).
//!
//! # A genuine ambiguity with `.stl`, disambiguated by content
//!
//! `HH:MM:SS:text` and [`crate::stl`]'s `HH:MM:SS:hh,HH:MM:SS:hh,text` share
//! a prefix: naively, `parse_line` on an STL line reads its first timecode as
//! a valid `HH:MM:SS` and the rest — `hh,HH:MM:SS:hh,text` — as this format's
//! "text". Caught by `tests/probe_matrix.rs`, which checks every format's
//! probe against every other format's sample and found this one tying STL's
//! own probe on an STL file. [`looks_like_stl_line`] rejects a line whose
//! "text" opens with that exact `hh,HH:MM:SS:hh,` shape — STL's hundredths
//! field, a comma, its second timecode, and a comma — which is not a pattern
//! real `VPlayer` prose produces.

use vaco_codec_core::CodecId;
use vaco_core::{Duration, Result};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{Demuxer, DemuxerDesc, ParserProvider};
use vaco_format_subtitle::Cue;
use vaco_format_subtitle::time::parse_vplayer_time;
use vaco_io::MediaSource;

use crate::engine::{self, DEMUX_FLAGS};

/// Whether `text` opens with an `hh,HH:MM:SS:hh,` shaped prefix — the
/// remainder of an STL line after its first three colon-separated fields are
/// (mis)read as this format's `HH:MM:SS`, not `VPlayer` prose.
fn looks_like_stl_line(text: &str) -> bool {
    let b = text.as_bytes();
    let digit = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
    let at = |i: usize, c: u8| b.get(i) == Some(&c);
    digit(0)
        && digit(1)
        && at(2, b',')
        && digit(3)
        && digit(4)
        && at(5, b':')
        && digit(6)
        && digit(7)
        && at(8, b':')
        && digit(9)
        && digit(10)
        && at(11, b':')
        && digit(12)
        && digit(13)
        && at(14, b',')
}

fn parse_line(line: &str) -> Option<(Duration, &str)> {
    let parts: Vec<&str> = line.splitn(4, ':').collect();
    match parts.as_slice() {
        [h, m, s, text] if !looks_like_stl_line(text) => {
            Some((parse_vplayer_time(&format!("{h}:{m}:{s}"))?, text))
        }
        _ => None,
    }
}

fn parse(bytes: &[u8]) -> Vec<Cue> {
    let text = String::from_utf8_lossy(bytes);
    let entries: Vec<(Duration, &str)> = text.lines().filter_map(parse_line).collect();
    let mut cues = Vec::new();
    for (i, &(start, body)) in entries.iter().enumerate() {
        let end = entries
            .get(i.saturating_add(1))
            .map_or(start, |&(next, _)| next);
        cues.push(Cue::new(start, end, body.as_bytes().to_vec()));
    }
    cues
}

/// Content probe: lines matching `HH:MM:SS:text`.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let text = String::from_utf8_lossy(data.buf);
    let hits = text.lines().filter(|l| parse_line(l).is_some()).count();
    if hits > 0 {
        ProbeScore::repeating(hits as u32)
    } else {
        ProbeScore::from_extension(data, &["txt"])
    }
}

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    engine::open_generic(src, Some(CodecId::Vplayer), parse)
}

/// The demuxer descriptor. `CodecId::Vplayer`, matching the reference's own
/// `codec_name=vplayer`.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "vplayer",
    long_name: "VPlayer subtitles",
    extensions: &["txt"],
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
    fn derives_end_from_next_start() {
        let cues = parse(b"00:00:05:Hello world\n00:00:08:Second\n");
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start, Duration::from_micros(5_000_000));
        assert_eq!(cues[0].end, Duration::from_micros(8_000_000));
        assert_eq!(cues[0].text, b"Hello world");
    }

    #[test]
    fn text_containing_a_colon_is_preserved() {
        let cues = parse(b"00:00:01:Note: important\n");
        assert_eq!(cues[0].text, b"Note: important");
    }

    #[test]
    fn probe_rejects_plain_prose() {
        assert_eq!(
            probe(&ProbeData::new(b"This is not timed at all, just text.\n")),
            ProbeScore::NONE
        );
    }

    #[test]
    fn does_not_claim_an_stl_line() {
        // Regression for the collision `tests/probe_matrix.rs` found: an STL
        // line's own timecode-shaped "text" must not parse as VPlayer.
        let stl_line = "00:00:01:12,00:00:03:00,Hello world";
        assert!(parse_line(stl_line).is_none());
        assert_eq!(
            probe(&ProbeData::new(stl_line.as_bytes())),
            ProbeScore::NONE
        );
    }
}
