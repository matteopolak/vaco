//! LRC lyrics (`.lrc`).
//!
//! `[mm:ss.xx]text` per line — hundredths, start-only (see
//! [`vaco_format_subtitle::time::parse_lrc_time`]). A cue's end is its
//! successor's start; the last cue in the file gets zero duration rather than
//! the reference's own last-cue sentinel (measured as `-1` tick in whatever
//! internal time base the reference happens to use for this format, which is
//! not a value worth reproducing exactly — see
//! `docs/format/vaco-subtitle-text.md`). A line may open with more than one
//! bracketed timestamp (the common "same lyric line sung at two points"
//! convention); each becomes its own cue sharing that line's text, in the
//! order they appear. Non-timestamp bracketed tags (`[ar:...]`, `[ti:...]`)
//! do not parse as a time and are silently skipped, which is what makes them
//! harmless metadata rather than malformed cues.
//!
//! Measured: the reference reports this demuxer's codec as the generic
//! `text`, not `lrc` — `vaco-codec-core` has no `Text` variant yet, so this
//! module's stream and muxer both use the generic `CodecId::Text`.

use vaco_codec_core::CodecId;
use vaco_core::{Duration, Result};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{Demuxer, DemuxerDesc, Muxer, MuxerDesc, ParserProvider};
use vaco_format_subtitle::Cue;
use vaco_format_subtitle::time::{format_lrc_time, parse_lrc_time};
use vaco_io::{IoWriter, MediaSink, MediaSource};

use crate::engine::{self, CueMux, DEMUX_FLAGS, GenericTextMuxer, MUX_FLAGS};

/// Strip leading `[mm:ss.xx]` tags off `line`, returning each parsed time and
/// whatever text follows the last one.
fn leading_timestamps(line: &str) -> (Vec<Duration>, &str) {
    let mut rest = line;
    let mut times = Vec::new();
    while let Some(stripped) = rest.strip_prefix('[') {
        let Some((tag, after)) = stripped.split_once(']') else {
            break;
        };
        let Some(d) = parse_lrc_time(tag) else { break };
        times.push(d);
        rest = after;
    }
    (times, rest)
}

fn parse(bytes: &[u8]) -> Vec<Cue> {
    let text = String::from_utf8_lossy(bytes);
    let mut entries: Vec<(Duration, &str)> = Vec::new();
    for line in text.lines() {
        let (times, rest) = leading_timestamps(line);
        for t in times {
            entries.push((t, rest));
        }
    }
    let mut cues = Vec::new();
    for (i, &(start, body)) in entries.iter().enumerate() {
        let end = entries
            .get(i.saturating_add(1))
            .map_or(start, |&(next, _)| next);
        cues.push(Cue::new(start, end, body.as_bytes().to_vec()));
    }
    cues
}

/// Content probe: lines with a leading `[mm:ss.xx]` tag.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let text = String::from_utf8_lossy(data.buf);
    let hits = text
        .lines()
        .filter(|l| !leading_timestamps(l).0.is_empty())
        .count();
    if hits > 0 {
        ProbeScore::repeating(hits as u32)
    } else {
        ProbeScore::from_extension(data, &["lrc"])
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
    name: "lrc",
    long_name: "LRC lyrics",
    extensions: &["lrc"],
    mime_types: &[],
    flags: DEMUX_FLAGS,
    probe,
    open: open_demuxer,
};

#[derive(Debug, Default)]
struct LrcMux;

impl CueMux for LrcMux {
    fn accepts(&self, codec_id: Option<CodecId>) -> bool {
        matches!(codec_id, Some(CodecId::Text))
    }

    fn write_cue(&mut self, out: &mut IoWriter, _index: usize, cue: &Cue) -> Result<()> {
        out.write(format!("[{}]", format_lrc_time(cue.start)).as_bytes())?;
        out.write(&cue.text)?;
        out.write(b"\n")
    }
}

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(GenericTextMuxer::new(sink, LrcMux, MUX_FLAGS)?))
}

/// The muxer descriptor.
pub const MUXER: MuxerDesc = MuxerDesc {
    name: "lrc",
    long_name: "LRC lyrics",
    extensions: &["lrc"],
    default_video: None,
    default_audio: None,
    open: open_muxer,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn derives_end_from_the_next_cue_and_zero_for_the_last() {
        let cues = parse(b"[00:01.00]Hello world\n[00:03.00]Second\n");
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].end, Duration::from_micros(3_000_000));
        assert_eq!(cues[1].end, cues[1].start);
    }

    #[test]
    fn metadata_tags_are_skipped() {
        let cues = parse(b"[ar:Some Artist]\n[ti:Some Title]\n[00:01.00]Hi\n");
        assert_eq!(cues.len(), 1);
    }

    #[test]
    fn a_line_with_two_timestamps_yields_two_cues() {
        let cues = parse(b"[00:01.00][00:10.00]Chorus\n");
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, b"Chorus");
        assert_eq!(cues[1].text, b"Chorus");
    }

    #[test]
    fn probe_rejects_plain_prose() {
        assert_eq!(
            probe(&ProbeData::new(b"No brackets in this text at all.\n")),
            ProbeScore::NONE
        );
    }

    #[test]
    fn round_trip_through_mux_and_demux() {
        use vaco_codec_core::CodecParameters;
        use vaco_core::MediaType;
        use vaco_format_core::vacoraw::MemorySink;
        use vaco_limits::{Budget, Limits};
        use vaco_packet::Packet;

        let sink = MemorySink::new();
        let shared = sink.shared();
        let mut mux = GenericTextMuxer::new(Box::new(sink), LrcMux, MUX_FLAGS).unwrap();
        mux.add_stream(&CodecParameters::new(MediaType::Subtitle).with_codec(CodecId::Text))
            .unwrap();
        mux.write_header().unwrap();
        let mut budget = Budget::new(Limits::permissive());
        let mut pkt = Packet::from_slice(&mut budget, b"Hi").unwrap();
        pkt.pts = vaco_core::Timestamp::new(1_000_000);
        mux.write_packet(&pkt).unwrap();
        mux.write_trailer().unwrap();
        let cues = parse(&shared.snapshot());
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, b"Hi");
    }
}
