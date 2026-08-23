//! `SubStation` Alpha / Advanced `SubStation` Alpha (`.ssa`, `.ass`), per the
//! published ASS specification's `[Script Info]` / `[V4+ Styles]` /
//! `[Events]` section structure.
//!
//! Measured (D17): one demuxer handles both script versions and reports
//! `codec_name=ass` for either — a `ScriptType: v4.00` (classic SSA) script
//! and a `v4.00+` (ASS) script both measure the same, so this module (and its
//! `CodecId::Ass` tag) does not attempt to distinguish them. `CodecId::Ssa`
//! exists in `vaco-codec-core` but nothing in the reference's file demuxer
//! produces it.
//!
//! The `Format:` line under `[Events]` names each `Dialogue:` line's field
//! order — this parser reads it rather than assuming the common
//! `Layer,Start,End,Style,...,Text` order, because a script that lists fields
//! differently is still valid ASS. `Text` is always the *last* field and may
//! itself contain commas, so a `Dialogue:` line is split at most
//! `field_count - 1` times.
//!
//! This parser works at the `str` level (not the byte level the way
//! [`crate::srt`] and [`crate::webvtt`] do) because finding a named field in
//! a comma-separated `Format:` line is naturally a string operation; a
//! `Dialogue:` line containing invalid UTF-8 is dropped rather than passed
//! through raw, which is the one format in this crate where that trade-off
//! was made for implementation simplicity rather than being a measured
//! reference behaviour.

use vaco_codec_core::CodecId;
use vaco_core::Result;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{Demuxer, DemuxerDesc, Muxer, MuxerDesc, ParserProvider};
use vaco_format_subtitle::Cue;
use vaco_format_subtitle::time::{format_ass_time, parse_ass_time};
use vaco_io::{IoWriter, MediaSink, MediaSource};

use crate::engine::{self, CueMux, DEMUX_FLAGS, GenericTextMuxer, MUX_FLAGS};

/// Content probe: `[Script Info]` plus one or more `Dialogue:` lines that
/// actually parse. `[Script Info]` alone (no events yet) still scores inside
/// the retry band, since a header-only fragment is plausibly ASS but not
/// confirmed.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let text = String::from_utf8_lossy(data.buf);
    let has_header = text.contains("[Script Info]")
        || text.contains("[V4+ Styles]")
        || text.contains("[V4 Styles]");
    let dialogue_hits = text
        .lines()
        .filter(|l| l.trim_start().starts_with("Dialogue:"))
        .count();
    if dialogue_hits > 0 {
        return ProbeScore::repeating(dialogue_hits as u32);
    }
    if has_header {
        return ProbeScore::weak(20);
    }
    ProbeScore::from_extension(data, &["ass", "ssa"])
}

struct EventFormat {
    field_count: usize,
    start: usize,
    end: usize,
}

impl Default for EventFormat {
    fn default() -> Self {
        // The conventional order, used when no `Format:` line precedes the
        // first `Dialogue:` line — malformed, but recoverable.
        Self {
            field_count: 10,
            start: 1,
            end: 2,
        }
    }
}

fn parse(bytes: &[u8]) -> Vec<Cue> {
    let text = String::from_utf8_lossy(bytes);
    let mut cues = Vec::new();
    let mut in_events = false;
    let mut fmt = EventFormat::default();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_events = trimmed.eq_ignore_ascii_case("[events]");
            continue;
        }
        if !in_events {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Format:") {
            let fields: Vec<&str> = rest.split(',').map(str::trim).collect();
            fmt.field_count = fields.len().max(1);
            fmt.start = fields
                .iter()
                .position(|f| f.eq_ignore_ascii_case("Start"))
                .unwrap_or(1);
            fmt.end = fields
                .iter()
                .position(|f| f.eq_ignore_ascii_case("End"))
                .unwrap_or(2);
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("Dialogue:") else {
            continue;
        };
        let parts: Vec<&str> = rest.trim_start().splitn(fmt.field_count, ',').collect();
        let needed = fmt.start.max(fmt.end);
        if parts.len() <= needed {
            continue;
        }
        let start = parts.get(fmt.start).and_then(|p| parse_ass_time(p.trim()));
        let end = parts.get(fmt.end).and_then(|p| parse_ass_time(p.trim()));
        let (Some(start), Some(end)) = (start, end) else {
            continue;
        };
        let cue_text = parts.last().copied().unwrap_or("");
        cues.push(Cue::new(start, end, cue_text.as_bytes().to_vec()));
    }
    cues
}

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    engine::open_generic(src, Some(CodecId::Ass), parse)
}

