//! `vaco_protocol_rtmp::handshake::find_digest` on an arbitrary 1536-byte
//! signature — what a hostile "server" could send as S1 before this crate
//! has done anything more than an HMAC comparison with it.
//! fuzz-crate: vaco-protocol-rtmp
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_protocol_rtmp::handshake::{SIG_SIZE, find_digest};

fuzz_target!(|data: &[u8]| {
    let Some(chunk) = data.get(..SIG_SIZE) else {
        return;
    };
    let Ok(sig) = <[u8; SIG_SIZE]>::try_from(chunk) else {
        return;
    };
    let _ = find_digest(&sig, b"Genuine Adobe Flash Media Server 001");
    let _ = find_digest(&sig, b"Genuine Adobe Flash Player 001");
});
