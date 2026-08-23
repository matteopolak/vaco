//! `MicroDVD` (`.sub`) — frame-numbered cues.
//!
//! `{start}{end}text`, one cue per line. Measured (D17): the numbers are
//! frame counts, and the default frame rate absent an explicit `{1}{1}<fps>`
//! header line on the first line is **23.976 (24000/1001)**, not 25 — see
//! [`vaco_format_subtitle::time::MICRODVD_DEFAULT_FPS`]. `{y:i}`/`{c:$...}`
//! style markup inside the text is left untouched; interpreting it is a
//! decoder's job.

use vaco_codec_core::CodecId;
use vaco_core::Result;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{Demuxer, DemuxerDesc, Muxer, MuxerDesc, ParserProvider};
use vaco_format_subtitle::Cue;
use vaco_format_subtitle::text::lines;
use vaco_format_subtitle::time::{
    MICRODVD_DEFAULT_FPS, microdvd_duration_to_frame, microdvd_frame_to_duration,
};
use vaco_io::{IoWriter, MediaSink, MediaSource};

use crate::engine::{self, CueMux, DEMUX_FLAGS, GenericTextMuxer, MUX_FLAGS};

/// Parse one `{n}{n}text` line's two braced numbers and the text after them.
fn parse_braces(line: &[u8]) -> Option<(i64, i64, &[u8])> {
    let rest = line.strip_prefix(b"{")?;
    let (a, rest) = split_at_close_brace(rest)?;
    let rest = rest.strip_prefix(b"{")?;
    let (b, rest) = split_at_close_brace(rest)?;
    let a: i64 = std::str::from_utf8(a).ok()?.parse().ok()?;
    let b: i64 = std::str::from_utf8(b).ok()?.parse().ok()?;
    Some((a, b, rest))
}

fn split_at_close_brace(input: &[u8]) -> Option<(&[u8], &[u8])> {
    let pos = input.iter().position(|&b| b == b'}')?;
    Some((input.get(..pos)?, input.get(pos.saturating_add(1)..)?))
}

fn parse(bytes: &[u8]) -> Vec<Cue> {
    let mut fps = MICRODVD_DEFAULT_FPS;
    let mut cues = Vec::new();
    for (i, line) in lines(bytes).into_iter().enumerate() {
        let Some((start_f, end_f, text)) = parse_braces(line) else {
            continue;
        };
        // A `{1}{1}<fps>` first line declares the rate and is not itself a cue.
        if i == 0 && start_f == 1 && end_f == 1 {
            if let Ok(s) = std::str::from_utf8(text)
                && let Ok(declared) = s.trim().parse::<f64>()
                && declared.is_finite()
                && declared > 0.0
            {
                fps = declared;
            }
            continue;
        }
        cues.push(Cue::new(
            microdvd_frame_to_duration(start_f, fps),
            microdvd_frame_to_duration(end_f, fps),
            // MicroDVD packs multiple lines of one cue with `|`, not `\n`; that
            // is the format's own convention and is left untouched here.
            text.to_vec(),
        ));
    }
    cues
}

/// Content probe: lines matching `{n}{n}...`.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let hits = lines(data.buf)
        .iter()
        .filter(|l| parse_braces(l).is_some())
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
    engine::open_generic(src, Some(CodecId::Microdvd), parse)
}

/// The demuxer descriptor.
///
/// `CodecId::Microdvd` — matches the reference's own `codec_name=microdvd`.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "microdvd",
    long_name: "MicroDVD subtitle",
    extensions: &["sub"],
    mime_types: &["text/x-microdvd"],
    flags: DEMUX_FLAGS,
    probe,
    open: open_demuxer,
};

#[derive(Debug)]
struct MicrodvdMux {
    fps: f64,
}

impl Default for MicrodvdMux {
    fn default() -> Self {
        Self {
            fps: MICRODVD_DEFAULT_FPS,
        }
    }
}

impl CueMux for MicrodvdMux {
    fn accepts(&self, codec_id: Option<CodecId>) -> bool {
        matches!(codec_id, Some(CodecId::Microdvd))
    }

    fn write_cue(&mut self, out: &mut IoWriter, _index: usize, cue: &Cue) -> Result<()> {
        let start = microdvd_duration_to_frame(cue.start, self.fps);
        let end = microdvd_duration_to_frame(cue.end, self.fps);
        out.write(format!("{{{start}}}{{{end}}}").as_bytes())?;
        out.write(&cue.text)?;
        out.write(b"\n")
    }
}

fn open_muxer(sink: Box<dyn MediaSink>) -> Result<Box<dyn Muxer>> {
    Ok(Box::new(GenericTextMuxer::new(
        sink,
        MicrodvdMux::default(),
        MUX_FLAGS,
    )?))
}

/// The muxer descriptor.
pub const MUXER: MuxerDesc = MuxerDesc {
    name: "microdvd",
    long_name: "MicroDVD subtitle",
    extensions: &["sub"],
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
    fn parses_frames_at_the_measured_default_rate() {
        let cues = parse(b"{0}{25}Hello world\n{25}{50}Second\n");
        assert_eq!(cues.len(), 2);
        assert!((cues[0].end.as_micros() - 1_042_709).abs() <= 1);
    }

    #[test]
    fn a_declared_fps_header_line_is_honoured_and_not_emitted_as_a_cue() {
        let cues = parse(b"{1}{1}25.000\n{0}{25}Hello\n");
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].end, Duration::from_micros(1_000_000));
    }

    #[test]
    fn probe_rejects_plain_prose() {
        assert_eq!(
            probe(&ProbeData::new(
                b"Ordinary prose without any braces here.\n"
            )),
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
        let mut mux =
            GenericTextMuxer::new(Box::new(sink), MicrodvdMux::default(), MUX_FLAGS).unwrap();
        mux.add_stream(&CodecParameters::new(MediaType::Subtitle).with_codec(CodecId::Microdvd))
            .unwrap();
        mux.write_header().unwrap();
        let mut budget = Budget::new(Limits::permissive());
        let mut pkt = Packet::from_slice(&mut budget, b"Hi").unwrap();
        pkt.pts = vaco_core::Timestamp::new(0);
        pkt.duration = Duration::from_micros(1_042_709);
        mux.write_packet(&pkt).unwrap();
        mux.write_trailer().unwrap();
        let cues = parse(&shared.snapshot());
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, b"Hi");
    }
}
