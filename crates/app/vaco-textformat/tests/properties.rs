//! Property tests for the escaping tables and the writers built on them.
//!
//! Two families:
//!
//! * **Round trip.** Wherever an escape is invertible, escaping then
//!   unescaping must be the identity for *any* string. This is what catches an
//!   escape that is emitted but not consumed, or a two-character escape that
//!   collides with a one-character one.
//! * **Separator containment.** A writer must never emit its own item
//!   separator unescaped inside a value, because a consumer splitting on it
//!   would tear the record in half. `compact` and `csv` uphold this; `flat`
//!   deliberately does not, and that deviation gets its own test rather than
//!   being quietly excluded.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a panic is the assertion mechanism"
)]

use proptest::prelude::*;
use vaco_textformat::escape::{
    DEFAULT_REPLACEMENT, StringValidation, escape_c, escape_csv, escape_flat, escape_ini,
    escape_json, escape_xml, unescape_c, unescape_csv, unescape_flat, unescape_ini, unescape_json,
    unescape_xml, validate_xml,
};
use vaco_textformat::sections::SectionId;
use vaco_textformat::{FormatOpts, TextFormat, writers};

/// Strings biased towards the characters the escaping tables care about.
fn nasty() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            2 => prop::char::range('\u{0}', '\u{1f}'),
            3 => prop::char::any(),
            8 => prop::sample::select(vec![
                '\\', '"', '\'', '=', ':', '#', ';', '|', ',', '@', '$', '`', '<', '>', '&',
                '[', ']', '/', ' ', '\t', '\n', '\r', 'a', '0', 'ü',
            ]),
        ],
        0..24,
    )
    .prop_map(|v| v.into_iter().collect())
}

/// Whether `s` contains `sep` outside a backslash escape.
fn has_bare(s: &str, sep: char) -> bool {
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            it.next();
        } else if c == sep {
            return true;
        }
    }
    false
}

