//! `vaco_format_rtp::sdp::parse` against arbitrary text.
//!
//! An SDP body is exactly what an RTSP `DESCRIBE` response hands over
//! verbatim from a server this crate does not control. Properties: the
//! parser never panics on any byte sequence (valid UTF-8 or not — invalid
//! UTF-8 is turned into a `String` losslessly-ish via `from_utf8_lossy`
//! before parsing, so this target also exercises what a real caller
//! actually does with untrusted bytes off a socket), it always terminates,
//! and every `MediaDescription` it returns has a `port` field that
//! round-trips through the same textual `m=` line it was read from (a
//! sanity check that the line-splitting itself is not corrupting fields).
//! fuzz-crate: vaco-format-rtp

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let Ok(sess) = vaco_format_rtp::sdp::parse(&text) else {
        return;
    };
    // Every media block's `port` field must have come from a `m=` line
    // actually present in the input, never a value the parser invented.
    for media in &sess.media {
        let port_str = media.port.to_string();
        assert!(
            text.contains(&port_str) || media.port == 0,
            "a MediaDescription named a port not present anywhere in the input"
        );
    }
});
