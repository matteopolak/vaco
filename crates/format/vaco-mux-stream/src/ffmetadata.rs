//! `ffmetadata`: the `;FFMETADATA1` key/value text format.
//!
//! # Measured against the reference (ffmpeg 8.1, `LC_ALL=C`)
//!
//! `ffmpeg -f lavfi -i testsrc=r=25:d=1 -metadata title="A Title" -metadata
//! comment=$'multi\nline' -f ffmetadata -`, byte-inspected with `od -c`:
//!
//! ```text
//! ;FFMETADATA1
//! title=A Title
//! comment=multi\
//! line
//! encoder=Lavf62.12.100
//! ```
//!
//! # Escaping
//!
//! `=`, `;`, `#`, `\` and a literal newline are backslash-escaped in a value
//! on the way out (`foo=a\=b\;c\#d\\e` for the value `a=b;c#d\e`, measured).
//! An embedded newline is escaped as a bare `\` immediately followed by a
//! **real** newline byte — not the two-character sequence `\` `n` — so the
//! value continues visually onto the next physical line and a naive
//! line-oriented diff of the dump does not show it as one line. Reading it
//! back: `\` followed by *any* character unescapes to that character
//! literally, including a real newline (rejoining the continuation) and
//! including characters with no escaping meaning at all — measured, `a\nb`
//! (backslash then the literal letter `n`) reads back as `anb`, and a
//! trailing backslash with nothing after it is simply dropped. So the reader
//! is the generic "backslash removes the next character's special meaning"
//! rule, not a fixed table of five escapes; [`unescape`] implements exactly
//! that, and [`escape`] only ever emits backslash before the five characters
//! that need it, so `unescape(escape(s)) == s` for every `s`
//! ([`tests::round_trips`]).
//!
//! # Grammar (measured by round-tripping through `ffmpeg -f ffmetadata`)
//!
//! * A line whose first character is `;` or `#` is a comment, in full —
//!   including the conventional `;FFMETADATA1` header line itself, which
//!   measured out to be nothing more than a comment: `;FFMETADATA2`,
//!   `;anything`, and no header line at all all parse identically. This
//!   crate still always **writes** `;FFMETADATA1` first, matching the
//!   reference's own output, but does not require or validate it on read.
//! * A line with no unescaped `=` is ignored — not an error, and not a
//!   key with an empty value. (`notakeyvalue` on its own line vanishes.)
//! * `key=value`: split at the **first** unescaped `=`. Neither side is
//!   trimmed — `title = spaced` round-trips as key `"title "`, value
//!   `" spaced"`, confirmed byte-for-byte.
//! * `[CHAPTER]` and `[STREAM]` open a section; every following `key=value`
//!   line belongs to that section (including keys with no special meaning —
//!   they still land in the section's own metadata) until the next `[...]`
//!   line or end of input. A section line with no preceding global lines is
//!   fine; a `[STREAM]`/`[CHAPTER]` section for an index the writer does not
//!   have is simply never reached in this crate's writer, and on read this
//!   parser reports what a script names without validating a
//!   `TIMEBASE`/`START`/`END` triple is complete — the writer always emits
//!   the triple together, but a hand-edited script that omits one is
//!   [`ParsedChapter`]'s caller's problem, matching the general permissive
//!   style [`vaco_core::escape`] already documents (an unterminated quote or
//!   a trailing backslash are accepted, not rejected).
//!
//! Section ordering measured, chapters and per-stream tags both present:
//! global lines, **then every `[STREAM]` block in stream order, then every
//! `[CHAPTER]` block in chapter order**.
//!
//! # The auto `encoder=` tag
//!
//! The reference always appends its own `encoder=Lavf<version>` as the last
//! global line, even overwriting a user-supplied `-metadata encoder=...`
//! (measured: the user's value is discarded, not merged). This crate mirrors
//! that shape with its own identity rather than impersonating a build of the
//! reference — see [`vaco-mux-hash`](../vaco_mux_hash/index.html)'s
//! `SOFTWARE_LINE` for the same decision made the same way. [`ENCODER_TAG`]
//! is appended (removing any caller-supplied `encoder` key first) by
//! [`write`], never by the parser.

