//! VP9 decode against arbitrary bytes: superframe splitting, the
//! uncompressed + compressed header, segmentation, the partition/mode-info
//! bitstream walk, coefficient token decode, dequantization, inverse
//! transforms and intra prediction — the whole
//! `Vp9Decoder::send_packet`/`receive_frame` pipeline, run one packet at a
//! time so the fuzzer never has to synthesise an IVF or WebM container to
//! reach any of it.
//!
//! Frame dimensions, tile sizes, superframe sub-frame counts and partition
//! depths are all attacker-controlled fields read straight out of the
//! bitstream, so this is also the target that would catch an allocation
//! sized directly off header data (the workspace-wide `vaco_limits::Budget`
//! convention this crate uses throughout `decode.rs`/`framebuf.rs` instead
//! of a raw `Vec::with_capacity`).
//!
//! fuzz-crate: vaco-codec-vp9

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::Decoder;
use vaco_codec_vp9::Vp9Decoder;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::strict());
    let Ok(packet) = Packet::from_slice(&mut budget, data) else {
        return;
    };
    let mut decoder = Vp9Decoder::new(Limits::strict());
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
    // A second packet exercises the persisted loop-filter/segmentation
    // state a later key frame's header parse reads back
    // (`parse_uncompressed_header`'s `prev_loop_filter`/`prev_seg`
    // parameters), not just a single frame's decode in isolation.
    if decoder.send_packet(Some(&packet)).is_err() {
        return;
    }
    while decoder.receive_frame().is_ok() {}
});
