//! `RealText` (`.rt`) — demux only.
//!
//! `<time begin="HH:MM:SS" .../>text` repeating. Like [`crate::sami`], the
//! packet payload includes the `<time .../>` tag itself, measured against
//! the reference the same way.
//!
//! Two measured facts that are easy to get wrong by assuming instead of
//! checking (D17):
//!
//! * A `<time>` tag with neither an `end=` nor a `dur=` attribute gets a
//!   **60-second default duration**
//!   ([`vaco_format_subtitle::time::REALTEXT_DEFAULT_DURATION`]) — it does
//!   *not* borrow the next cue's `begin`, unlike every other start-only
//!   format in this crate ([`crate::sami`], [`crate::lrc`],
//!   [`crate::vplayer`], [`crate::subviewer1`]).
//! * Content before the file's first `<time>` tag (typically just the
//!   `<window .../>` line) is dropped rather than emitted as a leading
//!   zero-duration cue, which is a simplification from what the reference
//!   does with such a preamble — noted here rather than silently matched.

use vaco_codec_core::CodecId;
use vaco_core::{Duration, Result};
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::{Demuxer, DemuxerDesc, ParserProvider};
use vaco_format_subtitle::Cue;
use vaco_format_subtitle::time::{REALTEXT_DEFAULT_DURATION, parse_realtext_time};
use vaco_io::MediaSource;

use crate::engine::{self, DEMUX_FLAGS};

/// The quoted value of `name="..."` in `tag`, case-insensitive on the name.
fn extract_quoted_attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let lower = tag.to_ascii_lowercase();
    let needle = format!("{name}=");
    let pos = lower.find(&needle)?;
    let after = tag.get(pos.saturating_add(needle.len())..)?;
    let quote = after.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = after.get(quote.len_utf8()..)?;
    let end = rest.find(quote)?;
    rest.get(..end)
}

fn time_starts(text: &str) -> Vec<usize> {
    text.to_ascii_lowercase()
        .match_indices("<time")
        .map(|(i, _)| i)
        .collect()
}

fn parse(bytes: &[u8]) -> Vec<Cue> {
    let text = String::from_utf8_lossy(bytes);
    let positions = time_starts(&text);
    let mut cues = Vec::new();
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
        let Some(begin) = extract_quoted_attr(tag, "begin").and_then(parse_realtext_time) else {
            continue;
        };
        let end = extract_quoted_attr(tag, "end")
            .and_then(parse_realtext_time)
            .or_else(|| {
                extract_quoted_attr(tag, "dur")
                    .and_then(parse_realtext_time)
                    .map(|dur| {
                        Duration::from_micros(begin.as_micros().saturating_add(dur.as_micros()))
                    })
            })
            .unwrap_or_else(|| {
                Duration::from_micros(
                    begin
                        .as_micros()
                        .saturating_add(REALTEXT_DEFAULT_DURATION.as_micros()),
                )
            });
        cues.push(Cue::new(begin, end, chunk.as_bytes().to_vec()));
    }
    cues
}

/// Content probe: `<time begin="...">` occurrences with a parseable `begin`.
#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    let hits = parse(data.buf).len();
    if hits > 0 {
        ProbeScore::repeating(hits as u32)
    } else {
        ProbeScore::from_extension(data, &["rt"])
    }
}

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    engine::open_generic(src, Some(CodecId::Realtext), parse)
}

/// The demuxer descriptor. `CodecId::Realtext`, matching the reference's own
/// `codec_name=realtext`.
pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "realtext",
    long_name: "RealText subtitle format",
    extensions: &["rt"],
    mime_types: &["application/x-rt"],
    flags: DEMUX_FLAGS,
    probe,
    open: open_demuxer,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn default_duration_is_sixty_seconds_when_no_end_or_dur() {
        let cues = parse(b"<window/>\n<time begin=\"00:00:01\"/>Hello world\n");
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start, Duration::from_micros(1_000_000));
        assert_eq!(cues[0].end, Duration::from_micros(61_000_000));
    }

    #[test]
    fn explicit_end_overrides_the_default() {
        let cues = parse(b"<time begin=\"00:00:01\" end=\"00:00:02\"/>Hi\n");
        assert_eq!(cues[0].end, Duration::from_micros(2_000_000));
    }

    #[test]
    fn dur_attribute_is_added_to_begin() {
        let cues = parse(b"<time begin=\"00:00:01\" dur=\"00:00:02\"/>Hi\n");
        assert_eq!(cues[0].end, Duration::from_micros(3_000_000));
    }

    #[test]
    fn probe_rejects_plain_prose() {
        assert_eq!(
            probe(&ProbeData::new(b"No time tags anywhere in this text.\n")),
            ProbeScore::NONE
        );
    }
}
