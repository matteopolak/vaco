//! `WebVTT` (`.vtt`), per the W3C Web Video Text Tracks Format specification.
//!
//! Structurally close to `SubRip` — blank-line-separated blocks, a `-->` timing
//! line — but the fraction is period-punctuated
//! ([`vaco_format_subtitle::time::parse_vtt_time`]), the hour field is
//! optional, and a timing line may carry trailing cue settings
//! (`align:middle`, `position:50%`, ...) this crate passes through as part of
//! the timing line but does not interpret. Every file must open with a
//! `WEBVTT` signature line.

use vaco_codec_core::CodecId;
use vaco_core::Result;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{Demuxer, DemuxerDesc, Muxer, MuxerDesc, ParserProvider};
use vaco_format_subtitle::Cue;
use vaco_format_subtitle::text::{blocks, join_lines};
use vaco_format_subtitle::time::{format_vtt_time, parse_vtt_timing_line};
use vaco_io::{IoWriter, MediaSink, MediaSource};

use crate::engine::{self, CueMux, DEMUX_FLAGS, GenericTextMuxer, MUX_FLAGS};

/// Content probe: the `WEBVTT` signature, required at the very start of the
/// file (an optional BOM has already been stripped by the time any probe
/// runs — see [`vaco_format_subtitle::encoding`] — but this probe still
/// tolerates one directly, since probing happens on raw bytes before that
/// step).
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let buf = data
        .buf
        .strip_prefix(&[0xEF, 0xBB, 0xBF])
        .unwrap_or(data.buf);
    let starts = buf.starts_with(b"WEBVTT")
        && buf
            .get(6)
            .is_none_or(|&b| b == b'\n' || b == b'\r' || b == b' ' || b == b'\t');
    if starts {
        ProbeScore::MAGIC_CHECKED
    } else {
        ProbeScore::from_extension(data, &["vtt"])
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
            if let Some(timing) = parse_vtt_timing_line(s) {
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
    engine::open_generic(src, Some(CodecId::Webvtt), parse)
}

/// The demuxer descriptor.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "webvtt",
    long_name: "WebVTT subtitle",
    extensions: &["vtt"],
    mime_types: &["text/vtt"],
    flags: DEMUX_FLAGS,
    probe,
    open: open_demuxer,
};

#[derive(Debug, Default)]
struct WebvttMux;

impl CueMux for WebvttMux {
    fn accepts(&self, codec_id: Option<CodecId>) -> bool {
        matches!(codec_id, Some(CodecId::Webvtt))
    }

    fn write_header(&mut self, out: &mut IoWriter) -> Result<()> {
        out.write(b"WEBVTT\n\n")
    }

    fn write_cue(&mut self, out: &mut IoWriter, _index: usize, cue: &Cue) -> Result<()> {
        out.write(
            format!(
                "{} --> {}\n",
                format_vtt_time(cue.start),
                format_vtt_time(cue.end)
            )
            .as_bytes(),
        )?;
        out.write(&cue.text)?;
        out.write(b"\n\n")
    }
}

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(GenericTextMuxer::new(sink, WebvttMux, MUX_FLAGS)?))
}

/// The muxer descriptor.
pub const MUXER: MuxerDesc = MuxerDesc {
    name: "webvtt",
    long_name: "WebVTT subtitle",
    extensions: &["vtt"],
    default_video: None,
    default_audio: None,
    open: open_muxer,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_core::Duration;

    const SAMPLE: &[u8] = b"WEBVTT\n\n00:00:01.000 --> 00:00:02.500\nHello\nworld\n\n00:00:03.000 --> 00:00:04.000 align:start\nSecond\n";

    #[test]
    fn parses_two_cues_and_ignores_cue_settings() {
        let cues = parse(SAMPLE);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, b"Hello\nworld");
        assert_eq!(cues[1].start, Duration::from_micros(3_000_000));
        assert_eq!(cues[1].text, b"Second");
    }

    #[test]
    fn short_form_without_hours_is_accepted() {
        let cues = parse(b"WEBVTT\n\n00:01.000 --> 00:02.000\nHi\n");
        assert_eq!(cues[0].start, Duration::from_micros(1_000_000));
    }

    #[test]
    fn probe_requires_the_signature() {
        assert_eq!(
            probe(&ProbeData::new(b"Not a vtt file at all")),
            ProbeScore::NONE
        );
        assert!(
            probe(&ProbeData::new(
                b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHi\n"
            ))
            .value()
                > 0
        );
    }

    #[test]
    fn probe_rejects_an_srt_comma_timing_line() {
        // "WEBVTT" text alone with no valid VTT timing does not, by itself,
        // exercise the signature path meaningfully beyond the prior test;
        // this checks a bare SRT file (no WEBVTT signature) scores zero.
        let data = ProbeData::new(b"1\n00:00:01,000 --> 00:00:02,000\nHello\n\n");
        assert_eq!(probe(&data), ProbeScore::NONE);
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
        let mut mux = GenericTextMuxer::new(Box::new(sink), WebvttMux, MUX_FLAGS).unwrap();
        mux.add_stream(&CodecParameters::new(MediaType::Subtitle).with_codec(CodecId::Webvtt))
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
        let round_tripped = parse(&shared.snapshot());
        assert_eq!(round_tripped.len(), 2);
        assert_eq!(round_tripped[0].text, b"Hello\nworld");
    }

    #[test]
    fn never_panics_on_arbitrary_bytes() {
        for len in 0..48 {
            let buf: Vec<u8> = (0..len).map(|i| (i * 41 % 256) as u8).collect();
            let _ = parse(&buf);
            let _ = probe(&ProbeData::new(&buf));
        }
    }
}
