//! Two pure, I/O-free surfaces of `vaco-protocol-httpproxy`, neither of
//! which touches a network — this crate's own docs say why request/response
//! construction is the tested surface rather than a live proxy.
//!
//! 1. [`connect::parse`] on an arbitrary `httpproxy:` URL tail.
//! 2. [`connect::parse_response`] on an arbitrary (fake, attacker-controlled
//!    in spirit — this is exactly what a malicious or broken proxy could
//!    send) HTTP response header block.
//! fuzz-crate: vaco-protocol-httpproxy
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_protocol_httpproxy::connect;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = core::str::from_utf8(data) else {
        return;
    };
    let _ = connect::parse(s);
    let _ = connect::parse_response(s);
});
