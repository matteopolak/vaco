//! `subfile:`'s odd comma-delimited option grammar, against arbitrary text.
//!
//! Every `subfile:` URL's `args` comes from `Url::args`, which is itself
//! untrusted the same way the URL splitter is (see `io_url_split.rs`):
//! it can arrive from a playlist naming a byte range inside a Motion-JPEG
//! file. `parse_args` must never panic, and whatever it does accept must obey
//! the one invariant it itself is supposed to enforce.
//! fuzz-crate: vaco-protocol-wrap
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_protocol_wrap::subfile::parse_args;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = core::str::from_utf8(data) else {
        return;
    };
    if let Ok(range) = parse_args(s)
        && let Some(end) = range.end
    {
        assert!(end >= range.start, "end before start slipped through parse_args");
    }
});
