//! `vaco_protocol_dial::read_header_block` reading an arbitrary byte stream
//! standing in for a malicious or broken peer.
//! fuzz-crate: vaco-protocol-dial
#![no_main]

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;
use vaco_protocol_dial::read_header_block;

fuzz_target!(|data: &[u8]| {
    let mut cursor = Cursor::new(data);
    let _ = read_header_block(&mut cursor, "fuzz", "closed early");
});