proptest! {
    #[test]
    fn compact_c_round_trips(v in nasty(), sep in prop::sample::select(vec!['|', ',', '@', ':'])) {
        let e = escape_c(&v, sep);
        let back = unescape_c(&e, sep);
        prop_assert_eq!(back.as_deref(), Some(v.as_str()));
    }

    #[test]
    fn compact_csv_round_trips(v in nasty(), sep in prop::sample::select(vec!['|', ',', '@'])) {
        let e = escape_csv(&v, sep);
        let back = unescape_csv(&e);
        prop_assert_eq!(back.as_deref(), Some(v.as_str()));
    }

    #[test]
    fn ini_round_trips(v in nasty()) {
        let e = escape_ini(&v);
        let back = unescape_ini(&e);
        prop_assert_eq!(back.as_deref(), Some(v.as_str()));
    }

    #[test]
    fn json_round_trips(v in nasty()) {
        let e = escape_json(&v);
        let back = unescape_json(&e);
        prop_assert_eq!(back.as_deref(), Some(v.as_str()));
    }

    #[test]
    fn flat_round_trips(v in nasty()) {
        let e = escape_flat(&v);
        let back = unescape_flat(&e);
        prop_assert_eq!(back.as_deref(), Some(v.as_str()));
    }

    #[test]
    fn xml_round_trips(v in nasty()) {
        // Only for strings XML can carry: validation runs first in the writer,
        // and it is lossy by design.
        let v = validate_xml(&v, StringValidation::Replace, DEFAULT_REPLACEMENT)
            .unwrap_or_default();
        let e = escape_xml(&v);
        let back = unescape_xml(&e);
        prop_assert_eq!(back.as_deref(), Some(v.as_str()));
    }

    /// `escape=c` never leaves the item separator bare.
    #[test]
    fn compact_c_never_emits_a_bare_separator(
        v in nasty(),
        sep in prop::sample::select(vec!['|', ',', '@', ':', '=']),
    ) {
        prop_assert!(!has_bare(&escape_c(&v, sep), sep));
    }

    /// `escape=csv` quotes whenever the separator, a quote, LF or CR appears,
    /// so a bare separator only ever occurs inside quotes.
    #[test]
    fn compact_csv_quotes_whenever_it_must(
        v in nasty(),
        sep in prop::sample::select(vec!['|', ',', '@']),
    ) {
        let e = escape_csv(&v, sep);
        if v.contains([sep, '"', '\n', '\r']) {
            prop_assert!(e.starts_with('"') && e.ends_with('"'), "{e:?}");
        } else {
            prop_assert_eq!(e, v);
        }
    }

    /// The `compact` writer as a whole: a tag value survives the full path
    /// through the writer and back out of the line.
    #[test]
    fn compact_writer_line_round_trips(v in nasty()) {
        let w = writers::make("compact").expect("writer");
        let mut tf = TextFormat::new(w, Vec::new(), FormatOpts::default());
        tf.open(SectionId::ROOT).expect("root");
        tf.open(SectionId::STREAMS).expect("streams");
        tf.open(SectionId::STREAM).expect("stream");
        tf.open(SectionId::STREAM_TAGS).expect("tags");
        tf.tag("K", &v).expect("tag");
        tf.close().expect("tags");
        tf.close().expect("stream");
        tf.close().expect("streams");
        tf.close().expect("root");
        let out = String::from_utf8(tf.finish().expect("finish")).expect("utf8");

        let body = out.strip_prefix("stream|tag:K=").and_then(|s| s.strip_suffix('\n'));
        let body = body.expect("shape");
        let back = unescape_c(body, '|');
        prop_assert_eq!(back.as_deref(), Some(v.as_str()));
    }

    /// No writer panics, and none of them ever emits a partial record: the
    /// output of a complete section tree always ends with a newline (or is
    /// empty, which only `flat` can be).
    #[test]
    fn no_writer_panics_and_records_are_terminated(v in nasty(), key in "[A-Za-z0-9_.\\- ]{0,12}") {
        for spec in writers::NAMES {
            let w = writers::make(spec).expect("writer");
            let mut tf = TextFormat::new(w, Vec::new(), FormatOpts::default());
            tf.open(SectionId::ROOT).expect("root");
            tf.open(SectionId::STREAMS).expect("streams");
            tf.open(SectionId::STREAM).expect("stream");
            tf.int("index", 0).expect("index");
            tf.open(SectionId::STREAM_TAGS).expect("tags");
            tf.tag(&key, &v).expect("tag");
            tf.close().expect("tags");
            tf.close().expect("stream");
            tf.close().expect("streams");
            tf.close().expect("root");
            let out = tf.finish().expect("finish");
            prop_assert!(out.ends_with(b"\n"), "{spec}: {:?}", String::from_utf8_lossy(&out));
        }
    }

    /// `flat` sanitises keys down to `[A-Za-z0-9_]`, so a key can never inject
    /// a path separator no matter what a container put in its metadata.
    #[test]
    fn flat_keys_cannot_inject_a_path_separator(key in nasty()) {
        let w = writers::make("flat").expect("writer");
        let mut tf = TextFormat::new(w, Vec::new(), FormatOpts::default());
        tf.open(SectionId::ROOT).expect("root");
        tf.open(SectionId::STREAMS).expect("streams");
        tf.open(SectionId::STREAM).expect("stream");
        tf.open(SectionId::STREAM_TAGS).expect("tags");
        tf.tag(&key, "v").expect("tag");
        tf.close().expect("tags");
        tf.close().expect("stream");
        tf.close().expect("streams");
        tf.close().expect("root");
        let out = String::from_utf8(tf.finish().expect("finish")).expect("utf8");
        let prefix = "streams.stream.0.tags.";
        let rest = out.strip_prefix(prefix).expect("prefix");
        let sanitised = rest.split_once('=').expect("assignment").0;
        prop_assert!(
            sanitised.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "{sanitised:?}"
        );
        prop_assert_eq!(sanitised.chars().count(), key.chars().count());
    }
}

/// `flat` really does emit its `sep_char` unescaped inside values.
///
/// Pinned rather than excluded: it is the reference behaviour, so a future
/// "fix" that escapes it would be a byte divergence.
#[test]
fn flat_does_not_escape_its_own_separator() {
    assert_eq!(escape_flat("a.b"), "a.b");
    assert_eq!(escape_flat("a#b"), "a#b");
}

/// `ini` escapes `=`, `:` and `#` but leaves `;` alone.
#[test]
fn ini_escapes_the_surprising_set() {
    assert_eq!(escape_ini("a=b:c#d;e"), "a\\=b\\:c\\#d;e");
}
