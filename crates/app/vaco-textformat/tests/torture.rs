//! The torture test: one nasty value through every writer, compared against
//! the reference binary byte for byte.
//!
//! This is the highest-value test in the crate. `reference.rs` holds the exact
//! stdout of ~100 real `ffprobe` invocations; each scenario below replays the
//! same section/field sequence through [`TextFormat`] and asserts equality.
//!
//! When one of these fails, the writer is wrong — not the capture. Re-run the
//! recorded `ffprobe` command before touching anything.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a panic is the assertion mechanism"
)]

#[path = "reference.rs"]
mod reference;

use vaco_textformat::sections::SectionId;
use vaco_textformat::{FormatOpts, Result, TextFormat, writers};

/// The torture string from plan 14 §4.3.
const NASTY: &str = "v=1,c:2|q\"3\\4;e[f]#g <&> ünï";

/// Every C0 control, each labelled with its own hex code so a dropped or
/// doubled escape is visible in the diff.
fn control_chars() -> String {
    use std::fmt::Write as _;
    (1u8..32).fold(String::new(), |mut acc, i| {
        let _ = write!(acc, "{i:02x}{}", char::from(i));
        acc
    })
}

fn render(spec: &str, f: impl FnOnce(&mut TextFormat<Vec<u8>>) -> Result<()>) -> Vec<u8> {
    let w = writers::make(spec).expect("writer spec");
    let mut tf = TextFormat::new(w, Vec::new(), FormatOpts::default());
    f(&mut tf).expect("emit");
    tf.finish().expect("finish")
}

/// `-show_entries stream_tags=NASTY`: one stream with no fields of its own and
/// one tag. Exercises every escaping table plus the `<stream >` quirk.
fn torture_tag(tf: &mut TextFormat<Vec<u8>>) -> Result<()> {
    tf.open(SectionId::ROOT)?;
    tf.open(SectionId::STREAMS)?;
    tf.open(SectionId::STREAM)?;
    tf.open(SectionId::STREAM_TAGS)?;
    tf.tag("NASTY", NASTY)?;
    tf.close()?;
    tf.close()?;
    tf.close()?;
    tf.close()
}

/// The same shape with every C0 control character in the value.
fn control_chars_scenario(tf: &mut TextFormat<Vec<u8>>) -> Result<()> {
    tf.open(SectionId::ROOT)?;
    tf.open(SectionId::STREAMS)?;
    tf.open(SectionId::STREAM)?;
    tf.open(SectionId::STREAM_TAGS)?;
    tf.tag("X", &control_chars())?;
    tf.close()?;
    tf.close()?;
    tf.close()?;
    tf.close()
}

/// One packet with `Skip Samples` side data. Exercises `UNIQUE_TYPE`: the
/// `compact` compound key, the `xml` `type` attribute plus `<side_datum/>`
/// children, and the per-field int/str split that decides `pts` versus `size`.
fn packet_side_data(tf: &mut TextFormat<Vec<u8>>) -> Result<()> {
    tf.open(SectionId::ROOT)?;
    tf.open(SectionId::PACKETS)?;
    tf.open(SectionId::PACKET)?;
    tf.str("codec_type", "audio")?;
    tf.int("stream_index", 0)?;
    tf.int("pts", -1024)?;
    tf.str("pts_time", "-0.023220")?;
    tf.int("dts", -1024)?;
    tf.str("dts_time", "-0.023220")?;
    tf.int("duration", 1024)?;
    tf.str("duration_time", "0.023220")?;
    tf.str("size", "258")?;
    tf.str("pos", "44")?;
    tf.str("flags", "KD_")?;
    tf.open(SectionId::PACKET_SIDE_DATA_LIST)?;
    tf.open_typed(SectionId::PACKET_SIDE_DATA, "Skip Samples")?;
    tf.str("side_data_type", "Skip Samples")?;
    tf.int("skip_samples", 1024)?;
    tf.int("discard_padding", 0)?;
    tf.int("skip_reason", 0)?;
    tf.int("discard_reason", 0)?;
    tf.close()?;
    tf.close()?;
    tf.close()?;
    tf.close()?;
    tf.close()
}

/// A program with two streams. Exercises `compact`'s separator machine for a
/// nested header section (`program|program_id=1|stream|index=0`) and the blank
/// line its footer leaves behind.
fn program_streams(tf: &mut TextFormat<Vec<u8>>) -> Result<()> {
    tf.open(SectionId::ROOT)?;
    tf.open(SectionId::PROGRAMS)?;
    tf.open(SectionId::PROGRAM)?;
    tf.int("program_id", 1)?;
    tf.open(SectionId::PROGRAM_STREAMS)?;
    for i in 0..2 {
        tf.open(SectionId::PROGRAM_STREAM)?;
        tf.int("index", i)?;
        tf.close()?;
    }
    tf.close()?;
    tf.close()?;
    tf.close()?;
    tf.close()
}

/// Two streams, preceded by the empty `programs` and `stream_groups` arrays
/// that `-show_entries stream=index` opens because `stream` is also the local
/// name of `program_stream`.
///
/// This is the `ini` blank-line case: three blank lines before
/// `[streams.stream.0]`, one per empty array plus one before the header.
fn empty_arrays(tf: &mut TextFormat<Vec<u8>>) -> Result<()> {
    tf.open(SectionId::ROOT)?;
    tf.open(SectionId::PROGRAMS)?;
    tf.close()?;
    tf.open(SectionId::STREAM_GROUPS)?;
    tf.close()?;
    tf.open(SectionId::STREAMS)?;
    for i in 0..2 {
        tf.open(SectionId::STREAM)?;
        tf.int("index", i)?;
        tf.close()?;
    }
    tf.close()?;
    tf.close()
}

