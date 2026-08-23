//! `SubViewer` 1.0 (`.sub`) — demux only.
//!
//! `[HH:MM:SS]` on its own line (whole seconds, start-only — see
//! [`vaco_format_subtitle::time::parse_subviewer1_time`]), then one or more
//! lines of text, then the next `[HH:MM:SS]`. A cue's end is its successor's
//! start; the last cue gets zero duration (see [`crate::lrc`] for why this
//! crate does not chase the reference's own last-cue sentinel value).
//!
//! Measured (D17) oddity, reproduced rather than "cleaned up": the reference
//! appends a single trailing `\0` byte to a cue's text that this parser does
//! not have an explanation for beyond the measurement itself — checked on one
//! sample (`ffprobe -f subviewer1 -show_packets -show_data`), not a wide
//! survey, so treat it as measured-but-unconfirmed-general rather than
//! specified.

use vaco_codec_core::CodecId;
use vaco_core::Duration;
use vaco_core::Result;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{Demuxer, DemuxerDesc, ParserProvider};
use vaco_format_subtitle::Cue;
use vaco_format_subtitle::time::parse_subviewer1_time;
use vaco_io::MediaSource;

use crate::engine::{self, DEMUX_FLAGS};

fn bracketed_time(line: &str) -> Option<Duration> {
    let inner = line.trim().strip_prefix('[')?.strip_suffix(']')?;
    parse_subviewer1_time(inner)
}

fn parse(bytes: &[u8]) -> Vec<Cue> {
    let text = String::from_utf8_lossy(bytes);
    let mut entries: Vec<(Duration, Vec<&str>)> = Vec::new();
    for line in text.lines() {
        if let Some(t) = bracketed_time(line) {
            entries.push((t, Vec::new()));
            continue;
        }
        if let Some(last) = entries.last_mut() {
            last.1.push(line);
        }
    }
    let mut cues = Vec::new();
    for (i, (start, lines)) in entries.iter().enumerate() {
        let end = entries
            .get(i.saturating_add(1))
            .map_or(*start, |(next, _)| *next);
        let mut body = lines.join("\n").into_bytes();
        body.push(0);
        cues.push(Cue::new(*start, end, body));
    }
    cues
}

/// Content probe: lines matching `[HH:MM:SS]`.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let text = String::from_utf8_lossy(data.buf);
    let hits = text.lines().filter(|l| bracketed_time(l).is_some()).count();
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
    engine::open_generic(src, Some(CodecId::Subviewer1), parse)
}

/// The demuxer descriptor. `CodecId::Subviewer1`, matching the reference's own
/// `codec_name=subviewer1`.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "subviewer1",
    long_name: "SubViewer v1 subtitle",
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
    fn derives_end_from_next_start_and_appends_the_measured_nul() {
        let cues = parse(b"[00:00:01]\nHello world\n[00:00:03]\n");
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start, Duration::from_micros(1_000_000));
        assert_eq!(cues[0].end, Duration::from_micros(3_000_000));
        assert_eq!(cues[0].text, b"Hello world\0");
    }

    #[test]
    fn probe_rejects_plain_prose() {
        assert_eq!(
            probe(&ProbeData::new(b"No brackets here, just words.\n")),
            ProbeScore::NONE
        );
    }
}
