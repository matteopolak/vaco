//! `JACOsub` (`.jss`).
//!
//! De-facto format, no formal standard — measured against the reference
//! (`ffmpeg -f jacosub`, D17): one cue per line, `H:MM:SS.hh H:MM:SS.hh
//! text`, centisecond timestamps in the same punctuation shape as ASS's
//! clock (see [`vaco_format_subtitle::time::parse_jacosub_time`]) but a
//! different format. Directive and comment lines (anything that is not two
//! whitespace-separated timestamps followed by text) are skipped rather than
//! rejected — `JACOsub`'s real grammar has several of those and this crate does
//! not need to interpret them to recover cues.

use vaco_codec_core::CodecId;
use vaco_core::Result;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{Demuxer, DemuxerDesc, Muxer, MuxerDesc, ParserProvider};
use vaco_format_subtitle::Cue;
use vaco_format_subtitle::time::{format_jacosub_time, parse_jacosub_time};
use vaco_io::{IoWriter, MediaSink, MediaSource};

use crate::engine::{self, CueMux, DEMUX_FLAGS, GenericTextMuxer, MUX_FLAGS};

fn parse_line(line: &str) -> Option<(vaco_core::Duration, vaco_core::Duration, &str)> {
    let rest = line.trim_start();
    let (t1, rest) = rest.split_once(char::is_whitespace)?;
    let rest = rest.trim_start();
    let (t2, rest) = rest.split_once(char::is_whitespace)?;
    let start = parse_jacosub_time(t1)?;
    let end = parse_jacosub_time(t2)?;
    Some((start, end, rest.trim_start()))
}

fn parse(bytes: &[u8]) -> Vec<Cue> {
    let text = String::from_utf8_lossy(bytes);
    text.lines()
        .filter_map(parse_line)
        .map(|(start, end, t)| Cue::new(start, end, t.as_bytes().to_vec()))
        .collect()
}

/// Content probe: lines matching the two-timestamp shape.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let text = String::from_utf8_lossy(data.buf);
    let hits = text.lines().filter(|l| parse_line(l).is_some()).count();
    if hits > 0 {
        ProbeScore::repeating(hits as u32)
    } else {
        ProbeScore::from_extension(data, &["jss"])
    }
}

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    engine::open_generic(src, Some(CodecId::Jacosub), parse)
}

/// The demuxer descriptor. `CodecId::Jacosub`, matching the reference's own
/// `codec_name=jacosub`.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "jacosub",
    long_name: "JACOsub subtitle format",
    extensions: &["jss", "js"],
    mime_types: &["text/x-jacosub"],
    flags: DEMUX_FLAGS,
    probe,
    open: open_demuxer,
};

#[derive(Debug, Default)]
struct JacosubMux;

impl CueMux for JacosubMux {
    fn accepts(&self, codec_id: Option<CodecId>) -> bool {
        matches!(codec_id, Some(CodecId::Jacosub))
    }

    fn write_cue(&mut self, out: &mut IoWriter, _index: usize, cue: &Cue) -> Result<()> {
        out.write(
            format!(
                "{} {} ",
                format_jacosub_time(cue.start),
                format_jacosub_time(cue.end)
            )
            .as_bytes(),
        )?;
        out.write(&cue.text)?;
        out.write(b"\n")
    }
}

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(GenericTextMuxer::new(
        sink, JacosubMux, MUX_FLAGS,
    )?))
}

/// The muxer descriptor.
pub const MUXER: MuxerDesc = MuxerDesc {
    name: "jacosub",
    long_name: "JACOsub subtitle format",
    extensions: &["jss"],
    default_video: None,
    default_audio: None,
    open: open_muxer,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_core::Duration;

    #[test]
    fn parses_a_cue_line() {
        let cues = parse(b"0:00:01.00 0:00:03.00 Hello world\n");
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start, Duration::from_micros(1_000_000));
        assert_eq!(cues[0].end, Duration::from_micros(3_000_000));
        assert_eq!(cues[0].text, b"Hello world");
    }

    #[test]
    fn a_directive_line_without_two_timestamps_is_skipped() {
        let cues = parse(b"#TIMERES 30\n0:00:01.00 0:00:02.00 Hi\n");
        assert_eq!(cues.len(), 1);
    }

    #[test]
    fn probe_rejects_plain_prose() {
        assert_eq!(
            probe(&ProbeData::new(b"Not a subtitle file, just prose.\n")),
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
        let mut mux = GenericTextMuxer::new(Box::new(sink), JacosubMux, MUX_FLAGS).unwrap();
        mux.add_stream(&CodecParameters::new(MediaType::Subtitle).with_codec(CodecId::Jacosub))
            .unwrap();
        mux.write_header().unwrap();
        let mut budget = Budget::new(Limits::permissive());
        let mut pkt = Packet::from_slice(&mut budget, b"Hello world").unwrap();
        pkt.pts = vaco_core::Timestamp::new(1_000_000);
        pkt.duration = Duration::from_micros(2_000_000);
        mux.write_packet(&pkt).unwrap();
        mux.write_trailer().unwrap();
        let cues = parse(&shared.snapshot());
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, b"Hello world");
    }
}