use vaco_core::escape::{self, Mode};

/// The literal header line this crate writes. Not validated on read — see
/// the module docs.
pub const HEADER_LINE: &str = ";FFMETADATA1";

/// This crate's own `encoder=` identity, mirroring `vaco-mux-hash`'s
/// `SOFTWARE_LINE` decision: claiming to be a `Lavf<version>` build of the
/// reference would be actively misleading.
pub const ENCODER_TAG: &str = "vaco";

/// Characters escaped in a key or value: `=`, `;`, `#`. `\` and `'` are
/// always escaped by [`vaco_core::escape::escape`] itself; a literal newline
/// is handled separately (see [`escape_value`]) because it is not a
/// single-character replacement.
const SPECIAL: &str = "=;#";

/// Escape one key or value for the flat (non-newline) case.
#[must_use]
fn escape_flat(s: &str) -> String {
    escape::escape(s, SPECIAL, Mode::Backslash)
}

/// Escape a value, additionally turning every literal `\n` into `\` followed
/// by a real newline — the continuation form the reference writes.
#[must_use]
pub fn escape_value(s: &str) -> String {
    if !s.contains('\n') {
        return escape_flat(s);
    }
    s.split('\n')
        .map(escape_flat)
        .collect::<Vec<_>>()
        .join("\\\n")
}

/// Reverse [`escape_value`] (and [`escape_flat`]): backslash drops and keeps
/// the next character literally, whatever it is; anything else is literal.
/// Matches the reference's measured behaviour exactly (`a\nb` -> `anb`, a
/// trailing lone `\` is dropped) — deliberately not
/// [`vaco_core::escape::unescape`], which also gives `'` quoting meaning that
/// ffmetadata's grammar never showed under probing (a bare `'` in a value
/// round-tripped unescaped in every probe).
#[must_use]
pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
            // else: trailing backslash, dropped.
        } else {
            out.push(c);
        }
    }
    out
}

/// One `key=value` line's parts, still following the "first unescaped `=`"
/// rule but with **no quote handling** — see [`unescape`]'s docs for why this
/// crate does not reuse [`vaco_core::escape::split_once_raw`] directly.
fn split_once_unescaped(s: &str, sep: char) -> Option<(String, String)> {
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        if c == '\\' {
            chars.next();
            continue;
        }
        if c == sep {
            let key = s.get(..i)?;
            let value = s.get(i + 1..)?;
            return Some((unescape(key), unescape(value)));
        }
    }
    None
}

/// A parsed chapter section (`[CHAPTER]`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParsedChapter {
    /// `TIMEBASE=n/d`, unparsed (a caller wanting a [`vaco_core::Rational`]
    /// parses this key itself; this crate does not assume it is present).
    pub metadata: Vec<(String, String)>,
}

/// A parsed stream section (`[STREAM]`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParsedStream {
    pub metadata: Vec<(String, String)>,
}

/// The result of [`parse`]: global metadata plus every section, in file order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParsedMetadata {
    pub global: Vec<(String, String)>,
    pub streams: Vec<ParsedStream>,
    pub chapters: Vec<ParsedChapter>,
}

enum Section {
    Global,
    Stream(usize),
    Chapter(usize),
}

/// Parse an `;FFMETADATA1`-style document.
///
/// Never fails: every line the grammar does not recognise (a comment, a
/// blank line, a bare line with no `=`) is silently skipped, matching the
/// reference's own measured tolerance for exactly those inputs. This is the
/// function the fuzz target
/// (`fuzz/fuzz_targets/vaco_mux_stream_ffmetadata_reader.rs`) drives directly
/// against arbitrary bytes.
#[must_use]
pub fn parse(input: &str) -> ParsedMetadata {
    let mut out = ParsedMetadata::default();
    let mut section = Section::Global;
    for line in split_lines(input) {
        let line = line.as_str();
        let first = line.chars().next();
        if first == Some(';') || first == Some('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            section = match rest {
                "STREAM" => {
                    out.streams.push(ParsedStream::default());
                    Section::Stream(out.streams.len() - 1)
                }
                "CHAPTER" => {
                    out.chapters.push(ParsedChapter::default());
                    Section::Chapter(out.chapters.len() - 1)
                }
                _ => Section::Global,
            };
            continue;
        }
        let Some((key, value)) = split_once_unescaped(line, '=') else {
            continue;
        };
        match &section {
            Section::Global => out.global.push((key, value)),
            Section::Stream(i) => {
                if let Some(s) = out.streams.get_mut(*i) {
                    s.metadata.push((key, value));
                }
            }
            Section::Chapter(i) => {
                if let Some(c) = out.chapters.get_mut(*i) {
                    c.metadata.push((key, value));
                }
            }
        }
    }
    out
}

