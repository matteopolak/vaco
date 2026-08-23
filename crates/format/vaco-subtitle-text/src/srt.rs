//! `SubRip` (`.srt`).
//!
//! Blank-line-separated blocks: an optional counter line, a `HH:MM:SS,mmm -->
//! HH:MM:SS,mmm` timing line — comma-punctuated, see
//! [`vaco_format_subtitle::time::parse_srt_time`] — then one or more lines of
//! text. The counter is not required to find the block (this parser locates
//! the timing line by content, not by position), and is not reproduced on
//! output — the reference renumbers on write.

use vaco_codec_core::CodecId;
use vaco_core::Result;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{Demuxer, DemuxerDesc, Muxer, MuxerDesc, ParserProvider};
use vaco_format_subtitle::Cue;
use vaco_format_subtitle::text::{blocks, join_lines};
use vaco_format_subtitle::time::{format_srt_time, parse_srt_timing_line};
use vaco_io::{IoWriter, MediaSink, MediaSource};

use crate::engine::{self, CueMux, DEMUX_FLAGS, GenericTextMuxer, MUX_FLAGS};

/// Content probe: count lines that parse as an SRT timing line (comma
/// fraction). A `WebVTT` or `SubViewer` sample uses a period there and scores
/// zero hits here — see `tests/probe_matrix.rs` for the cross-format check.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let text = String::from_utf8_lossy(data.buf);
    let hits = text
        .lines()
        .take(256)
        .filter(|l| parse_srt_timing_line(l.trim()).is_some())
        .count();
    if hits > 0 {
        ProbeScore::repeating(hits as u32)
    } else {
        ProbeScore::from_extension(data, &["srt"])
    }
}

fn parse(bytes: &[u8]) -> Vec<Cue> {
    let mut cues = Vec::new();
    for block in blocks(bytes) {
        let mut found = None;
        for (i, line) in block.iter().enumerate() {
            let Ok(s) = std::str::from_utf8(line) else {
                continue;
            };
            if let Some(timing) = parse_srt_timing_line(s.trim()) {
                found = Some((i, timing));
                break;
            }
        }
        let Some((idx, (start, end))) = found else {
            continue;
        };
        let text_lines = block.get(idx.saturating_add(1)..).unwrap_or(&[]);
        cues.push(Cue::new(start, end, join_lines(text_lines)));
    }
    cues
}

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    engine::open_generic(src, Some(CodecId::SubRip), parse)
}

/// The demuxer descriptor.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "srt",
    long_name: "SubRip subtitle",
    extensions: &["srt"],
    mime_types: &["application/x-subrip"],
    flags: DEMUX_FLAGS,
    probe,
    open: open_demuxer,
};

#[derive(Debug, Default)]
struct SrtMux;

impl CueMux for SrtMux {
    fn accepts(&self, codec_id: Option<CodecId>) -> bool {
        matches!(codec_id, Some(CodecId::SubRip))
    }

    fn write_cue(&mut self, out: &mut IoWriter, index: usize, cue: &Cue) -> Result<()> {
        out.write(format!("{index}\n").as_bytes())?;
        out.write(
            format!(
                "{} --> {}\n",
                format_srt_time(cue.start),
                format_srt_time(cue.end)
            )
            .as_bytes(),
        )?;
        out.write(&cue.text)?;
        out.write(b"\n\n")
    }
}

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(GenericTextMuxer::new(sink, SrtMux, MUX_FLAGS)?))
}

/// The muxer descriptor.
pub const MUXER: MuxerDesc = MuxerDesc {
    name: "srt",
    long_name: "SubRip subtitle",
    extensions: &["srt"],
    default_video: None,
    default_audio: None,
    open: open_muxer,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_core::Duration;

    const SAMPLE: &[u8] = b"1\n00:00:01,000 --> 00:00:02,500\nHello\nworld\n\n2\n00:00:03,000 --> 00:00:04,000\nSecond\n";

    #[test]
    fn parses_two_cues_with_multiline_text() {
        let cues = parse(SAMPLE);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start, Duration::from_micros(1_000_000));
        assert_eq!(cues[0].end, Duration::from_micros(2_500_000));
        assert_eq!(cues[0].text, b"Hello\nworld");
        assert_eq!(cues[1].text, b"Second");
    }

    #[test]
    fn missing_counter_line_still_parses() {
        let cues = parse(b"00:00:01,000 --> 00:00:02,000\nNo counter\n");
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, b"No counter");
    }

    #[test]
    fn probe_rejects_plain_prose() {
        let data = ProbeData::new(
            b"The quick brown fox jumps over the lazy dog.\nAnother line of prose here.\n",
        );
        assert_eq!(probe(&data), ProbeScore::NONE);
    }

    #[test]
    fn probe_rejects_a_webvtt_timing_line() {
        let data = ProbeData::new(b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHello\n");
        assert_eq!(probe(&data), ProbeScore::NONE);
    }

    #[test]
    fn probe_accepts_real_srt() {
        let data = ProbeData::new(SAMPLE);
        assert!(probe(&data).value() > ProbeScore::RETRY.value());
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
        let mut mux = GenericTextMuxer::new(Box::new(sink), SrtMux, MUX_FLAGS).unwrap();
        mux.add_stream(&CodecParameters::new(MediaType::Subtitle).with_codec(CodecId::SubRip))
            .unwrap();
        mux.write_header().unwrap();
        for cue in parse(SAMPLE) {
            let mut budget = Budget::new(Limits::permissive());
            let mut pkt = Packet::from_slice(&mut budget, &cue.text).unwrap();
            pkt.pts = vaco_core::Timestamp::new(cue.start.as_micros());
            pkt.duration = cue.duration();
            mux.write_packet(&pkt).unwrap();
        }
        mux.write_trailer().unwrap();
        let written = shared.snapshot();
        let round_tripped = parse(&written);
        assert_eq!(round_tripped, parse(SAMPLE));
    }
}