/// One stream and the format section, with the two empty arrays
/// `-show_entries stream=…` drags in. Covers both inline styles at once and
/// the `xml` writer's blank line between root children.
fn stream_and_format(tf: &mut TextFormat<Vec<u8>>) -> Result<()> {
    tf.open(SectionId::ROOT)?;
    tf.open(SectionId::PROGRAMS)?;
    tf.close()?;
    tf.open(SectionId::STREAM_GROUPS)?;
    tf.close()?;
    tf.open(SectionId::STREAMS)?;
    tf.open(SectionId::STREAM)?;
    tf.int("index", 0)?;
    tf.open(SectionId::STREAM_DISPOSITION)?;
    tf.int("default", 1)?;
    tf.int("forced", 0)?;
    tf.close()?;
    tf.open(SectionId::STREAM_TAGS)?;
    tf.tag("language", "und")?;
    tf.close()?;
    tf.close()?;
    tf.close()?;
    tf.open(SectionId::FORMAT)?;
    tf.str("size", "10028")?;
    tf.open(SectionId::FORMAT_TAGS)?;
    tf.tag("title", "hello")?;
    tf.close()?;
    tf.close()?;
    tf.close()
}

fn replay(scenario: &str, tf: &mut TextFormat<Vec<u8>>) -> Result<()> {
    match scenario {
        "torture_tag" => torture_tag(tf),
        "control_chars" => control_chars_scenario(tf),
        "packet_side_data" => packet_side_data(tf),
        "program_streams" => program_streams(tf),
        "empty_arrays" => empty_arrays(tf),
        "stream_and_format" => stream_and_format(tf),
        other => panic!("unknown scenario {other}"),
    }
}

#[test]
fn every_capture_matches_byte_for_byte() {
    let mut failures = Vec::new();
    let all = reference::CAPTURES
        .iter()
        .chain(reference::CAPTURES_STREAM_AND_FORMAT);
    let mut total = 0;
    for c in all {
        total += 1;
        let got = render(c.spec, |tf| replay(c.scenario, tf));
        if got != c.bytes {
            failures.push(format!(
                "\n  ffprobe -of {} {}\n  want: {:?}\n  got:  {:?}",
                c.spec,
                c.args,
                String::from_utf8_lossy(c.bytes),
                String::from_utf8_lossy(&got),
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {total} captures differ:{}",
        failures.len(),
        failures.join("")
    );
}

#[test]
fn the_capture_set_covers_every_writer() {
    for name in writers::NAMES {
        assert!(
            reference::CAPTURES.iter().any(|c| c.spec == name),
            "no capture for -of {name}"
        );
    }
}

/// The five findings plan 14 §4 flagged for confirmation, asserted directly so
/// a regression names the finding rather than a byte offset.
mod documented_findings {
    use super::*;

    fn find(scenario: &str, spec: &str) -> &'static [u8] {
        reference::CAPTURES
            .iter()
            .chain(reference::CAPTURES_STREAM_AND_FORMAT)
            .find(|c| c.scenario == scenario && c.spec == spec)
            .map_or_else(|| panic!("no capture for {scenario}/{spec}"), |c| c.bytes)
    }

    #[test]
    fn xml_writes_stream_with_a_trailing_space_when_there_are_no_attributes() {
        let bytes = find("torture_tag", "xml");
        let text = String::from_utf8_lossy(bytes);
        assert!(text.contains("<stream >\n"), "{text}");
        assert_eq!(render("xml", torture_tag), bytes, "we must reproduce it");
    }

    #[test]
    fn ini_blank_lines_are_not_one_per_section_header() {
        // Plan 14 §4.3 claims a `\n` before *every* header, including
        // wrappers. `torture_tag` disproves it: `[streams.stream.0.tags]`
        // follows `[streams.stream.0]` with no blank between them.
        let text = String::from_utf8_lossy(find("torture_tag", "ini")).into_owned();
        assert!(
            text.contains("[streams.stream.0]\n[streams.stream.0.tags]\n"),
            "{text}"
        );
        // …while `empty_arrays` really does open with three blank lines.
        let text = String::from_utf8_lossy(find("empty_arrays", "ini")).into_owned();
        assert!(
            text.starts_with("# ffprobe output\n\n\n\n[streams.stream.0]\n"),
            "{text}"
        );
    }

    #[test]
    fn json_number_versus_string_is_per_field() {
        let text = String::from_utf8_lossy(find("packet_side_data", "json")).into_owned();
        assert!(text.contains("\"pts\": -1024,"), "{text}");
        assert!(text.contains("\"size\": \"258\","), "{text}");
        assert!(text.contains("\"skip_samples\": 1024,"), "{text}");
    }

    #[test]
    fn compact_qualifies_unique_type_sections_with_a_sanitised_type() {
        let text = String::from_utf8_lossy(find("packet_side_data", "compact")).into_owned();
        assert!(
            text.contains("side_datum/skip_samples:skip_samples=1024"),
            "{text}"
        );
    }

    #[test]
    fn default_escapes_nothing_at_all() {
        let bytes = find("control_chars", "default");
        let text = String::from_utf8_lossy(bytes).into_owned();
        // A raw 0x01 survives to the output.
        assert!(text.contains("01\u{1}02\u{2}"), "{text:?}");
    }

    #[test]
    fn xml_replaces_characters_it_cannot_represent() {
        // Not in plan 14 at all: the `xml` writer's default string validation
        // substitutes U+FFFD for every C0 control except tab, LF and CR.
        let text = String::from_utf8_lossy(find("control_chars", "xml")).into_owned();
        assert!(text.contains("01\u{fffd}02\u{fffd}"), "{text:?}");
        assert!(text.contains("09\t0a\n0b\u{fffd}"), "{text:?}");
    }
}