/// Split `input` into logical lines, rejoining a `\` + real-newline
/// continuation into the line it continues (so [`parse`] sees one physical
/// pass and [`split_once_unescaped`] can find the `=` that started the
/// value).
///
/// Deliberately **not** stripping a trailing `\r`: an earlier version did,
/// to be forgiving of a CRLF-terminated script, and a proptest round-trip
/// caught the cost — a value that is itself the single byte `\r` (nothing
/// says one cannot be) would be silently eaten on the way back in, since
/// there would be no way to tell "trailing `\r` is line-ending noise" from
/// "trailing `\r` is the whole value." [`escape_value`]/[`unescape`] already
/// carry any `\r` a value contains without needing a special case.
fn split_lines(input: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cont = false;
    for raw in input.split('\n') {
        if cont && let Some(last) = out.last_mut() {
            last.push('\n');
            last.push_str(raw);
        } else {
            out.push(raw.to_owned());
        }
        // A continuation is an odd number of trailing backslashes: each pair
        // escapes itself, so only a genuinely unpaired final `\` continues.
        let trailing = out
            .last()
            .map_or(0, |l| l.chars().rev().take_while(|&c| c == '\\').count());
        cont = !trailing.is_multiple_of(2);
    }
    out
}

/// One chapter to write, mirroring the reference's `[CHAPTER]` fields.
#[derive(Debug, Clone, PartialEq)]
pub struct ChapterMeta {
    /// `TIMEBASE=<num>/<den>`.
    pub time_base: (i32, i32),
    pub start: i64,
    pub end: i64,
    /// Extra keys, `title` included if present — order preserved.
    pub metadata: Vec<(String, String)>,
}

/// Render one `;FFMETADATA1` document.
///
/// `global`, `streams` (one `[STREAM]` block per entry, in order) and
/// `chapters` (one `[CHAPTER]` block per entry, in order) — matching the
/// measured section order (global, then every stream, then every chapter).
/// [`ENCODER_TAG`] is appended to `global` last, after removing any
/// caller-supplied `encoder` key, matching the reference's overwrite
/// behaviour.
#[must_use]
pub fn write(
    global: &[(String, String)],
    streams: &[Vec<(String, String)>],
    chapters: &[ChapterMeta],
) -> String {
    let mut out = String::new();
    out.push_str(HEADER_LINE);
    out.push('\n');
    for (k, v) in global {
        if k == "encoder" {
            continue;
        }
        write_kv(&mut out, k, v);
    }
    write_kv(&mut out, "encoder", ENCODER_TAG);
    for stream in streams {
        out.push_str("[STREAM]\n");
        for (k, v) in stream {
            write_kv(&mut out, k, v);
        }
    }
    for chapter in chapters {
        out.push_str("[CHAPTER]\n");
        let _ = core::fmt::Write::write_fmt(
            &mut out,
            format_args!("TIMEBASE={}/{}\n", chapter.time_base.0, chapter.time_base.1),
        );
        let _ = core::fmt::Write::write_fmt(&mut out, format_args!("START={}\n", chapter.start));
        let _ = core::fmt::Write::write_fmt(&mut out, format_args!("END={}\n", chapter.end));
        for (k, v) in &chapter.metadata {
            write_kv(&mut out, k, v);
        }
    }
    out
}

