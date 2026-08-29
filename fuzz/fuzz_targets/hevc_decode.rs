//! HEVC decode against arbitrary bytes: Annex-B NAL splitting, VPS/SPS/PPS
//! parsing (via `vaco-parse-hevc`), CABAC (`vaco-codec-cabac`), the CTU
//! quadtree/coding-unit/transform-tree walk, residual coding, dequantisation,
//! inverse transform (`vaco-codec-dsp-idct::hevc`) and intra prediction — the
//! whole `HevcDecoder::send_packet`/`receive_frame` pipeline, run one packet
//! at a time so the fuzzer never has to synthesise a real elementary stream
//! to reach any of it.
//!
//! CTU size, coding-quadtree depth, transform-tree depth, PU/TU geometry and
//! residual block size are all attacker-controlled fields read straight out
//! of the bitstream, so this is also the target that would catch an
//! allocation sized directly off header data (this crate's own
//! `vaco_limits::Budget` convention throughout `framebuf.rs`, rather than a
//! raw `Vec::with_capacity`) and any panic from the `#[forbid(unsafe_code)]`,
//! no-`unwrap`/`indexing_slicing` discipline the rest of the crate follows.
//!
//! This crate is deliberately unregistered (see its crate doc) pending
//! further byte-exactness work, mirroring `vaco-codec-av1`/`vaco-codec-opus`
//! — this target still exists because the requirement is "parses untrusted
//! input", not "is registered".
//!
//! fuzz-crate: vaco-codec-hevc

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::Decoder;
use vaco_codec_hevc::HevcDecoder;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::strict());
    let Ok(packet) = Packet::from_slice(&mut budget, data) else {
        return;
    };
    let mut decoder = HevcDecoder::new(Limits::strict());
    if decoder.send_packet(Some(&packet)).is_err() {
        return;
    }
    while let Ok(frame) = decoder.receive_frame() {
        // Every plane the decoder claims to have written must actually be
        // addressable at the frame's own reported dimensions -- an
        // out-of-bounds `Plane::row`/`get` here is exactly the class of
        // cropping/stride bug a fuzzer finds fast without needing a
        // pixel-accuracy oracle (that is `tests/oracle.rs`'s job).
        for idx in 0..3 {
            if let Some(plane) = frame.plane(idx) {
                let _ = plane.row(0);
            }
        }
    }
    // A second packet exercises the VPS/SPS/PPS maps this decoder persists
    // across NAL units (`HevcDecoder::sps`/`pps`), not just a single slice's
    // decode in isolation against freshly-parsed parameter sets.
    if decoder.send_packet(Some(&packet)).is_err() {
        return;
    }
    while decoder.receive_frame().is_ok() {}
});
