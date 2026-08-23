//! SAMI (`.smi`) — demux only.
//!
//! `<SYNC Start=NNNN><P ...>text` repeating; `Start` is plain milliseconds,
//! absolute, no quotes required. A cue's end is its successor's `Start`; the
//! last cue gets zero duration (see [`crate::lrc`]).
//!
//! Measured (D17), and the reason this parser splits the file at `<SYNC`
//! occurrences instead of extracting clean text between them: **the packet
//! payload includes the `<SYNC ...><P ...>` markup itself**, not just the
//! text that follows it. `ffprobe -f sami -show_packets -show_data` on
//! `<SYNC Start=1000><P>Hello world` produces a payload starting with the
//! literal `<SYNC Start=1000><P>Hello world` bytes — the reference does not
//! strip its own timing tag out of the cue text, and this parser reproduces
//! that rather than "cleaning it up" into something the reference does not
//! actually produce.

use vaco_codec_core::CodecId;
use vaco_core::{Duration, Result};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{Demuxer, DemuxerDesc, ParserProvider};
use vaco_format_subtitle::Cue;
use vaco_io::MediaSource;

use crate::engine::{self, DEMUX_FLAGS};

/// The digits immediately after `name=`, skipping one optional quote —
/// SAMI's `Start=1000` and `Start="1000"` both occur in the wild.
fn extract_digits_attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let lower = tag.to_ascii_lowercase();
    let needle = format!("{name}=");
    let pos = lower.find(&needle)?;
    let after = tag.get(pos.saturating_add(needle.len())..)?;
    let after = after.trim_start_matches(['"', '\'']);
    let end = after
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after.len());
    if end == 0 {
        return None;
    }
    after.get(..end)
}

fn sync_starts(text: &str) -> Vec<usize> {
    text.to_ascii_lowercase()
        .match_indices("<sync")
        .map(|(i, _)| i)
        .collect()
}

fn parse(bytes: &[u8]) -> Vec<Cue> {
    let text = String::from_utf8_lossy(bytes);
    let positions = sync_starts(&text);
    let mut entries: Vec<(Duration, &str)> = Vec::new();
    for (i, &pos) in positions.iter().enumerate() {
        let chunk_end = positions
            .get(i.saturating_add(1))
            .copied()
            .unwrap_or(text.len());
        let Some(chunk) = text.get(pos..chunk_end) else {
            continue;
        };
        let tag_end = chunk.find('>').map_or(chunk.len(), |p| p.saturating_add(1));
        let Some(tag) = chunk.get(..tag_end) else {
            continue;
        };
        let Some(ms) = extract_digits_attr(tag, "start").and_then(|d| d.parse::<i64>().ok()) else {
            continue;
        };
        entries.push((Duration::from_micros(ms.saturating_mul(1000)), chunk));
    }
    let mut cues = Vec::new();
    for (i, &(start, chunk)) in entries.iter().enumerate() {
        let end = entries
            .get(i.saturating_add(1))
            .map_or(start, |&(next, _)| next);
        cues.push(Cue::new(start, end, chunk.as_bytes().to_vec()));
    }
    cues
}

/// Content probe: `<SYNC Start=...>` occurrences with a parseable `Start`.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let hits = parse(data.buf).len();
    if hits > 0 {
        ProbeScore::repeating(hits as u32)
    } else {
        ProbeScore::from_extension(data, &["smi", "sami"])
    }
}

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    engine::open_generic(src, Some(CodecId::Sami), parse)
}

/// The demuxer descriptor. `CodecId::Sami`, matching the reference's own
/// `codec_name=sami`.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "sami",
    long_name: "SAMI subtitle format",
    extensions: &["smi", "sami"],
    mime_types: &["application/x-sami"],
    flags: DEMUX_FLAGS,
    probe,
    open: open_demuxer,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = b"<SAMI><BODY>\n<SYNC Start=1000><P>Hello world\n<SYNC Start=3000><P>&nbsp;\n</BODY></SAMI>\n";

    #[test]
    fn payload_includes_the_sync_and_p_tags() {
        let cues = parse(SAMPLE);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start, Duration::from_micros(1_000_000));
        assert_eq!(cues[0].end, Duration::from_micros(3_000_000));
        assert!(
            cues[0]
                .text_lossy()
                .starts_with("<SYNC Start=1000><P>Hello world")
        );
    }

    #[test]
    fn probe_rejects_plain_prose() {
        assert_eq!(
            probe(&ProbeData::new(b"Nothing resembling a SYNC tag here.\n")),
            ProbeScore::NONE
        );
    }
}
