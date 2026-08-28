//! MPEG-1/2 video decode against arbitrary bytes: sequence/GOP/picture
//! header and extension parsing, slice/macroblock walking, VLC decode for
//! macroblock type/motion vectors/coded_block_pattern/DCT coefficients,
//! motion compensation and B-picture reference management — the whole
//! `Mpeg12Decoder::send_packet`/`receive_frame` pipeline, run one packet at
//! a time so the fuzzer never has to synthesise a container to reach any
//! of it.
//!
//! Picture and macroblock-grid dimensions are attacker-controlled fields
//! read straight out of `sequence_header()`/`sequence_extension()`, so this
//! is also the target that would catch an allocation sized directly off
//! header data rather than through the workspace-wide `vaco_limits::Budget`
//! convention this crate uses in `decoder.rs`.
//!
//! Two packets are sent per input (like `vp8_decode`'s target): the first
//! exercises decode from a cold start, the second exercises the persistent
//! `previous`/`recent`/`held` reference-picture state a P- or B-picture
//! macroblock reads from, and the escape-coding/`ActivePicture::mpeg1`
//! branch a real MPEG-1-vs-MPEG-2 bug in this crate once lived in — both
//! directions of that branch should survive arbitrary input without
//! panicking, even though neither is yet pixel-accurate on every input.
//!
//! fuzz-crate: vaco-codec-mpeg12

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::Decoder;
use vaco_codec_mpeg12::Mpeg12Decoder;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::strict());
    let Ok(packet) = Packet::from_slice(&mut budget, data) else {
        return;
    };
    let mut decoder = Mpeg12Decoder::new(Limits::strict());
    if decoder.send_packet(Some(&packet)).is_err() {
        return;
    }
    while let Ok(frame) = decoder.receive_frame() {
        // Every plane the decoder claims to have written must actually be
        // addressable — an out-of-bounds `Plane::row` here is exactly the
        // kind of allocation/cropping bug a fuzzer, unlike the pixel-
        // accuracy differential suite, is well-placed to find fast.
        for idx in 0..3 {
            if let Some(plane) = frame.plane(idx) {
                let _ = plane.row(0);
            }
        }
    }
    if decoder.send_packet(Some(&packet)).is_err() {
        return;
    }
    while decoder.receive_frame().is_ok() {}
});
