//! `data:` URL parsing against arbitrary text.
//!
//! The most direct untrusted-input surface `vaco-protocol-local` has: a
//! `data:` URL's `rest` comes straight from wherever the outer URL string
//! came from (a command line, a playlist, an HLS variant list), and
//! `data::parse` must never panic on it, however malformed.
//! fuzz-crate: vaco-protocol-local
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_protocol_local::data::parse;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = core::str::from_utf8(data) else {
        return;
    };
    let result = parse(s);
    if let Ok(bytes) = result
        && let Some(comma) = s.find(',')
    {
        let header = &s[..comma];
        let is_base64 = header.split(';').skip(1).any(|p| p == "base64");
        if !is_base64 {
            // Point 1 of the module docs: a non-base64 payload is passed
            // through byte-for-byte, with no percent-decoding.
            assert_eq!(bytes, s[comma + 1..].as_bytes(), "literal payload changed");
        }
    }
});
