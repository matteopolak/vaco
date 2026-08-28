//! `ffmetadata`: the reference's own `;FFMETADATA1` metadata interchange
//! text format — **demuxer only**. The muxer already exists, registered as
//! `vaco_mux_stream::MUXER_FFMETADATA`; writing a second one here would
//! collide with it, so this module reads what that one (or the reference)
//! writes.
//!
//! # Grammar
//!
//! `Vaco-Spec-Ref ffmpeg-formats-doc` (the "Metadata" chapter of
//! `ffmpeg-formats.html`) states the shape; the escaping rule and every
//! numeric constant below were independently confirmed against `ffmpeg`/
//! `ffprobe` 8.1 rather than taken on faith:
//!
//! * A line consists of everything up to an **unescaped** newline — a `\`
//!   immediately followed by a real newline byte continues the value onto
//!   the next physical line rather than ending it.
//! * A line whose first byte is `;` or `#` is a comment and is dropped
//!   whole, including the conventional `;FFMETADATA1` line itself: measured,
//!   `;FFMETADATA2`, `;anything` and no header line at all all parse
//!   identically once a caller forces the format. Auto-detection is
//!   stricter — see [`probe`].
//! * `[STREAM]` and `[CHAPTER]` (case-sensitive, exactly those brackets)
//!   open a section; every following line belongs to it until the next
//!   section line or end of file.
//! * A non-comment, non-section line with no unescaped `=` is ignored.
//!   Otherwise it splits at the **first** unescaped `=` into a key and a
//!   value; neither side is trimmed (`title = x` round-trips as key
//!   `"title "`, confirmed).
//! * `=`, `;`, `#`, `\` and a literal newline are escaped with a leading
//!   `\` on write. On read, `\` removes the *next* character's special
//!   meaning unconditionally — including a character with no special
//!   meaning at all, and including end-of-string, where a lone trailing `\`
//!   is simply dropped (measured, matching this project's own
//!   `vaco_core::escape` convention for "trailing backslash is accepted, not
//!   an error" — but reimplemented locally rather than reused, because that
//!   module's grammar also treats `'` as a quote character and ffmetadata's
//!   does not: an apostrophe in a title must stay a literal apostrophe).
//! * Inside `[CHAPTER]`, three keys are consumed as fields rather than
//!   stored as tags: `TIMEBASE=num/den` (defaulting to `1/1000000000`,
//!   nanoseconds, when absent) and `START=`/`END=` (both required to
//!   produce a chapter; `END < START` is rejected, matching the reference's
//!   own refusal to open such a file).
//!
//! # What is not reproduced
//!
//! `[STREAM]` sections are read as per-stream tags but are not surfaced as
//! phantom [`vaco_format_core::Stream`]s the way the reference's own
//! `ffmetadata` demuxer does (it reports each as a zero-information `data`
//! stream at a fixed `1/90000` time base, sized from the *last chapter's end
//! time* — measured, and specific enough to this one demuxer's internal
//! bookkeeping that reproducing it did not seem worth the coupling between
//! chapters and streams it would introduce here). `-map_metadata`/
//! `-map_chapters`, the documented use of this format, need only
//! [`Demuxer::metadata`] and [`Demuxer::chapters`], both of which are exact.

use vaco_core::{Error, Rational, Result, Timestamp};
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Chapter, Demuxer, DemuxerDesc, ParserProvider};
use vaco_io::{IoContext, IoOptions, MediaSource};
use vaco_limits::Budget;
use vaco_packet::Packet;

const HEADER_PREFIX: &[u8] = b";FFMETADATA";

/// A generous but finite cap on the whole file: this format has no per-field
/// length prefix at all, so the only bound available is "how big a text
/// metadata sidecar could plausibly be".
const MAX_FILE: usize = 64 << 20;

const FLAGS: FormatFlags = FormatFlags::NOSTREAMS.union(FormatFlags::NOTIMESTAMPS);

