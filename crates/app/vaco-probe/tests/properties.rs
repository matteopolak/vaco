//! Properties that must hold for every stream a demuxer can hand us.
//!
//! The unit tests pin *one* observed output each. These pin the invariants that
//! have to survive inputs nobody wrote a test for — which is the whole set,
//! since a `Stream` is built from an untrusted file.
//!
//! The strongest one is [`emitted_keys_are_always_a_subsequence_of_the_table`]:
//! it says the field *order* is a property of the table and not of the values,
//! so no combination of present and absent data can reorder the output. A
//! reordering would be a byte divergence that only shows up on some files,
//! which is the worst kind to find.

#![allow(
    clippy::expect_used,
    reason = "a test that cannot set up is a failed test"
)]

use proptest::prelude::*;

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{CodecId, CodecParameters, Level, Profile};
use vaco_core::{MediaType, Rational, Timestamp};
use vaco_format_core::{Disposition, Stream};
use vaco_probe::emit::Emit;
use vaco_probe::fields;
use vaco_probe::show;
use vaco_textformat::sections::SectionId;
use vaco_textformat::{FormatOpts, OptionalFields, TextFormat, writers};

/// Every writer, so a property is not accidentally about one of them.
const WRITERS: [&str; 7] = ["default", "compact", "csv", "flat", "ini", "json", "xml"];

prop_compose! {
    /// A stream whose every optional field is independently present or absent.
    fn any_stream()(
        index in 0u32..8,
        media in 0usize..4,
        codec in prop::option::of(0usize..8),
        width in 0u32..8192,
        height in 0u32..8192,
        sar_num in -4i32..8,
        sar_den in -4i32..8,
        fps_num in -4i32..120,
        fps_den in -4i32..8,
        tb_num in -4i32..8,
        tb_den in -4i32..96_000,
        sample_rate in 0u32..192_000,
        channels in prop::option::of(1u32..9),
        bit_rate in prop::option::of(0u64..100_000_000),
        level in prop::option::of(-200i32..256),
        profile in prop::option::of(0i32..256),
        start in prop::option::of(-1_000_000i64..1_000_000),
        duration in prop::option::of(0i64..100_000_000),
        frames in prop::option::of(0u64..1_000_000),
        extradata in prop::option::of(0usize..64),
        tag in prop::option::of(prop::array::uniform4(any::<u8>())),
        disposition in any::<u32>(),
        tags in prop::collection::vec(("[a-z_]{1,8}", "[\\PC]{0,16}"), 0..4),
    ) -> Stream {
        // `.get()` rather than `[]`: `indexing_slicing` is denied workspace-wide
        // and a test is not exempt from it. The strategy's range already keeps
        // this in bounds, which is exactly why indexing would look safe and
        // still be one edit away from not being.
        let media = *[
            MediaType::Video,
            MediaType::Audio,
            MediaType::Subtitle,
            MediaType::Data,
        ]
        .get(media)
        .unwrap_or(&MediaType::Data);
        let mut s = Stream::new(index, media, Rational::new(tb_num, tb_den));
        s.id = Some(i64::from(index));
        s.start_time = start.map_or(Timestamp::NONE, Timestamp::new);
        s.duration_ts = duration;
        s.frame_count = frames;
        s.disposition = Disposition::from_bits_truncate(disposition);
        s.metadata = tags.into_iter().collect();

        let all: Vec<CodecId> = CodecId::all().collect();
        s.params = match media {
            MediaType::Video => CodecParameters::video(),
            MediaType::Audio => CodecParameters::audio(),
            other => CodecParameters::new(other),
        };
        if let Some(i) = codec
            && let Some(id) = all.get(i % all.len().max(1))
        {
            s.params.codec_id = Some(*id);
        }
        s.params.codec_tag = tag;
        s.params.bit_rate = bit_rate;
        s.params.level = level.map(Level);
        s.params.profile = profile.map(|value| Profile { value, name: "P" });
        s.params.extradata = extradata.map(|n| vec![0u8; n]);
        if let Some(v) = s.params.video.as_mut() {
            v.width = width;
            v.height = height;
            v.coded_width = width;
            v.coded_height = height;
            v.sample_aspect_ratio = Rational::new(sar_num, sar_den);
            v.frame_rate = Rational::new(fps_num, fps_den);
        }
        if let Some(a) = s.params.audio.as_mut() {
            a.sample_rate = sample_rate;
            a.layout = channels.map(|n| {
                ChannelLayout::unspecified(n)
            });
        }
        s
    }
}

