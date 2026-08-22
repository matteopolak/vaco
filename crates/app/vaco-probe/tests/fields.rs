//! The emitters follow [`vaco_probe::fields`], in order.
//!
//! Plan 14 §4.4 asks for exactly this: "a test that walks the emitted key
//! sequence per section against `FieldDesc` rows, and fails on any mismatch —
//! including ordering". Without it, a field can be added to the table and never
//! emitted, or emitted and never declared, and either one is a silent
//! divergence from the reference that no unit test would catch.
//!
//! The key sequence is read back out of the `flat` writer, which prints one
//! `path.key=value` per line and never reorders. `-show_optional_fields always`
//! is in force so that an unavailable field still occupies its slot.

#![allow(
    clippy::expect_used,
    reason = "a test that cannot set up is a failed test"
)]

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{MediaType, Rational};
use vaco_format_core::Stream;
use vaco_probe::emit::Emit;
use vaco_probe::fields::{self, Absent, Field, Scope};
use vaco_probe::show::{self, FormatInfo};
use vaco_textformat::sections::SectionId;
use vaco_textformat::{FormatOpts, OptionalFields, TextFormat, writers};

/// Render with `flat` and `-show_optional_fields always`, then return the key
/// of every line in emission order.
fn keys(f: impl FnOnce(&mut Emit<'_, Vec<u8>>)) -> Vec<String> {
    let w = writers::make("flat").expect("writer");
    let opts = FormatOpts {
        show_optional_fields: OptionalFields::Always,
        ..FormatOpts::default()
    };
    let mut tf = TextFormat::new(w, Vec::new(), opts);
    tf.open(SectionId::ROOT).expect("root");
    {
        let mut e = Emit::new(&mut tf, OptionalFields::Always);
        f(&mut e);
    }
    tf.close().expect("root");
    let text = String::from_utf8(tf.finish().expect("finish")).expect("utf8");
    text.lines()
        .filter_map(|l| l.split_once('='))
        .filter_map(|(path, _)| path.rsplit('.').next())
        .map(str::to_owned)
        .collect()
}

/// The names a table declares for a stream of `media`, in table order.
fn expected(table: &'static [Field], media: Option<MediaType>) -> Vec<String> {
    table
        .iter()
        .filter(|f| match f.scope {
            Scope::Always => true,
            Scope::Video => media == Some(MediaType::Video),
            Scope::Audio => media == Some(MediaType::Audio),
            Scope::VideoOrSubtitle => {
                matches!(media, Some(MediaType::Video | MediaType::Subtitle))
            }
        })
        // `Absent::Omit` and `Absent::Never` rows disappear when the value is
        // missing, so a stream with nothing in them cannot show them.
        .filter(|f| f.absent != Absent::Omit)
        .map(|f| f.name.to_owned())
        .collect()
}

fn bare(index: u32, media: MediaType) -> Stream {
    let mut s = Stream::new(index, media, Rational::new(1, 1000));
    s.params = match media {
        MediaType::Video => CodecParameters::video().with_codec(CodecId::H264),
        // PCM rather than AAC on purpose: PCM has no RFC 6381 codecs
        // parameter, so `mime_codec_string` — an `Absent::Omit` row — stays
        // absent and the expected sequence is exactly the non-`Omit` rows.
        MediaType::Audio => CodecParameters::audio().with_codec(CodecId::Pcm),
        _ => CodecParameters::new(media),
    };
    s
}

#[test]
fn a_video_stream_emits_the_table_in_order() {
    let got = keys(|e| {
        e.tf().open(SectionId::STREAMS).expect("streams");
        e.tf().open(SectionId::STREAM).expect("stream");
        show::stream(e, &bare(0, MediaType::Video)).expect("stream");
        e.tf().close().expect("stream");
        e.tf().close().expect("streams");
    });
    let want = expected(fields::STREAM, Some(MediaType::Video));
    // `show::stream` opens its own STREAM section, so the outer one contributes
    // nothing but nesting; the key sequence is unaffected.
    let got: Vec<String> = got.into_iter().take_while(|k| k != "default").collect();
    assert_eq!(got, want);
}

#[test]
fn an_audio_stream_emits_the_table_in_order() {
    let got = keys(|e| {
        show::stream(e, &bare(0, MediaType::Audio)).expect("stream");
    });
    let want = expected(fields::STREAM, Some(MediaType::Audio));
    let got: Vec<String> = got.into_iter().take_while(|k| k != "default").collect();
    assert_eq!(got, want);
}

#[test]
fn a_subtitle_stream_gets_width_and_height_and_nothing_else_visual() {
    let got = keys(|e| {
        show::stream(e, &bare(0, MediaType::Subtitle)).expect("stream");
    });
    let got: Vec<String> = got.into_iter().take_while(|k| k != "default").collect();
    assert_eq!(got, expected(fields::STREAM, Some(MediaType::Subtitle)));
    assert!(got.contains(&"width".to_owned()));
    assert!(!got.contains(&"pix_fmt".to_owned()));
    assert!(!got.contains(&"sample_rate".to_owned()));
}

#[test]
fn the_format_section_emits_its_table_in_order() {
    let got = keys(|e| {
        let info = FormatInfo {
            filename: "x",
            format_name: "f",
            format_long_name: "F",
            probe_score: 0,
            size: None,
            nb_programs: 0,
            nb_stream_groups: 0,
        };
        show::format(e, &info, &[], None, &[]).expect("format");
    });
    assert_eq!(got, expected(fields::FORMAT, None));
}

#[test]
fn the_error_section_emits_its_table_in_order() {
    let got = keys(|e| {
        show::error(e, -2, "No such file or directory").expect("error");
    });
    assert_eq!(got, expected(fields::ERROR, None));
}

#[test]
fn the_packet_section_emits_its_table_in_order() {
    use vaco_limits::{Budget, Limits};
    use vaco_packet::Packet;

    let mut budget = Budget::new(Limits::strict());
    let pkt = Packet::from_slice(&mut budget, b"payload").expect("packet");
    let stream = bare(0, MediaType::Video);
    let got = keys(|e| {
        e.tf().open(SectionId::PACKETS).expect("packets");
        show::packet(e, &pkt, Some(&stream)).expect("packet");
        e.tf().close().expect("packets");
    });
    assert_eq!(got, expected(fields::PACKET, None));
}

#[test]
fn every_declared_field_is_reachable_from_some_emitter() {
    // A table row nothing emits is a lie about the output. Collect the union of
    // what the three media types produce and compare against the table.
    let mut seen: Vec<String> = Vec::new();
    for media in [MediaType::Video, MediaType::Audio, MediaType::Subtitle] {
        for k in keys(|e| {
            show::stream(e, &bare(0, media)).expect("stream");
        }) {
            if !seen.contains(&k) {
                seen.push(k);
            }
        }
    }
    for field in fields::STREAM {
        if field.absent == Absent::Omit {
            // `mime_codec_string` and `extradata_size` need a value to appear at
            // all; `tests/reference.rs` covers both with a real stream.
            continue;
        }
        assert!(
            seen.contains(&field.name.to_owned()),
            "stream.{} is declared but never emitted",
            field.name
        );
    }
}

#[test]
fn nothing_is_emitted_that_the_table_does_not_declare() {
    for media in [MediaType::Video, MediaType::Audio, MediaType::Subtitle] {
        for k in keys(|e| {
            show::stream(e, &bare(0, media)).expect("stream");
        }) {
            let known = fields::find(fields::STREAM, &k).is_some()
                // The disposition sub-section's keys are the 19 flag names.
                || vaco_cli_core::Disposition::ALL.iter().any(|(_, n)| *n == k);
            assert!(known, "{k} is emitted but not declared");
        }
    }
}