#[must_use]
pub fn probe(data: &ProbeData<'_>) -> ProbeScore {
    if data.starts_with(HEADER_PREFIX) {
        ProbeScore::MAX
    } else {
        ProbeScore::NONE
    }
}

pub const DEMUXER: DemuxerDesc = DemuxerDesc {
    name: "ffmetadata",
    long_name: "FFmpeg metadata in text",
    extensions: &["ffmeta"],
    mime_types: &[],
    flags: FLAGS,
    probe,
    open: open_demuxer,
};

fn open_demuxer(
    src: Box<dyn MediaSource>,
    _parsers: &dyn ParserProvider,
) -> Result<Box<dyn Demuxer>> {
    Ok(Box::new(FfmetadataDemuxer::open(src)?))
}

/// Reverse one level of escaping: `\` removes the next character's special
/// meaning, including a real newline (rejoining a continuation) and
/// including a character that had no special meaning to begin with. A
/// trailing `\` with nothing after it is dropped. See the module docs for
/// why this is not [`vaco_core::escape::unescape`].
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Split `text` into logical lines: everything up to an unescaped `\n`, with
/// the escape sequence that protects a `\n` (`\` immediately followed by a
/// real newline) left in place rather than treated as a boundary.
fn logical_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut escaped = false;
    let mut last = 0usize;
    for (i, c) in text.char_indices() {
        last = i + c.len_utf8();
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '\n' => {
                lines.push(text.get(start..i).unwrap_or_default());
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < last || lines.is_empty() {
        lines.push(text.get(start..).unwrap_or_default());
    }
    lines
}

/// Split a non-comment, non-section line at the first unescaped `=`.
fn split_key_value(line: &str) -> Option<(&str, &str)> {
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '=' => return Some((line.get(..i)?, line.get(i + 1..)?)),
            _ => {}
        }
    }
    None
}

/// A `[NAME]` section header, or `None` if `line` is not one.
fn section_name(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('[')?;
    rest.strip_suffix(']')
}

#[derive(Default)]
struct Parsed {
    tags: Vec<(String, String)>,
    chapters: Vec<Chapter>,
    stream_tags: Vec<Vec<(String, String)>>,
}

enum Section {
    Global,
    Stream,
    Chapter {
        time_base: Rational,
        start: Option<i64>,
        end: Option<i64>,
    },
}

/// # Errors
/// [`Error::InvalidData`] when a chapter's `END` precedes its `START`,
/// matching the reference's own refusal to open such a file.
fn parse(text: &str) -> Result<Parsed> {
    let mut out = Parsed::default();
    let mut section = Section::Global;
    let mut chapter_tags: Vec<(String, String)> = Vec::new();

    for raw in logical_lines(text) {
        if raw.is_empty() || raw.starts_with(';') || raw.starts_with('#') {
            continue;
        }
        if let Some(name) = section_name(raw) {
            finish_chapter(&mut section, &mut chapter_tags, &mut out)?;
            section = match name {
                "STREAM" => {
                    out.stream_tags.push(Vec::new());
                    Section::Stream
                }
                "CHAPTER" => Section::Chapter {
                    time_base: Rational::new(1, 1_000_000_000),
                    start: None,
                    end: None,
                },
                _ => Section::Global,
            };
            continue;
        }
        let Some((key, value)) = split_key_value(raw) else {
            continue;
        };
        let value = unescape(value);
        match &mut section {
            Section::Global => out.tags.push((unescape(key), value)),
            Section::Stream => {
                if let Some(tags) = out.stream_tags.last_mut() {
                    tags.push((unescape(key), value));
                }
            }
            Section::Chapter {
                time_base,
                start,
                end,
            } => match key {
                "TIMEBASE" => {
                    if let Some((n, d)) = value.split_once('/')
                        && let (Ok(n), Ok(d)) = (n.parse::<i32>(), d.parse::<i32>())
                        && d != 0
                    {
                        *time_base = Rational::new(n, d);
                    }
                }
                "START" => *start = value.parse::<i64>().ok(),
                "END" => *end = value.parse::<i64>().ok(),
                _ => chapter_tags.push((unescape(key), value)),
            },
        }
    }
    finish_chapter(&mut section, &mut chapter_tags, &mut out)?;
    Ok(out)
}

