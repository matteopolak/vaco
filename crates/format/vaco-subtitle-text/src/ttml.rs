//! TTML1 (Timed Text Markup Language), per the W3C TTML1 recommendation.
//!
//! # The one format in this crate with no reference demuxer
//!
//! Measured (D17): `ffmpeg -muxers` lists `ttml`, but `ffmpeg -demuxers` does
//! not — there is no TTML file demuxer in the reference at all. This module's
//! [`DEMUXER`] is therefore implemented from the W3C TTML1 spec directly,
//! with nothing to differential-test it against; it is exactly as solid as
//! its test suite and no more. Everything else in this crate that claims a
//! `DEMUXER` const has a reference implementation it was checked against —
//! this one does not, and that is flagged here rather than left implicit.
//!
//! Only `<p>` elements' `begin`/`end`/`dur` attributes and text content are
//! read; `<style>`/`<region>`/nested `<span>` styling, and TTML's `tick`
//! (`t`) and `frame` (`f`) time-metric suffixes, are not implemented — a
//! `<p>` using them is skipped rather than mistimed. `<br/>` becomes `\n`.

#![allow(
    clippy::integer_division,
    reason = "exact integer div/mod against fixed bases (3600, 1_000_000, 1_000), same as vaco_format_subtitle::time"
)]

use quick_xml::Reader;
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesRef, Event};

use vaco_codec_core::CodecId;
use vaco_core::{Duration, Result};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{Demuxer, DemuxerDesc, Muxer, MuxerDesc, ParserProvider};
use vaco_format_subtitle::Cue;
use vaco_io::{IoWriter, MediaSink, MediaSource};

use crate::engine::{self, CueMux, DEMUX_FLAGS, GenericTextMuxer, MUX_FLAGS};

/// A TTML clock-time (`HH:MM:SS(.fraction)?`) or a `<seconds>s` /
/// `<milliseconds>ms` offset-time. Frame (`f`) and tick (`t`) metrics are not
/// supported and fail to parse.
fn parse_ttml_time(s: &str) -> Option<Duration> {
    let s = s.trim();
    if let Some(rest) = s.strip_suffix("ms") {
        let v: f64 = rest.parse().ok()?;
        return Some(Duration::from_micros((v * 1_000.0).round() as i64));
    }
    if let Some(rest) = s.strip_suffix('s') {
        let v: f64 = rest.parse().ok()?;
        return Some(Duration::from_micros((v * 1_000_000.0).round() as i64));
    }
    let mut it = s.split(':');
    let h: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let sec_field = it.next()?;
    if it.next().is_some() {
        return None; // a fourth (frame) field is not supported.
    }
    let (sec, frac) = sec_field.split_once('.').unwrap_or((sec_field, ""));
    let sec: i64 = sec.parse().ok()?;
    let frac_us: i64 = if frac.is_empty() {
        0
    } else {
        let width = frac.len().min(6);
        let digits = frac.get(..width)?;
        let value: i64 = digits.parse().ok()?;
        value.saturating_mul(10i64.pow(u32::try_from(6 - width).ok()?))
    };
    Some(Duration::from_micros(
        (h.checked_mul(3600)?
            .checked_add(m.checked_mul(60)?)?
            .checked_add(sec)?)
        .checked_mul(1_000_000)?
        .checked_add(frac_us)?,
    ))
}

fn format_ttml_time(d: Duration) -> String {
    let us = d.as_micros().max(0);
    let total_secs = us / 1_000_000;
    let ms = (us % 1_000_000) / 1_000;
    format!(
        "{:02}:{:02}:{:02}.{ms:03}",
        total_secs / 3600,
        (total_secs / 60) % 60,
        total_secs % 60
    )
}

/// The replacement text of one `&...;` reference. `quick-xml` reports general
/// and character references as their own events instead of folding them into
/// the surrounding `Text`, so a parser that matches only `Text` drops them.
fn entity_text(r: &BytesRef<'_>) -> Option<String> {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        return Some(c.to_string());
    }
    resolve_predefined_entity(r).map(str::to_owned)
}

