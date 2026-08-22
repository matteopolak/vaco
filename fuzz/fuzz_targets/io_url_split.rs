//! The URL splitter, against arbitrary text.
//!
//! A URL is the most direct untrusted input in the project: it arrives from a
//! command line, from a playlist, and from a container's own metadata. The
//! invariant that matters is that splitting is **lossless** — if
//! `split_url(s).to_string() != s`, then what the whitelist checked and what a
//! protocol opens are two different strings, which is exactly the shape of a
//! bypass.
#![no_main]
use libfuzzer_sys::fuzz_target;
use vaco_protocol_core::split_url;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = core::str::from_utf8(data) else {
        return;
    };
    let url = split_url(s);

    // Lossless: the split can be reassembled into exactly the input.
    let rendered = url.to_string();
    assert_eq!(rendered, s, "split_url lost bytes");

    // Idempotent: splitting the rendering yields the same split.
    let again = split_url(&rendered);
    assert_eq!(again, url, "split_url is not idempotent");

    // A scheme, when found, is a legal scheme name — nothing else may be
    // dispatched on.
    if let Some(scheme) = &url.scheme {
        assert!(
            scheme.starts_with(|c: char| c.is_ascii_alphabetic()),
            "scheme {scheme:?} does not start with a letter"
        );
        assert!(
            scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-'),
            "scheme {scheme:?} contains an illegal character"
        );
    }

    // A bare path is `file` and only `file` (rule U1).
    if url.scheme.is_none() {
        assert_eq!(url.effective_scheme(), "file");
        assert!(url.args.is_empty());
        assert_eq!(url.rest, s);
    }

    // Taking inline options must not invent or lose bytes either.
    let mut with_opts = split_url(s);
    with_opts.take_inline_opts();
    assert!(with_opts.rest.len() <= url.rest.len());
});