/// The demuxer descriptor.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "ass,ssa",
    long_name: "ASS (Advanced SubStation Alpha) subtitle",
    extensions: &["ass", "ssa"],
    mime_types: &["text/x-ass"],
    flags: DEMUX_FLAGS,
    probe,
    open: open_demuxer,
};

#[derive(Debug, Default)]
struct AssMux;

const HEADER: &str = "[Script Info]\nScriptType: v4.00+\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,0,2,10,10,10,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n";

impl CueMux for AssMux {
    fn accepts(&self, codec_id: Option<CodecId>) -> bool {
        matches!(codec_id, Some(CodecId::Ass))
    }

    fn write_header(&mut self, out: &mut IoWriter) -> Result<()> {
        out.write(HEADER.as_bytes())
    }

    fn write_cue(&mut self, out: &mut IoWriter, _index: usize, cue: &Cue) -> Result<()> {
        out.write(
            format!(
                "Dialogue: 0,{},{},Default,,0,0,0,,",
                format_ass_time(cue.start),
                format_ass_time(cue.end)
            )
            .as_bytes(),
        )?;
        out.write(&cue.text)?;
        out.write(b"\n")
    }
}

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(GenericTextMuxer::new(sink, AssMux, MUX_FLAGS)?))
}

/// The muxer descriptor.
pub const MUXER: MuxerDesc = MuxerDesc {
    name: "ass",
    long_name: "ASS (Advanced SubStation Alpha) subtitle",
    extensions: &["ass"],
    default_video: None,
    default_audio: None,
    open: open_muxer,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_core::Duration;

    const SAMPLE: &[u8] = b"[Script Info]\nScriptType: v4.00+\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.00,0:00:03.00,Default,,0,0,0,,Hello, world\n";

    #[test]
    fn parses_dialogue_with_a_comma_in_the_text() {
        let cues = parse(SAMPLE);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start, Duration::from_micros(1_000_000));
        assert_eq!(cues[0].end, Duration::from_micros(3_000_000));
        assert_eq!(cues[0].text, b"Hello, world");
    }

    #[test]
    fn honours_a_reordered_format_line() {
        // `Text` is always the last field in real ASS (it is the one field
        // allowed to contain unescaped commas), but `Start`/`End` may be
        // anywhere before it — this checks their position is read from the
        // `Format:` line rather than assumed.
        let sample =
            b"[Events]\nFormat: End, Start, Text\nDialogue: 0:00:02.00,0:00:01.00,hi there\n";
        let cues = parse(sample);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start, Duration::from_micros(1_000_000));
        assert_eq!(cues[0].end, Duration::from_micros(2_000_000));
        assert_eq!(cues[0].text, b"hi there");
    }

    #[test]
    fn probe_rejects_plain_prose() {
        assert_eq!(
            probe(&ProbeData::new(b"Just an ordinary paragraph of text.\n")),
            ProbeScore::NONE
        );
    }

    #[test]
    fn probe_accepts_real_ass() {
        assert!(probe(&ProbeData::new(SAMPLE)).value() > ProbeScore::RETRY.value());
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
        let mut mux = GenericTextMuxer::new(Box::new(sink), AssMux, MUX_FLAGS).unwrap();
        mux.add_stream(&CodecParameters::new(MediaType::Subtitle).with_codec(CodecId::Ass))
            .unwrap();
        mux.write_header().unwrap();
        let mut budget = Budget::new(Limits::permissive());
        let mut pkt = Packet::from_slice(&mut budget, b"Hi there").unwrap();
        pkt.pts = vaco_core::Timestamp::new(1_000_000);
        pkt.duration = Duration::from_micros(2_000_000);
        mux.write_packet(&pkt).unwrap();
        mux.write_trailer().unwrap();
        let cues = parse(&shared.snapshot());
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, b"Hi there");
        assert_eq!(cues[0].start, Duration::from_micros(1_000_000));
    }
}
