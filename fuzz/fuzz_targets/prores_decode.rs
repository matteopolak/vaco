//! ProRes decode against arbitrary bytes as a whole packet payload — no
//! separate extradata to build, since RDD 36's `frame()` syntax (frame size,
//! `'icpf'` tag, frame/picture/slice headers, and coefficient data) is
//! entirely in-band per packet. Exercises header parsing, the
//! Golomb-Rice/exponential-Golomb entropy decode, block/slice inverse
//! scanning, dequantization, and the IDCT/reconstruction pipeline end to
//! end.
//!
//! fuzz-crate: vaco-codec-prores

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::Decoder;
use vaco_codec_prores::ProresDecoder;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

fuzz_target!(|data: &[u8]| {
    if data.len() > 65536 {
        return;
    }
    let mut budget = Budget::new(Limits::default());
    let Ok(packet) = Packet::from_slice(&mut budget, data) else {
        return;
    };
    let mut dec = ProresDecoder::new(Limits::default());
    if dec.send_packet(Some(&packet)).is_err() {
        return;
    }
    while dec.receive_frame().is_ok() {}
});
