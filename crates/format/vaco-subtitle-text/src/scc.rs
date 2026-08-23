//! Scenarist Closed Captions (`.scc`) — CEA-608 data, hex-encoded as text.
//!
//! Structurally this is the odd one out in this crate: the "text" is a
//! transcript of a binary bitstream, and the reference's own muxer/demuxer
//! pair calls its codec `eia_608` (`CodecId::Eia608`, already in
//! `vaco-codec-core` — this is the one format here that did not need a new
//! variant). Each non-blank line after the `Scenarist_SCC V1.0` header is a
//! timecode followed by whitespace-separated four-hex-digit pairs; measured
//! against the reference, each pair `AABB` becomes a three-byte CEA-608
//! triplet `FC AA BB` (`0xFC` is the "valid, field 1" marker byte), and every
//! pair on one line concatenates into a single packet.
//!
//! # Measured simplification
//!
//! The timecode is read as non-drop-frame: `HH:MM:SS` counts real seconds
//! exactly and the frame field adds a nominal `f/30` of a second, whether the
//! separator before it is `:` or `;` (drop-frame's own signal, which this
//! parser does not distinguish). Real SCC files at true NTSC 30000/1001 fps
//! drift from this by roughly 0.1% over an hour; that is not a byte-exact
//! match of the reference's own drop-frame arithmetic, and is flagged here
//! rather than silently assumed correct.

#![allow(
    clippy::integer_division,
    reason = "exact integer div/mod against fixed bases (30, 60, 3600, 1_000_000), same as vaco_format_subtitle::time"
)]

use vaco_codec_core::CodecId;
use vaco_core::{Duration, Result};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{Demuxer, DemuxerDesc, Muxer, MuxerDesc, ParserProvider};
use vaco_format_subtitle::Cue;
use vaco_io::{IoWriter, MediaSink, MediaSource};

use crate::engine::{self, CueMux, DEMUX_FLAGS, GenericTextMuxer, MUX_FLAGS};

const HEADER_LINE: &str = "Scenarist_SCC V1.0";

/// Timecode-to-microseconds, non-drop-frame: `HH:MM:SS` counts real seconds
/// exactly, and the frame field adds `f/30` of a second on top. This is the
/// simplification the module docs flag — a true NTSC 30000/1001 reading
/// would stretch the whole-seconds part too, which is exactly the drift a
/// non-drop-frame *display* convention exists to avoid, so anchoring on
/// nominal 30 for the sub-second remainder is the closer approximation of
/// the two, not an arbitrary choice.
fn timecode_micros(h: i64, m: i64, s: i64, f: i64) -> i64 {
    // Fuzz-found (`crash-74c42c43e02d8c49a5f9d5354d5dabb63fbdda80`): `parse_timecode`
    // places no bound on how many digits an `HH`/`MM`/`SS`/`FF` field may
    // have, so each parses to any `i64` a decimal string can spell — and the
    // combination below used plain `*`/`+`, which panics on overflow under
    // the fuzz profile's checked arithmetic (`planning/AGENT-CONSTRAINTS.md`,
    // "Overflow must not panic under the fuzz profile"). Saturating
    // throughout turns an absurd timecode into a clamped-but-representable
    // duration instead of a crash, which is the right lenient-demuxer answer
    // for a malformed field, not just a safe one.
    let secs = h
        .saturating_mul(3600)
        .saturating_add(m.saturating_mul(60))
        .saturating_add(s);
    let whole = secs.saturating_mul(1_000_000);
    let frac = (i128::from(f) * 1_000_000 + 15) / 30;
    whole.saturating_add(i64::try_from(frac).unwrap_or(i64::MAX))
}

fn parse_timecode(s: &str) -> Option<Duration> {
    let s = s.replace(';', ":");
    let mut it = s.split(':');
    let h = it.next()?.parse().ok()?;
    let m = it.next()?.parse().ok()?;
    let sec = it.next()?.parse().ok()?;
    let f = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some(Duration::from_micros(timecode_micros(h, m, sec, f)))
}

fn decode_pair(hex: &str) -> Option<[u8; 3]> {
    if hex.len() != 4 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let hi = u8::from_str_radix(hex.get(0..2)?, 16).ok()?;
    let lo = u8::from_str_radix(hex.get(2..4)?, 16).ok()?;
    Some([0xFC, hi, lo])
}

/// One parsed line: its start time and the concatenated CEA-608 triplets.
fn parse_line(line: &str) -> Option<(Duration, Vec<u8>)> {
    let mut parts = line.split_whitespace();
    let start = parse_timecode(parts.next()?)?;
    let mut payload = Vec::new();
    for tok in parts {
        if let Some(triplet) = decode_pair(tok) {
            payload.extend_from_slice(&triplet);
        }
    }
    Some((start, payload))
}

fn parse(bytes: &[u8]) -> Vec<Cue> {
    let text = String::from_utf8_lossy(bytes);
    let entries: Vec<(Duration, Vec<u8>)> = text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with(HEADER_LINE))
        .filter_map(parse_line)
        .collect();
    let mut cues = Vec::new();
    for (i, (start, payload)) in entries.iter().enumerate() {
        let end = entries.get(i + 1).map_or(*start, |(next, _)| *next);
        cues.push(Cue::new(*start, end, payload.clone()));
    }
    cues
}

