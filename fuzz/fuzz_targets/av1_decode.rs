//! AV1 decode against arbitrary bytes: OBU framing, sequence/frame header
//! parsing (via `vaco-parse-av1`), the symbol decoder and its adaptive CDF
//! machinery, the tile/superblock/partition/mode-info walk, coefficient
//! decode, dequantization, inverse transforms and intra prediction — the
//! whole `Av1Decoder::send_packet`/`receive_frame` pipeline, run one packet
//! at a time so the fuzzer never has to synthesise an IVF/OBU-stream
//! container to reach any of it.
//!
//! Frame dimensions, tile counts, superblock size, partition depths and
//! transform sizes are all attacker-controlled fields read straight out of
//! the bitstream, so this is also the target that would catch an
//! allocation sized directly off header data (this crate's own
//! `vaco_limits::Budget` convention throughout `decode.rs`/`framebuf.rs`
//! instead of a raw `Vec::with_capacity`).
//!
//! fuzz-crate: vaco-codec-av1

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_av1::Av1Decoder;
use vaco_codec_core::Decoder;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::strict());
    let Ok(packet) = Packet::from_slice(&mut budget, data) else {
        return;
    };
    let mut decoder = Av1Decoder::new(Limits::strict());
    if decoder.send_packet(Some(&packet)).is_err() {
        return;
    }
    while let Ok(frame) = decoder.receive_frame() {
        // Every plane the decoder claims to have written must actually be
        // addressable at the frame's own reported dimensions -- a mismatch
        // here is exactly the kind of cropping/stride bug a fuzzer alone
        // (as opposed to the differential test suite in tests/oracle.rs)
        // can find fast, since it only needs an out-of-bounds
        // `Plane::row`/`get`, not a pixel-accuracy oracle.
        for idx in 0..3 {
            if let Some(plane) = frame.plane(idx) {
                let _ = plane.row(0);
            }
        }
    }
    // A second packet exercises the sequence header this decoder persists
    // across temporal units (`Av1Decoder::seq`, read back by every later
    // frame/tile-group OBU), not just a single temporal unit's decode in
    // isolation.
    if decoder.send_packet(Some(&packet)).is_err() {
        return;
    }
    while decoder.receive_frame().is_ok() {}
});