fn write_kv(out: &mut String, key: &str, value: &str) {
    // Both sides go through the newline-aware escaper: the reference was
    // only measured writing a raw newline in a *value*, but a key produced
    // by `parse` can legitimately contain one too (a continued line whose
    // `=` falls after the continuation) — see the fuzz target that found
    // this. Using [`escape_flat`] for the key would silently corrupt the
    // line structure the one time a key actually needs it.
    let escaped_key = escape_value(key);
    // A key that, once escaped, still starts with a bare `[` is
    // indistinguishable on read from a `[SECTION]` header line — `parse`
    // checks that *before* looking for `=`, so `[foo=bar` would silently
    // become an (unrecognised) section instead of a global pair. Also found
    // by the fuzz target: escaping just this one leading character removes
    // the ambiguity (`\[` cannot open a section) without touching every `[`
    // a value might contain, since only the key's first byte ever reaches
    // that check.
    if let Some(rest) = escaped_key.strip_prefix('[') {
        out.push('\\');
        out.push('[');
        out.push_str(rest);
    } else {
        out.push_str(&escaped_key);
    }
    out.push('=');
    out.push_str(&escape_value(value));
    out.push('\n');
}

// ------------------------------------------------------------ the registration

use vaco_codec_core::CodecParameters;
use vaco_core::Rational;
use vaco_format_core::flags::FormatFlags;
use vaco_format_core::{Muxer, MuxerDesc};
use vaco_io::{IoOptions, IoWriter, MediaSink};
use vaco_packet::Packet;

/// The `Muxer`-trait-only path, which the module docs' "Configuration" gap
/// applies to in full.
///
/// # Why this writes only the header and the auto `encoder=` tag
///
/// [`vaco_format_core::Muxer`] has no channel for file-level metadata,
/// per-stream metadata or chapters — `add_stream` takes only
/// [`CodecParameters`], and nothing else in the trait carries a tag list or a
/// chapter table. `vaco-mux-matroska`'s `MatroskaMuxer` documents the
/// identical gap for `Tags`/`Chapters`/`Attachments` (see its `mux.rs` module
/// docs) — this is not a shortfall specific to this crate, it is what every
/// muxer in the workspace gets from this frozen trait today. So
/// [`FfmetadataMuxer`], driven only through `dyn Muxer`, always produces
/// exactly `;FFMETADATA1\nencoder=vaco\n` and nothing else, regardless of
/// how many streams are added or what packets arrive — there is nothing
/// upstream of it to report. A caller that actually has metadata, per-stream
/// tags or chapters to write calls [`write`] directly and hands the bytes to
/// its own sink; that is the real, useful entry point this module provides,
/// and it is what this module's own tests exercise.
#[derive(Debug)]
pub struct FfmetadataMuxer {
    out: IoWriter,
    stream_count: u32,
}

impl FfmetadataMuxer {
    /// # Errors
    /// As [`IoWriter::new`].
    pub fn new(sink: Box<dyn MediaSink>) -> vaco_core::Result<Self> {
        Ok(Self {
            out: IoWriter::new(sink, &IoOptions::default())?,
            stream_count: 0,
        })
    }
}

impl Muxer for FfmetadataMuxer {
    fn flags(&self) -> FormatFlags {
        FLAGS
    }

    fn add_stream(&mut self, _params: &CodecParameters) -> vaco_core::Result<u32> {
        // Measured: the reference accepts any codec type here (query_codec
        // never rejected one in probing) since the format carries no
        // bitstream at all — it only ever discards write_packet's payload.
        let index = self.stream_count;
        self.stream_count += 1;
        Ok(index)
    }

    fn write_header(&mut self) -> vaco_core::Result<()> {
        // No `[STREAM]`/`[CHAPTER]` blocks: see the type docs for why there is
        // nothing to put in one via this trait.
        let doc = write(&[], &[], &[]);
        self.out.write(doc.as_bytes())
    }

    fn write_packet(&mut self, _packet: &Packet) -> vaco_core::Result<()> {
        Ok(())
    }

    fn write_trailer(&mut self) -> vaco_core::Result<()> {
        self.out.flush()
    }

    fn stream_time_base(&self, _stream_index: u32) -> Option<Rational> {
        None
    }
}

/// `-f ffmetadata` flags. Measured: a file with zero streams is a valid,
/// non-error output (`NOSTREAMS`); packets carry no timestamp discipline at
/// all since none are ever written (`NOTIMESTAMPS`, `TS_NONSTRICT`).
pub const FLAGS: FormatFlags = FormatFlags::NOTIMESTAMPS
    .union(FormatFlags::TS_NONSTRICT)
    .union(FormatFlags::NOSTREAMS)
    .union(FormatFlags::TS_NEGATIVE);