fn finish_chapter(
    section: &mut Section,
    chapter_tags: &mut Vec<(String, String)>,
    out: &mut Parsed,
) -> Result<()> {
    if let Section::Chapter {
        time_base,
        start,
        end,
    } = section
    {
        if let (Some(start), Some(end)) = (*start, *end) {
            if end < start {
                return Err(Error::InvalidData(
                    "ffmetadata: chapter end time precedes its start",
                ));
            }
            out.chapters.push(Chapter {
                id: i64::try_from(out.chapters.len()).unwrap_or(i64::MAX),
                time_base: *time_base,
                start: Timestamp::new(start),
                end: Timestamp::new(end),
                metadata: std::mem::take(chapter_tags),
            });
        }
        chapter_tags.clear();
    }
    Ok(())
}

#[derive(Debug)]
pub struct FfmetadataDemuxer {
    tags: Vec<(String, String)>,
    chapters: Vec<Chapter>,
}

impl FfmetadataDemuxer {
    /// # Errors
    /// [`Error::InvalidData`] for a chapter whose `END` precedes its
    /// `START`, or a file larger than this demuxer accepts.
    pub fn open(src: Box<dyn MediaSource>) -> Result<Self> {
        let mut io = IoContext::new(src, &IoOptions::default())?;
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let declared = io
            .size()
            .and_then(|s| usize::try_from(s).ok())
            .unwrap_or(64 * 1024)
            .min(MAX_FILE);
        let mut buf = budget.incremental::<u8>(declared);
        let mut chunk = [0u8; 8192];
        loop {
            let n = io.read_partial(&mut chunk)?;
            if n == 0 {
                break;
            }
            buf.push_slice(&mut budget, chunk.get(..n).unwrap_or(&[]))?;
            if buf.as_slice().len() > MAX_FILE {
                return Err(Error::InvalidData("ffmetadata: file too large"));
            }
        }
        let text = String::from_utf8_lossy(buf.as_slice()).into_owned();
        let parsed = parse(&text)?;
        Ok(Self {
            tags: parsed.tags,
            chapters: parsed.chapters,
        })
    }
}

impl Demuxer for FfmetadataDemuxer {
    fn streams(&self) -> &[vaco_format_core::Stream] {
        &[]
    }

    fn chapters(&self) -> &[Chapter] {
        &self.chapters
    }

    fn metadata(&self) -> &[(String, String)] {
        &self.tags
    }

    fn read_packet(&mut self) -> Result<Packet> {
        Err(Error::Eof)
    }