/// Content probe: the `Scenarist_SCC` header, or — absent that — a run of
/// lines that each parse as `timecode` + hex pairs.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let text = String::from_utf8_lossy(data.buf);
    if text.trim_start().starts_with(HEADER_LINE) {
        return ProbeScore::MAGIC_CHECKED;
    }
    let hits = text.lines().filter(|l| parse_line(l).is_some()).count();
    if hits > 0 {
        ProbeScore::repeating(hits as u32)
    } else {
        ProbeScore::from_extension(data, &["scc"])
    }
}

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    engine::open_generic(src, Some(CodecId::Eia608), parse)
}

/// The demuxer descriptor.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "scc",
    long_name: "Scenarist Closed Captions",
    extensions: &["scc"],
    mime_types: &[],
    flags: DEMUX_FLAGS,
    probe,
    open: open_demuxer,
};

#[derive(Debug, Default)]
struct SccMux;

#[allow(
    clippy::many_single_char_names,
    reason = "h, m, s, f are the standard names for a timecode's own fields"
)]
fn format_timecode(d: Duration) -> String {
    let us = d.as_micros().max(0);
    let total_secs = us / 1_000_000;
    let frac_us = us % 1_000_000;
    let f = (frac_us * 30) / 1_000_000;
    let s = total_secs % 60;
    let m = (total_secs / 60) % 60;
    let h = total_secs / 3600;
    format!("{h:02}:{m:02}:{s:02}:{f:02}")
}

impl CueMux for SccMux {
    fn accepts(&self, codec_id: Option<CodecId>) -> bool {
        matches!(codec_id, Some(CodecId::Eia608))
    }

    fn write_header(&mut self, out: &mut IoWriter) -> Result<()> {
        out.write(format!("{HEADER_LINE}\n\n").as_bytes())
    }

    fn write_cue(&mut self, out: &mut IoWriter, _index: usize, cue: &Cue) -> Result<()> {
        out.write(format_timecode(cue.start).as_bytes())?;
        for triplet in cue.text.chunks(3) {
            if let [_, a, b] = *triplet {
                out.write(format!("\t{a:02x}{b:02x}").as_bytes())?;
            }
        }
        out.write(b"\n\n")
    }
}

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(GenericTextMuxer::new(sink, SccMux, MUX_FLAGS)?))
}

/// The muxer descriptor.
pub const MUXER: MuxerDesc = MuxerDesc {
    name: "scc",
    long_name: "Scenarist Closed Captions",
    extensions: &["scc"],
    default_video: None,
    default_audio: None,
    open: open_muxer,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    const SAMPLE: &[u8] =
        b"Scenarist_SCC V1.0\n\n00:00:01:00\t9420 9420 942c 942c\n\n00:00:02:00\t8080\n\n";

    #[test]
    fn huge_timecode_fields_saturate_instead_of_overflowing() {
        // Regression for a fuzz-found panic
        // (`fuzz/seeds/subtitle_text_demux/regression-scc-timecode-overflow-crash-74c42c43`):
        // `parse_timecode` places no bound on a field's digit count, so a
        // value that survives `i64::parse` but overflows `* 3600` used to
        // panic under the fuzz profile's checked arithmetic.
        let huge = i64::MAX / 100;
        assert_eq!(timecode_micros(huge, 0, 0, 0), i64::MAX);
        assert!(parse_timecode(&format!("{huge}:0:0:0")).is_some());
        // The whole demuxer, not just the helper, must survive it.
        let line = format!("{huge}:0:0:0\t9420\n");
        let _ = parse(line.as_bytes());
    }

    #[test]
    fn decodes_hex_pairs_into_cea608_triplets() {
        let cues = parse(SAMPLE);
        assert_eq!(cues.len(), 2);
        assert_eq!(
            cues[0].text,
            vec![
                0xFC, 0x94, 0x20, 0xFC, 0x94, 0x20, 0xFC, 0x94, 0x2C, 0xFC, 0x94, 0x2C
            ]
        );
        assert_eq!(cues[0].start, Duration::from_micros(1_000_000));
    }

    #[test]
    fn probe_rejects_plain_prose() {
        assert_eq!(
            probe(&ProbeData::new(
                b"Just a normal sentence of English text.\n"
            )),
            ProbeScore::NONE
        );
    }

    #[test]
    fn probe_accepts_the_header() {
        assert_eq!(probe(&ProbeData::new(SAMPLE)), ProbeScore::MAGIC_CHECKED);
    }

    #[test]
    fn round_trip_preserves_triplets() {
        use vaco_codec_core::CodecParameters;
        use vaco_core::MediaType;
        use vaco_format_core::vacoraw::MemorySink;
        use vaco_limits::{Budget, Limits};
        use vaco_packet::Packet;

        let sink = MemorySink::new();
        let shared = sink.shared();
        let mut mux = GenericTextMuxer::new(Box::new(sink), SccMux, MUX_FLAGS).unwrap();
        mux.add_stream(&CodecParameters::new(MediaType::Subtitle).with_codec(CodecId::Eia608))
            .unwrap();
        mux.write_header().unwrap();
        for cue in parse(SAMPLE) {
            let mut budget = Budget::new(Limits::permissive());
            let mut pkt = Packet::from_slice(&mut budget, &cue.text).unwrap();
            pkt.pts = vaco_core::Timestamp::new(cue.start.as_micros());
            mux.write_packet(&pkt).unwrap();
        }
        mux.write_trailer().unwrap();
        let round_tripped = parse(&shared.snapshot());
        assert_eq!(round_tripped.len(), 2);
        assert_eq!(
            round_tripped[0].text,
            vec![
                0xFC, 0x94, 0x20, 0xFC, 0x94, 0x20, 0xFC, 0x94, 0x2C, 0xFC, 0x94, 0x2C
            ]
        );
    }
}