/// # Errors
/// As [`FfmetadataMuxer::new`].
fn open_ffmetadata(sink: Box<dyn MediaSink>) -> vaco_core::Result<Box<dyn Muxer>> {
    Ok(Box::new(FfmetadataMuxer::new(sink)?))
}

/// `ffmetadata`: `ffmpeg -h muxer=ffmetadata` names it "`FFmpeg` metadata in
/// text" and declares no default codec of either kind — matches this
/// registration.
pub static MUXER_FFMETADATA: MuxerDesc = MuxerDesc {
    name: "ffmetadata",
    long_name: "FFmpeg metadata in text",
    extensions: &["ffmeta"],
    default_video: None,
    default_audio: None,
    open: open_ffmetadata,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn escapes_the_five_special_characters() {
        assert_eq!(escape_flat("a=b;c#d\\e"), "a\\=b\\;c\\#d\\\\e");
    }

    #[test]
    fn escape_value_splits_newlines_with_backslash_continuation() {
        assert_eq!(escape_value("multi\nline"), "multi\\\nline");
    }

    #[test]
    fn unescape_drops_backslash_before_any_character() {
        assert_eq!(unescape("a\\nb"), "anb");
        assert_eq!(unescape("abc\\"), "abc");
        assert_eq!(unescape("a\\=b\\;c\\#d\\\\e"), "a=b;c#d\\e");
    }

    #[test]
    fn parse_skips_comments_blank_lines_and_bare_lines() {
        let doc = "; a comment\n# also a comment\ntitle=Hello\n\nnotakeyvalue\nartist=Someone\n";
        let parsed = parse(doc);
        assert_eq!(
            parsed.global,
            vec![
                ("title".to_owned(), "Hello".to_owned()),
                ("artist".to_owned(), "Someone".to_owned()),
            ]
        );
    }

    #[test]
    fn parse_does_not_require_the_header_line() {
        let parsed = parse("title=NoHeader\n");
        assert_eq!(
            parsed.global,
            vec![("title".to_owned(), "NoHeader".to_owned())]
        );
    }

    #[test]
    fn parse_treats_any_semicolon_or_hash_line_as_a_comment() {
        let parsed = parse(";FFMETADATA2\ntitle=X\n");
        assert_eq!(parsed.global, vec![("title".to_owned(), "X".to_owned())]);
    }

    #[test]
    fn key_and_value_are_not_trimmed() {
        let parsed = parse("title = spaced\n");
        assert_eq!(
            parsed.global,
            vec![("title ".to_owned(), " spaced".to_owned())]
        );
    }

    #[test]
    fn sections_collect_arbitrary_keys_until_the_next_section() {
        let doc = "title=G\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=100\ny=2\nafterglobal=3\n";
        let parsed = parse(doc);
        assert_eq!(parsed.global, vec![("title".to_owned(), "G".to_owned())]);
        assert_eq!(parsed.chapters.len(), 1);
        assert_eq!(
            parsed.chapters[0].metadata,
            vec![
                ("TIMEBASE".to_owned(), "1/1000".to_owned()),
                ("START".to_owned(), "0".to_owned()),
                ("END".to_owned(), "100".to_owned()),
                ("y".to_owned(), "2".to_owned()),
                ("afterglobal".to_owned(), "3".to_owned()),
            ]
        );
    }

    #[test]
    fn write_matches_the_measured_shape() {
        let out = write(
            &[
                ("title".to_owned(), "A Title".to_owned()),
                ("comment".to_owned(), "multi\nline".to_owned()),
            ],
            &[],
            &[],
        );
        assert_eq!(
            out,
            ";FFMETADATA1\ntitle=A Title\ncomment=multi\\\nline\nencoder=vaco\n"
        );
    }

    #[test]
    fn write_overwrites_a_caller_supplied_encoder_key() {
        let out = write(&[("encoder".to_owned(), "custom".to_owned())], &[], &[]);
        assert_eq!(out, ";FFMETADATA1\nencoder=vaco\n");
    }

    /// Regression: `fuzz/fuzz_targets/mux_stream_ffmetadata_reader.rs` found
    /// that a key starting with `[` round-tripped through a `[SECTION]`
    /// header instead of surviving as a global pair, because `parse` checks
    /// for a section header before it looks for `=`. See
    /// `fuzz/seeds/mux_stream_ffmetadata_reader/`.
    #[test]
    fn a_key_starting_with_a_bracket_does_not_read_back_as_a_section() {
        let key = "[weird key".to_owned();
        let value = "value".to_owned();
        let out = write(&[(key.clone(), value.clone())], &[], &[]);
        let parsed = parse(&out);
        assert!(parsed.chapters.is_empty() && parsed.streams.is_empty());
        assert!(parsed.global.contains(&(key, value)));
    }

    #[test]
    fn write_orders_streams_before_chapters() {
        let out = write(
            &[],
            &[vec![("title".to_owned(), "Video".to_owned())]],
            &[ChapterMeta {
                time_base: (1, 1000),
                start: 0,
                end: 100,
                metadata: vec![("title".to_owned(), "Ch1".to_owned())],
            }],
        );
        let stream_pos = out.find("[STREAM]").unwrap();
        let chapter_pos = out.find("[CHAPTER]").unwrap();
        assert!(stream_pos < chapter_pos);
    }

    #[test]
    fn full_document_round_trips_through_parse() {
        let written = write(
            &[("title".to_owned(), "Movie".to_owned())],
            &[vec![("title".to_owned(), "Video Track".to_owned())]],
            &[ChapterMeta {
                time_base: (1, 1000),
                start: 0,
                end: 2000,
                metadata: vec![("title".to_owned(), "Chapter One".to_owned())],
            }],
        );
        let parsed = parse(&written);
        assert!(
            parsed
                .global
                .contains(&("title".to_owned(), "Movie".to_owned()))
        );
        assert_eq!(parsed.streams.len(), 1);
        assert_eq!(parsed.chapters.len(), 1);
        assert!(
            parsed.chapters[0]
                .metadata
                .contains(&("START".to_owned(), "0".to_owned()))
        );
    }

    proptest! {
        /// The escaping round-trips for any string: no value can desync a
        /// downstream differential comparison by picking up or losing bytes.
        #[test]
        fn escape_value_round_trips(s in ".*") {
            let escaped = escape_value(&s);
            prop_assert_eq!(unescape(&escaped), s);
        }

        /// A single `key=value` global line survives a full write+parse
        /// cycle for any key/value pair drawn from characters this grammar
        /// treats specially, plus arbitrary text.
        #[test]
        fn single_global_kv_round_trips(
            key in "[a-zA-Z0-9_]{1,12}",
            value in ".{0,40}",
        ) {
            let written = write(&[(key.clone(), value.clone())], &[], &[]);
            let parsed = parse(&written);
            prop_assert!(parsed.global.contains(&(key, value)));
        }
    }

    #[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
    mod muxer {
        use super::*;
        use vaco_core::MediaType;
        use vaco_format_core::vacoraw::MemorySink;

        #[test]
        fn accepts_any_stream_and_writes_only_header_and_encoder() {
            let sink = MemorySink::new();
            let shared = sink.shared();
            let mut m = FfmetadataMuxer::new(Box::new(sink)).unwrap();
            assert_eq!(
                m.add_stream(&CodecParameters::new(MediaType::Video))
                    .unwrap(),
                0
            );
            assert_eq!(
                m.add_stream(&CodecParameters::new(MediaType::Attachment))
                    .unwrap(),
                1
            );
            m.write_header().unwrap();
            m.write_trailer().unwrap();
            let text = String::from_utf8(shared.snapshot()).unwrap();
            assert_eq!(text, ";FFMETADATA1\nencoder=vaco\n");
        }

        #[test]
        fn opens_from_the_registry_descriptor() {
            let sink = Box::new(MemorySink::new());
            assert!((MUXER_FFMETADATA.open)(sink).is_ok());
            assert!(MUXER_FFMETADATA.matches_name("ffmetadata"));
        }
    }
}
