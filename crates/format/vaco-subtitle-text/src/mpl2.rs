//! MPL2 (`.txt`) — demux only.
//!
//! `[start][end]text` per line. Measured (D17): the numbers are **tenths of a
//! second**, not frames — see
//! [`vaco_format_subtitle::time::parse_deciseconds`], shared with
//! [`crate::pjs`] because the two formats genuinely agree on this one unit.

use vaco_codec_core::CodecId;
use vaco_core::Result;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{Demuxer, DemuxerDesc, ParserProvider};
use vaco_format_subtitle::Cue;
use vaco_format_subtitle::time::parse_deciseconds;
use vaco_io::MediaSource;

use crate::engine::{self, DEMUX_FLAGS};

fn parse_line(line: &str) -> Option<Cue> {
    let rest = line.strip_prefix('[')?;
    let (a, rest) = rest.split_once(']')?;
    let rest = rest.strip_prefix('[')?;
    let (b, text) = rest.split_once(']')?;
    let start = parse_deciseconds(a.trim())?;
    let end = parse_deciseconds(b.trim())?;
    Some(Cue::new(start, end, text.as_bytes().to_vec()))
}

fn parse(bytes: &[u8]) -> Vec<Cue> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter_map(parse_line)
        .collect()
}

/// Content probe: lines matching `[n][n]text`.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let text = String::from_utf8_lossy(data.buf);
    let hits = text.lines().filter(|l| parse_line(l).is_some()).count();
    if hits > 0 {
        ProbeScore::repeating(hits as u32)
    } else {
        ProbeScore::from_extension(data, &["mpl2", "txt"])
    }
}

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    engine::open_generic(src, Some(CodecId::Mpl2), parse)
}

/// The demuxer descriptor. `CodecId::Mpl2`, matching the reference's own
/// `codec_name=mpl2`.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "mpl2",
    long_name: "MPL2 subtitles",
    extensions: &["mpl2", "txt"],
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
    fn tenths_of_a_second_matches_measurement() {
        let cues = parse(b"[10][50]Hello world\n");
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start, Duration::from_micros(1_000_000));
        assert_eq!(cues[0].end, Duration::from_micros(5_000_000));
    }

    #[test]
    fn probe_rejects_plain_prose() {
        assert_eq!(
            probe(&ProbeData::new(b"No brackets in this line of prose.\n")),
            ProbeScore::NONE
        );
    }

    #[test]
    fn probe_rejects_microdvd_frame_braces() {
        // Same shape, different delimiter and unit; must not cross-claim.
        let data = ProbeData::new(b"{0}{25}Hello world\n");
        assert_eq!(probe(&data), ProbeScore::NONE);
    }
}
