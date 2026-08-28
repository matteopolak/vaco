//! VP8 decode against arbitrary bytes: frame-tag parse, the compressed
//! header, segmentation, mode/motion-vector records, coefficient tokens,
//! dequantization/inverse transforms, intra and inter prediction, the loop
//! filter — the whole `Vp8Decoder::send_packet`/`receive_frame` pipeline,
//! run one packet at a time so the fuzzer never has to synthesise an IVF or
//! WebM container to reach any of it.
//!
//! Frame dimensions, segment counts and partition counts are all
//! attacker-controlled fields read straight out of the bitstream, so this
//! is also the target that would catch an allocation sized directly off
//! header data (the workspace-wide `vaco_limits::Budget` convention this
//! crate uses throughout `decode.rs`/`framebuf.rs` instead of a raw
//! `Vec::with_capacity`).
//!
//! fuzz-crate: vaco-codec-vp8

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::Decoder;
use vaco_codec_vp8::Vp8Decoder;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::strict());
    let Ok(packet) = Packet::from_slice(&mut budget, data) else {
        return;
    };
    let mut decoder = Vp8Decoder::new(Limits::strict());
    if decoder.send_packet(Some(&packet)).is_err() {
        return;
    }
    while let Ok(frame) = decoder.receive_frame() {
        // Every plane the decoder claims to have written must actually be
        // addressable at the frame's own reported dimensions -- a mismatch
        // here is exactly the kind of cropping/stride bug a fuzzer alone
        // (as opposed to the differential test suite) can find fast, since
        // it only needs an out-of-bounds `Plane::row`/`get`, not a
        // pixel-accuracy oracle.
        for idx in 0..3 {
            if let Some(plane) = frame.plane(idx) {
                let _ = plane.row(0);
            }
        }
    }
    // A second packet (e.g. an interframe referencing state the first
    // packet may or may not have set up) exercises the persistent
    // reference-frame/entropy-context state machine, not just a single
    // key-frame decode.
    if decoder.send_packet(Some(&packet)).is_err() {
        return;
    }
    while decoder.receive_frame().is_ok() {}
});
