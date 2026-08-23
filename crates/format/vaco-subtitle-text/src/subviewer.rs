//! `SubViewer` (`.sub`) — demux only, the reference ships no encoder for it.
//!
//! Blank-line-separated blocks: a `HH:MM:SS.mmm,HH:MM:SS.mmm` timing line
//! (milliseconds, period — see
//! [`vaco_format_subtitle::time::parse_subviewer_time`], and note this is a
//! different fraction unit from ASS/JACOsub's period-punctuated centiseconds
//! even though both use a period), then one or more lines of text. A header
//! block (`[INFORMATION]`, `[TITLE]`, ...) does not contain a comma-joined
//! timing line and is skipped the same way a malformed block would be.

use vaco_codec_core::CodecId;
use vaco_core::Result;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{Demuxer, DemuxerDesc, ParserProvider};
use vaco_format_subtitle::Cue;
use vaco_format_subtitle::text::{blocks, join_lines};
use vaco_format_subtitle::time::parse_subviewer_timing_line;
use vaco_io::MediaSource;

use crate::engine::{self, DEMUX_FLAGS};

fn parse(bytes: &[u8]) -> Vec<Cue> {
    let mut cues = Vec::new();
    for block in blocks(bytes) {
        let Some(first) = block.first() else { continue };
        let Ok(s) = std::str::from_utf8(first) else {
            continue;
        };
        let Some((start, end)) = parse_subviewer_timing_line(s.trim()) else {
            continue;
        };
        let text_lines = block.get(1..).unwrap_or(&[]);
        cues.push(Cue::new(start, end, join_lines(text_lines)));
    }
    cues
}

/// Content probe: blocks whose first line is a `parse_subviewer_timing_line`
/// match.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let text = String::from_utf8_lossy(data.buf);
    let hits = text
        .split("\n\n")
        .filter_map(|b| b.lines().next())
        .filter(|l| parse_subviewer_timing_line(l.trim()).is_some())
        .count();
    if hits > 0 {
        ProbeScore::repeating(hits as u32)
    } else {
        ProbeScore::from_extension(data, &["sub"])
    }
}

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    engine::open_generic(src, Some(CodecId::Subviewer), parse)
}

/// The demuxer descriptor. `CodecId::Subviewer`, matching the reference's own
/// `codec_name=subviewer`.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "subviewer",
    long_name: "SubViewer subtitle",
    extensions: &["sub"],
    mime_types: &["text/x-subviewer"],
    flags: DEMUX_FLAGS,
    probe,
    open: open_demuxer,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_core::Duration;

    const SAMPLE: &[u8] = b"[INFORMATION]\n[TITLE]Demo\n\n00:00:01.250,00:00:03.000\nHello world\n\n00:00:04.000,00:00:05.000\nSecond line\n";

    #[test]
    fn parses_cues_and_skips_the_header_block() {
        let cues = parse(SAMPLE);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start, Duration::from_micros(1_250_000));
        assert_eq!(cues[0].text, b"Hello world");
    }

    #[test]
    fn probe_rejects_plain_prose() {
        assert_eq!(
            probe(&ProbeData::new(
                b"Some prose without any timing at all.\n\nMore prose.\n"
            )),
            ProbeScore::NONE
        );
    }

    #[test]
    fn probe_rejects_ass_centisecond_punctuation() {
        // ASS's `H:MM:SS.cc` clock is a different grammar (single-digit hour,
        // centiseconds) from SubViewer's `HH:MM:SS.mmm,HH:MM:SS.mmm`.
        let data =
            ProbeData::new(b"[Events]\nDialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,Hi\n");
        assert_eq!(probe(&data), ProbeScore::NONE);
    }
}