fn parse(bytes: &[u8]) -> Vec<Cue> {
    let mut reader = Reader::from_reader(bytes);
    let mut buf = Vec::new();
    let mut cues = Vec::new();
    let mut in_p = false;
    let mut begin: Option<Duration> = None;
    let mut end: Option<Duration> = None;
    let mut dur: Option<Duration> = None;
    let mut text: Vec<u8> = Vec::new();
    while let Ok(event) = reader.read_event_into(&mut buf) {
        match event {
            Event::Start(e) if e.local_name().as_ref() == "p" => {
                in_p = true;
                begin = None;
                end = None;
                dur = None;
                text.clear();
                for attr in e.attributes().flatten() {
                    let value = attr.value.as_ref();
                    match attr.key.local_name().as_ref() {
                        "begin" => begin = parse_ttml_time(value),
                        "end" => end = parse_ttml_time(value),
                        "dur" => dur = parse_ttml_time(value),
                        _ => {}
                    }
                }
            }
            Event::End(e) if in_p && e.local_name().as_ref() == "p" => {
                in_p = false;
                if let Some(start) = begin {
                    let resolved_end = end.unwrap_or_else(|| {
                        Duration::from_micros(
                            start
                                .as_micros()
                                .saturating_add(dur.map_or(0, Duration::as_micros)),
                        )
                    });
                    cues.push(Cue::new(start, resolved_end, std::mem::take(&mut text)));
                }
            }
            Event::Text(e) if in_p => {
                text.extend_from_slice(e.xml10_content().as_bytes());
            }
            // A reference arrives as its own event, not folded into `Text`.
            Event::GeneralRef(r) if in_p => {
                if let Some(resolved) = entity_text(&r) {
                    text.extend_from_slice(resolved.as_bytes());
                }
            }
            Event::Empty(e) if in_p && e.local_name().as_ref() == "br" => {
                text.push(b'\n');
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    cues
}

/// Content probe: a `<tt` root-ish signature plus at least one `<p` element.
/// Weaker than the other formats' probes because there is nothing to measure
/// this against — see the module docs.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let text = String::from_utf8_lossy(data.buf);
    let has_tt = text.contains("<tt ")
        || text.contains("<tt>")
        || text.contains("xmlns=\"http://www.w3.org/ns/ttml\"");
    let has_p = text.contains("<p ") || text.contains("<p>");
    if has_tt && has_p {
        ProbeScore::CONTENT
    } else if has_tt {
        ProbeScore::weak(15)
    } else {
        ProbeScore::from_extension(data, &["ttml"])
    }
}

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    engine::open_generic(src, Some(CodecId::Ttml), parse)
}

/// The demuxer descriptor. Spec-only, not reference-verified — see the module
/// docs. `CodecId::Ttml`.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "ttml",
    long_name: "TTML subtitle",
    extensions: &["ttml"],
    mime_types: &["application/ttml+xml"],
    flags: DEMUX_FLAGS,
    probe,
    open: open_demuxer,
};

fn xml_escape(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

#[derive(Debug, Default)]
struct TtmlMux;

impl CueMux for TtmlMux {
    fn accepts(&self, codec_id: Option<CodecId>) -> bool {
        matches!(codec_id, Some(CodecId::Ttml))
    }

    fn write_header(&mut self, out: &mut IoWriter) -> Result<()> {
        out.write(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n")?;
        out.write(b"<tt xmlns=\"http://www.w3.org/ns/ttml\">\n<body>\n<div>\n")
    }

    fn write_cue(&mut self, out: &mut IoWriter, _index: usize, cue: &Cue) -> Result<()> {
        out.write(
            format!(
                "<p begin=\"{}\" end=\"{}\">{}</p>\n",
                format_ttml_time(cue.start),
                format_ttml_time(cue.end),
                xml_escape(&cue.text)
            )
            .as_bytes(),
        )
    }

    fn write_trailer(&mut self, out: &mut IoWriter) -> Result<()> {
        out.write(b"</div>\n</body>\n</tt>\n")
    }
}

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(GenericTextMuxer::new(sink, TtmlMux, MUX_FLAGS)?))
}

/// The muxer descriptor.
pub const MUXER: MuxerDesc = MuxerDesc {
    name: "ttml",
    long_name: "TTML subtitle",
    extensions: &["ttml"],
    default_video: None,
    default_audio: None,
    open: open_muxer,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = b"<?xml version=\"1.0\"?>\n<tt xmlns=\"http://www.w3.org/ns/ttml\"><body><div>\n<p begin=\"00:00:01.000\" end=\"00:00:02.500\">Hello<br/>world</p>\n<p begin=\"00:00:03.000\" dur=\"1.5s\">Second</p>\n</div></body></tt>\n";

    #[test]
    fn parses_begin_end_and_a_br_as_newline() {
        let cues = parse(SAMPLE);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start, Duration::from_micros(1_000_000));
        assert_eq!(cues[0].end, Duration::from_micros(2_500_000));
        assert_eq!(cues[0].text, b"Hello\nworld");
    }

    #[test]
    fn dur_resolves_the_end_when_end_is_absent() {
        let cues = parse(SAMPLE);
        assert_eq!(cues[1].start, Duration::from_micros(3_000_000));
        assert_eq!(cues[1].end, Duration::from_micros(4_500_000));
    }

    #[test]
    fn probe_rejects_plain_prose_and_generic_xml() {
        assert_eq!(
            probe(&ProbeData::new(b"Just some prose.")),
            ProbeScore::NONE
        );
        assert_eq!(
            probe(&ProbeData::new(b"<root><item>not ttml</item></root>")),
            ProbeScore::NONE
        );
    }

    #[test]
    fn escapes_ampersand_and_angle_brackets_on_write() {
        assert_eq!(xml_escape(b"A & B < C > D"), "A &amp; B &lt; C &gt; D");
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
        let mut mux = GenericTextMuxer::new(Box::new(sink), TtmlMux, MUX_FLAGS).unwrap();
        mux.add_stream(&CodecParameters::new(MediaType::Subtitle).with_codec(CodecId::Ttml))
            .unwrap();
        mux.write_header().unwrap();
        let mut budget = Budget::new(Limits::permissive());
        let mut pkt = Packet::from_slice(&mut budget, b"Hi & bye").unwrap();
        pkt.pts = vaco_core::Timestamp::new(1_000_000);
        pkt.duration = Duration::from_micros(2_000_000);
        mux.write_packet(&pkt).unwrap();
        mux.write_trailer().unwrap();
        let cues = parse(&shared.snapshot());
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, b"Hi & bye");
        assert_eq!(cues[0].start, Duration::from_micros(1_000_000));
    }

    #[test]
    fn never_panics_on_arbitrary_bytes() {
        for len in 0..64 {
            let buf: Vec<u8> = (0..len).map(|i| (i * 53 % 256) as u8).collect();
            let _ = parse(&buf);
        }
    }
}
