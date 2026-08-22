//! The URL grammar's round-trip invariant, and the shapes plan 18 names.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "tests"
)]

use proptest::prelude::*;
use vaco_protocol_core::split_url;

#[test]
fn every_documented_shape_survives() {
    let cases: &[(&str, &str, Option<&str>, &str)] = &[
        // input, scheme, nested, rest
        (
            "concat:file1.ts|file2.ts",
            "concat",
            None,
            "file1.ts|file2.ts",
        ),
        (
            "crypto+file:secret.bin",
            "crypto",
            Some("file"),
            "secret.bin",
        ),
        (
            "tee:out1.mkv|[f=mpegts]out2.ts",
            "tee",
            None,
            "out1.mkv|[f=mpegts]out2.ts",
        ),
        ("pipe:1", "pipe", None, "1"),
        (
            "data:audio/wav;base64,UklGRg",
            "data",
            None,
            "audio/wav;base64,UklGRg",
        ),
        ("async:http://host/path", "async", None, "http://host/path"),
        (
            "cache:https://host/path",
            "cache",
            None,
            "https://host/path",
        ),
        ("clip.mkv", "file", None, "clip.mkv"),
        (r"C:\videos\clip.mkv", "file", None, r"C:\videos\clip.mkv"),
    ];
    for (input, scheme, nested, rest) in cases {
        let u = split_url(input);
        assert_eq!(u.effective_scheme(), *scheme, "{input}");
        assert_eq!(u.nested.as_deref(), *nested, "{input}");
        assert_eq!(u.rest, *rest, "{input}");
        assert_eq!(&u.to_string(), input, "{input}");
    }
}

#[test]
fn subfile_arguments_are_kept_for_the_protocol() {
    let u = split_url("subfile,,start,1024,end,4096,,:archive.bin");
    assert_eq!(u.effective_scheme(), "subfile");
    assert_eq!(u.args, ",,start,1024,end,4096,,");
    assert_eq!(u.rest, "archive.bin");
}

proptest! {
    /// Splitting is lossless for every string, so no nested open can differ
    /// between what was checked and what is opened.
    #[test]
    fn split_then_format_is_the_identity(s in ".{0,120}") {
        prop_assert_eq!(split_url(&s).to_string(), s);
    }

    /// And splitting is idempotent under formatting.
    #[test]
    fn split_is_idempotent(s in ".{0,120}") {
        let once = split_url(&s);
        let twice = split_url(&once.to_string());
        prop_assert_eq!(once, twice);
    }

    /// A scheme, if found, is always a legal scheme name.
    #[test]
    fn scheme_shape_is_respected(s in "[a-zA-Z0-9+.,:/\\\\_-]{0,60}") {
        let u = split_url(&s);
        if let Some(scheme) = &u.scheme {
            prop_assert!(scheme.starts_with(|c: char| c.is_ascii_alphabetic()), "{scheme}");
            prop_assert!(
                scheme.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-'),
                "{scheme}"
            );
        }
    }
}
