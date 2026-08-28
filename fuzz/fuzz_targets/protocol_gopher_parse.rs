//! Two pure, I/O-free parsers of `vaco-protocol-gopher`.
//!
//! 1. `selector::split_authority` on an arbitrary `gopher:` URL tail.
//! 2. `selector::parse` on an arbitrary path.
//! fuzz-crate: vaco-protocol-gopher
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_protocol_gopher::selector;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = core::str::from_utf8(data) else {
        return;
    };
    let (_, path) = selector::split_authority(s);
    let _ = selector::parse(path);
    let _ = selector::parse(s);
});
