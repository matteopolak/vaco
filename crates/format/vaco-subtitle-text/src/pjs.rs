//! PJS (`.pjs`) — demux only.
//!
//! `start,end,"text"` per line. Measured (D17): the numbers are tenths of a
//! second, the same unit [`crate::mpl2`] uses — see
//! [`vaco_format_subtitle::time::parse_deciseconds`]. The text field is
//! quoted; the quotes are stripped, not otherwise interpreted (no escape
//! handling — no sample of the format needed one).

use vaco_codec_core::CodecId;
use vaco_core::Result;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{Demuxer, DemuxerDesc, ParserProvider};
use vaco_format_subtitle::Cue;
use vaco_format_subtitle::time::parse_deciseconds;
use vaco_io::MediaSource;

use crate::engine::{self, DEMUX_FLAGS};

fn parse_line(line: &str) -> Option<Cue> {
    let parts: Vec<&str> = line.trim().splitn(3, ',').collect();
    match parts.as_slice() {
        [a, b, rest] => {
            let start = parse_deciseconds(a.trim())?;
            let end = parse_deciseconds(b.trim())?;
            let text = rest.trim();
            let text = text.strip_prefix('"').unwrap_or(text);
            let text = text.strip_suffix('"').unwrap_or(text);
            Some(Cue::new(start, end, text.as_bytes().to_vec()))
        }
        _ => None,
    }
}

fn parse(bytes: &[u8]) -> Vec<Cue> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter_map(parse_line)
        .collect()
}

/// Content probe: lines matching `n,n,"text"`.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let text = String::from_utf8_lossy(data.buf);
    let hits = text.lines().filter(|l| parse_line(l).is_some()).count();
    if hits > 0 {
        ProbeScore::repeating(hits as u32)
    } else {
        ProbeScore::from_extension(data, &["pjs"])
    }
}

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    engine::open_generic(src, Some(CodecId::Pjs), parse)
}

/// The demuxer descriptor. `CodecId::Pjs`, matching the reference's own
/// `codec_name=pjs`.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "pjs",
    long_name: "PJS (Phoenix Japanimation Society) subtitles",
    extensions: &["pjs"],
    mime_types: &[],
    flags: DEMUX_FLAGS,
    probe,
    open: open_demuxer,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_core::Duration;

    #[test]
    fn tenths_of_a_second_and_quote_stripping() {
        let cues = parse(b"10,50,\"Hello world\"\n");
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start, Duration::from_micros(1_000_000));
        assert_eq!(cues[0].end, Duration::from_micros(5_000_000));
        assert_eq!(cues[0].text, b"Hello world");
    }

    #[test]
    fn probe_rejects_plain_prose() {
        assert_eq!(
            probe(&ProbeData::new(
                b"Just a line of English prose, nothing more.\n"
            )),
            ProbeScore::NONE
        );
    }
}