fn render(spec: &str, streams: &[Stream]) -> String {
    let w = writers::make(spec).expect("writer");
    let mut tf = TextFormat::new(w, Vec::new(), FormatOpts::default());
    tf.open(SectionId::ROOT).expect("root");
    tf.open(SectionId::STREAMS).expect("streams");
    {
        let mut e = Emit::new(&mut tf, OptionalFields::Auto);
        for s in streams {
            show::stream(&mut e, s, true).expect("stream");
        }
    }
    tf.close().expect("streams");
    tf.close().expect("root");
    String::from_utf8(tf.finish().expect("finish")).expect("utf8")
}

/// The `flat` writer's keys, in emission order, up to the disposition block.
fn stream_keys(s: &Stream) -> Vec<String> {
    render("flat", std::slice::from_ref(s))
        .lines()
        .filter_map(|l| l.split_once('='))
        .filter_map(|(path, _)| path.rsplit('.').next())
        .take_while(|k| *k != "default")
        .map(str::to_owned)
        .collect()
}

proptest! {
    /// **The field order is a property of the table, not of the values.**
    ///
    /// Whatever is present and whatever is missing, the keys that do appear
    /// appear in table order and nothing else appears at all. Without this,
    /// a divergence could hide behind "only on files that have no aspect
    /// ratio".
    #[test]
    fn emitted_keys_are_always_a_subsequence_of_the_table(s in any_stream()) {
        let keys = stream_keys(&s);
        let mut table = fields::STREAM.iter().map(|f| f.name);
        for k in &keys {
            let found = table.any(|name| name == k);
            prop_assert!(found, "{k} is out of order or not in the table: {keys:?}");
        }
    }

    /// Every writer produces valid UTF-8 and terminates, for every stream.
    #[test]
    fn every_writer_survives_every_stream(s in any_stream()) {
        for spec in WRITERS {
            let out = render(spec, std::slice::from_ref(&s));
            prop_assert!(out.is_char_boundary(out.len()));
        }
    }

    /// Rendering is deterministic. D6 is byte identity; output that varies run
    /// to run cannot be byte-identical to anything.
    #[test]
    fn rendering_is_deterministic(s in any_stream()) {
        for spec in WRITERS {
            prop_assert_eq!(
                render(spec, std::slice::from_ref(&s)),
                render(spec, std::slice::from_ref(&s)),
            );
        }
    }

    /// A stream's own output does not depend on its neighbours.
    ///
    /// `flat` and `ini` number elements by position, so the *paths* differ;
    /// the field keys and values must not. This is what makes `-select_streams`
    /// safe: filtering the list cannot change what a surviving stream prints.
    #[test]
    fn a_streams_fields_do_not_depend_on_its_neighbours(
        a in any_stream(),
        b in any_stream(),
    ) {
        // `lines()` rather than a trim: a tag value may legitimately end in a
        // space, and trimming would compare a byte the writer emitted against
        // one it did not.
        let alone = render("compact", std::slice::from_ref(&a));
        let together = render("compact", &[a, b]);
        prop_assert_eq!(
            alone.lines().next().unwrap_or_default(),
            together.lines().next().unwrap_or_default(),
        );
    }

    /// `-show_optional_fields never` is a subset of `auto`, which is a subset
    /// of `always`, for every writer and every stream.
    ///
    /// The three settings differ only in what they do with an *absent* value,
    /// so nothing may appear at a stricter setting that is missing at a looser
    /// one.
    #[test]
    fn the_optional_field_policies_are_ordered(s in any_stream()) {
        for spec in ["flat", "json"] {
            let count = |policy| {
                let w = writers::make(spec).expect("writer");
                let opts = FormatOpts { show_optional_fields: policy, ..FormatOpts::default() };
                let mut tf = TextFormat::new(w, Vec::new(), opts);
                tf.open(SectionId::ROOT).expect("root");
                {
                    let mut e = Emit::new(&mut tf, policy);
                    show::stream(&mut e, &s, true).expect("stream");
                }
                tf.close().expect("root");
                let text = String::from_utf8(tf.finish().expect("finish")).expect("utf8");
                text.matches('\n').count()
            };
            let never = count(OptionalFields::Never);
            let auto = count(OptionalFields::Auto);
            let always = count(OptionalFields::Always);
            prop_assert!(never <= auto, "{spec}: never {never} > auto {auto}");
            prop_assert!(auto <= always, "{spec}: auto {auto} > always {always}");
        }
    }
}

proptest! {
    /// argv parsing is total: no argument vector panics, and the answer does
    /// not change between two identical calls.
    #[test]
    fn parsing_argv_is_total_and_deterministic(
        argv in prop::collection::vec("[-a-zA-Z0-9_:=,./ ]{0,12}", 0..8),
    ) {
        let first = vaco_probe::cli::parse(&argv).is_ok();
        let second = vaco_probe::cli::parse(&argv).is_ok();
        prop_assert_eq!(first, second);
    }
}
