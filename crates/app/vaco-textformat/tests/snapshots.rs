//! Snapshots pinning each writer's exact output shape.
//!
//! The torture test compares against the reference binary and is the source of
//! truth. These snapshots do something the torture test cannot: they show the
//! *whole shape* of a rich document in one reviewable blob, so a change to
//! indentation, blank lines or option handling turns into a readable diff in
//! review rather than a byte offset in an assertion.
//!
//! Regenerate with `cargo insta review` after — and only after — a reference
//! run justifies the change.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a panic is the assertion mechanism"
)]

use vaco_textformat::sections::SectionId;
use vaco_textformat::{FormatOpts, OptionalFields, Result, TextFormat, Unit, writers};

/// A document that reaches every structural feature at once: two root
/// children, an array of elements, both inline styles, a variable-field
/// section, a unique-typed section, and an empty array.
fn document(tf: &mut TextFormat<Vec<u8>>) -> Result<()> {
    tf.open(SectionId::ROOT)?;

    tf.open(SectionId::PROGRAMS)?;
    tf.close()?;

    tf.open(SectionId::STREAMS)?;
    for i in 0..2 {
        tf.open(SectionId::STREAM)?;
        tf.int("index", i)?;
        tf.str("codec_name", "aac")?;
        tf.str("sample_rate", "44100")?;
        tf.int("channels", 1)?;
        tf.duration("start_time", Some(0.0))?;
        tf.value("bit_rate", Some(70303.0), Unit::BitPerSecond)?;
        tf.int_opt("max_bit_rate", None)?;
        tf.open(SectionId::STREAM_DISPOSITION)?;
        tf.int("default", 1)?;
        tf.close()?;
        tf.open(SectionId::STREAM_TAGS)?;
        tf.tag("language", "und")?;
        tf.tag("WE-IRD_KEY.1", "v=1,c:2|q\"3\\4;e[f]#g <&> ünï")?;
        tf.close()?;
        tf.open(SectionId::STREAM_SIDE_DATA_LIST)?;
        tf.open_typed(SectionId::STREAM_SIDE_DATA, "Skip Samples")?;
        tf.str("side_data_type", "Skip Samples")?;
        tf.int("skip_samples", 1024)?;
        tf.close()?;
        tf.close()?;
        tf.close()?;
    }
    tf.close()?;

    tf.open(SectionId::FORMAT)?;
    tf.str("filename", "t.mkv")?;
    tf.value("size", Some(10028.0), Unit::Byte)?;
    tf.open(SectionId::FORMAT_TAGS)?;
    tf.tag("title", "hello")?;
    tf.close()?;
    tf.close()?;

    tf.close()
}

fn render(spec: &str, opts: FormatOpts) -> String {
    let w = writers::make(spec).expect("writer spec");
    let mut tf = TextFormat::new(w, Vec::new(), opts);
    document(&mut tf).expect("emit");
    String::from_utf8(tf.finish().expect("finish")).expect("utf8")
}

macro_rules! snapshot {
    ($name:ident, $spec:literal) => {
        #[test]
        fn $name() {
            insta::assert_snapshot!(render($spec, FormatOpts::default()));
        }
    };
    ($name:ident, $spec:literal, $opts:expr) => {
        #[test]
        fn $name() {
            insta::assert_snapshot!(render($spec, $opts));
        }
    };
}

snapshot!(default_writer, "default");
snapshot!(default_nokey, "default=nk=1");
snapshot!(default_noprint_wrappers, "default=nw=1");
snapshot!(compact_writer, "compact");
snapshot!(compact_escape_csv, "compact=e=csv");
snapshot!(compact_escape_none, "compact=e=none");
snapshot!(compact_no_section, "compact=p=0");
snapshot!(compact_separator, "compact=s=@");
snapshot!(csv_writer, "csv");
snapshot!(flat_writer, "flat");
snapshot!(flat_non_hierarchical, "flat=h=0");
snapshot!(ini_writer, "ini");
snapshot!(ini_non_hierarchical, "ini=h=0");
snapshot!(json_writer, "json");
snapshot!(json_compact, "json=c=1");
snapshot!(xml_writer, "xml");
snapshot!(xml_fully_qualified, "xml=q=1");

snapshot!(default_pretty, "default", FormatOpts::pretty());
snapshot!(json_pretty, "json", FormatOpts::pretty());
snapshot!(
    default_optional_always,
    "default",
    FormatOpts {
        show_optional_fields: OptionalFields::Always,
        ..FormatOpts::default()
    }
);
snapshot!(
    json_optional_always,
    "json",
    FormatOpts {
        show_optional_fields: OptionalFields::Always,
        ..FormatOpts::default()
    }
);
snapshot!(
    json_optional_never,
    "json",
    FormatOpts {
        show_optional_fields: OptionalFields::Never,
        ..FormatOpts::default()
    }
);

/// `xsd_strict=1` refuses the run configurations XML cannot represent, and
/// accepts the ones 8.1 does not check for.
#[test]
fn xsd_strict_refuses_unit_and_prefix_only() {
    let cases = [
        (FormatOpts::default(), true),
        (
            FormatOpts {
                pretty: vaco_textformat::Pretty {
                    unit: true,
                    ..Default::default()
                },
                ..FormatOpts::default()
            },
            false,
        ),
        (
            FormatOpts {
                pretty: vaco_textformat::Pretty {
                    prefix: true,
                    ..Default::default()
                },
                ..FormatOpts::default()
            },
            false,
        ),
        // Not checked by 8.1: a no-op option and one it simply does not test.
        (
            FormatOpts {
                pretty: vaco_textformat::Pretty {
                    byte_binary_prefix: true,
                    sexagesimal: true,
                    ..Default::default()
                },
                ..FormatOpts::default()
            },
            true,
        ),
    ];
    for (opts, ok) in cases {
        let w = writers::make("xml=x=1").expect("writer");
        let tf = TextFormat::new(w, Vec::new(), opts.clone());
        assert_eq!(tf.validate().is_ok(), ok, "{opts:?}");
    }
}

/// The message text ffprobe prints before exiting 1.
#[test]
fn xsd_strict_message() {
    let w = writers::make("xml=xsd_strict=1").expect("writer");
    let tf = TextFormat::new(
        w,
        Vec::new(),
        FormatOpts {
            pretty: vaco_textformat::Pretty {
                unit: true,
                ..Default::default()
            },
            ..FormatOpts::default()
        },
    );
    let err = tf.validate().expect_err("must refuse");
    insta::assert_snapshot!(err.to_string());
}
