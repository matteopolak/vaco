//! Spruce subtitle format (`.stl`) — demux only.
//!
//! `HH:MM:SS:hh,HH:MM:SS:hh,text` per line. Measured (D17): despite the
//! fourth field's resemblance to an editing timecode's frame slot, it counts
//! **hundredths of a second** at every frame rate — see
//! [`vaco_format_subtitle::time::parse_stl_time`]. Not to be confused with
//! the EBU/binary STL format of the same common abbreviation; this is the
//! text one the reference's `stl` demuxer reads.

use vaco_codec_core::CodecId;
use vaco_core::Result;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{Demuxer, DemuxerDesc, ParserProvider};
use vaco_format_subtitle::Cue;
use vaco_format_subtitle::time::parse_stl_time;
use vaco_io::MediaSource;

use crate::engine::{self, DEMUX_FLAGS};

fn parse_line(line: &str) -> Option<Cue> {
    let parts: Vec<&str> = line.splitn(3, ',').collect();
    match parts.as_slice() {
        [a, b, text] => {
            let start = parse_stl_time(a.trim())?;
            let end = parse_stl_time(b.trim())?;
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

/// Content probe: lines matching `HH:MM:SS:hh,HH:MM:SS:hh,text`.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let text = String::from_utf8_lossy(data.buf);
    let hits = text.lines().filter(|l| parse_line(l).is_some()).count();
    if hits > 0 {
        ProbeScore::repeating(hits as u32)
    } else {
        ProbeScore::from_extension(data, &["stl"])
    }
}

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    engine::open_generic(src, Some(CodecId::Stl), parse)
}

/// The demuxer descriptor. `CodecId::Stl`, matching the reference's own
/// `codec_name=stl`.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "stl",
    long_name: "Spruce subtitle format",
    extensions: &["stl"],
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
    fn fourth_field_is_hundredths_not_frames() {
        let cues = parse(b"00:00:01:12,00:00:03:00,Hello world\n");
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start, Duration::from_micros(1_120_000));
        assert_eq!(cues[0].end, Duration::from_micros(3_000_000));
    }

    #[test]
    fn probe_rejects_plain_prose() {
        assert_eq!(
            probe(&ProbeData::new(
                b"Some ordinary text, with a comma, in it.\n"
            )),
            ProbeScore::NONE
        );
    }
}
