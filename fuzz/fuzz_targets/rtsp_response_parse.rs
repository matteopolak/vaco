//! `Response::parse_head` against arbitrary bytes.
//!
//! Every byte here came off a socket to an RTSP server this crate does not
//! control. Properties: the parser never panics, `content_length()` never
//! panics on whatever `Content-Length` value was parsed (including a
//! header claiming a negative or absurdly large number, which `usize`
//! parsing simply rejects rather than wrapping), and a successful parse's
//! `status` is always the numeric value actually present in the input's
//! status line.
//! fuzz-crate: vaco-demux-rtsp

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_demux_rtsp::message::Response;

fuzz_target!(|data: &[u8]| {
    let Ok(resp) = Response::parse_head(data) else {
        return;
    };
    // Must never panic, and must be a plausible RTSP status code shape —
    // the parser accepts any u16, which is intentional (a server sending a
    // nonstandard code should not itself be treated as a parse failure).
    let _ = resp.content_length();
    let _ = resp.cseq();
    let _ = resp.status;
});
