//! fuzz-crate: vaco-protocol-srtp
//!
//! `#551`'s attacker-controlled surface: a wire packet, not a key. Fixed
//! (fuzzer-owned) keys derive a real [`vaco_protocol_srtp::SrtpContext`],
//! then arbitrary bytes are fed straight to `unprotect` as if they had
//! arrived off the network. Property: `unprotect` never panics — it
//! either authenticates and decrypts, or returns `None`, on any input.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_protocol_srtp::{SrtpContext, derive_session_keys_aes128};

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let seq = u16::from_le_bytes([data[0], data[1]]);
    let header_len = usize::from(data[2]);
    let packet = &data[3..];

    let keys = derive_session_keys_aes128(&[0x42; 16], &[0x24; 14]);
    let mut ctx = SrtpContext::new(keys, 0xAABB_CCDD);
    let _ = ctx.unprotect(seq, header_len, packet);
});