    fn seek(&mut self, _target: SeekTarget, _flags: SeekFlags) -> Result<()> {
        Err(Error::NotSeekable)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::MemorySource;

    fn open(text: &str) -> FfmetadataDemuxer {
        FfmetadataDemuxer::open(Box::new(MemorySource::new(text.as_bytes().to_vec()))).unwrap()
    }

    #[test]
    fn probe_needs_the_literal_prefix() {
        assert_eq!(probe(&ProbeData::new(b";FFMETADATA1\n")), ProbeScore::MAX);
        assert_eq!(probe(&ProbeData::new(b";FFMETADATA")), ProbeScore::MAX);
        assert_eq!(probe(&ProbeData::new(b";FFMETADAT")), ProbeScore::NONE);
        assert_eq!(probe(&ProbeData::new(b";ffmetadata1\n")), ProbeScore::NONE);
        assert_eq!(probe(&ProbeData::new(b"title=x\n")), ProbeScore::NONE);
    }

    #[test]
    fn header_line_is_a_comment_not_a_requirement() {
        let d = open("title=NoHeader\n");
        assert_eq!(d.metadata(), &[("title".to_owned(), "NoHeader".to_owned())]);
        let d = open(";FFMETADATA2\ntitle=Two\n");
        assert_eq!(d.metadata(), &[("title".to_owned(), "Two".to_owned())]);
    }

    #[test]
    fn global_tags_and_comments() {
        let d = open(";FFMETADATA1\ntitle=A Title\n;a comment\n#also a comment\nartist=Someone\n");
        assert_eq!(
            d.metadata(),
            &[
                ("title".to_owned(), "A Title".to_owned()),
                ("artist".to_owned(), "Someone".to_owned()),
            ]
        );
    }

    #[test]
    fn escaping_matches_the_five_special_characters() {
        let d = open("title=bike\\\\shed\ncomment=a\\=b\\;c\\#d\n");
        assert_eq!(
            d.metadata(),
            &[
                ("title".to_owned(), "bike\\shed".to_owned()),
                ("comment".to_owned(), "a=b;c#d".to_owned()),
            ]
        );
    }

    #[test]
    fn escaped_newline_continues_the_value() {
        let d = open("comment=multi\\\nline\n");
        assert_eq!(
            d.metadata(),
            &[("comment".to_owned(), "multi\nline".to_owned())]
        );
    }

    #[test]
    fn whitespace_around_equals_is_part_of_the_tag() {
        let d = open("foo = bar\n");
        assert_eq!(d.metadata(), &[("foo ".to_owned(), " bar".to_owned())]);
    }

    #[test]
    fn chapter_section_with_timebase() {
        let d = open(concat!(
            ";FFMETADATA1\n",
            "[CHAPTER]\n",
            "TIMEBASE=1/1000\n",
            "START=0\n",
            "END=60000\n",
            "title=chapter #1\n",
        ));
        assert_eq!(d.chapters().len(), 1);
        let c = d.chapters().first().unwrap();
        assert_eq!(c.time_base, Rational::new(1, 1000));
        assert_eq!(c.start, Timestamp::new(0));
        assert_eq!(c.end, Timestamp::new(60_000));
        assert_eq!(c.metadata, &[("title".to_owned(), "chapter #1".to_owned())]);
    }

    #[test]
    fn chapter_without_timebase_defaults_to_nanoseconds() {
        let d = open("[CHAPTER]\nSTART=0\nEND=1000000000\n");
        assert_eq!(
            d.chapters().first().unwrap().time_base,
            Rational::new(1, 1_000_000_000)
        );
    }

    #[test]
    fn chapter_end_before_start_is_rejected() {
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let _ = &mut budget;
        assert!(
            FfmetadataDemuxer::open(Box::new(MemorySource::new(
                b"[CHAPTER]\nSTART=100\nEND=50\n".to_vec()
            )))
            .is_err()
        );
    }

    #[test]
    fn stream_section_tags_are_indexed_in_order() {
        let d = open("[STREAM]\ntitle=S0\n[STREAM]\ntitle=S1\nlanguage=eng\n");
        // Not exposed as phantom streams (module docs); reachable only via a
        // future `Demuxer` extension point if one is ever added. For now this
        // test documents the parser's internal shape rather than public API.
        let parsed = parse("[STREAM]\ntitle=S0\n[STREAM]\ntitle=S1\nlanguage=eng\n").unwrap();
        assert_eq!(
            parsed.stream_tags,
            vec![
                vec![("title".to_owned(), "S0".to_owned())],
                vec![
                    ("title".to_owned(), "S1".to_owned()),
                    ("language".to_owned(), "eng".to_owned())
                ],
            ]
        );
        let _ = d;
    }

    #[test]
    fn round_trip_property_holds_for_the_five_special_characters() {
        // Not a full proptest (the escape/unescape pair here has no `escape`
        // half — writing is `vaco_mux_stream`'s job) but a fixed check that
        // every special character survives the read side unescaped exactly
        // once, which is the property a proptest would otherwise assert.
        for raw in ["a=b", "a;b", "a#b", "a\\b", "plain"] {
            let mut escaped = String::new();
            for c in raw.chars() {
                if "=;#\\".contains(c) {
                    escaped.push('\\');
                }
                escaped.push(c);
            }
            assert_eq!(unescape(&escaped), raw);
        }
    }
}
