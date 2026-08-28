//! Two pure, I/O-free surfaces of `vaco-protocol-icecast`, neither of which
//! touches a network — this crate's own docs say why request construction
//! and response-status parsing are the tested surfaces rather than a live
//! Icecast server.
//!
//! 1. [`protocol::parse_url`] on an arbitrary `icecast:` URL tail.
//! 2. [`request::parse_status_line`] on an arbitrary (fake, attacker-controlled
//!    in spirit — this is exactly what a malicious or broken server could
//!    send) HTTP response prefix.
//! fuzz-crate: vaco-protocol-icecast
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_protocol_icecast::{protocol, request};

fuzz_target!(|data: &[u8]| {
    let _ = request::parse_status_line(data);

    let Ok(s) = core::str::from_utf8(data) else {
        return;
    };
    let _ = protocol::parse_url(s, false);
    let _ = protocol::parse_url(s, true);
});
