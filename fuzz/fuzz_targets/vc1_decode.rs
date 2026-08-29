//! VC-1 I-picture decode against arbitrary bytes as a packet payload, with
//! a fixed, valid extradata (this crate's own `STRUCT_C + VERT_SIZE +
//! HORIZ_SIZE` convention — see `vaco-codec-vc1`'s `header` module doc) so
//! the fuzzer reaches the picture/macroblock/block layer instead of failing
//! at "no extradata set" on every input. Exercises header parsing, CBPCY
//! neighbour prediction, DC/AC entropy decode (including all three escape
//! modes), dequantization, and the Annex A inverse transform end to end.
//!
//! fuzz-crate: vaco-codec-vc1

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::Decoder;
use vaco_codec_vc1::Vc1Decoder;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

const EXTRADATA: [u8; 12] = {
    let struct_c = 0x4100_0001u32.to_be_bytes();
    let vert = 64u32.to_le_bytes();
    let horiz = 64u32.to_le_bytes();
    [
        struct_c[0], struct_c[1], struct_c[2], struct_c[3],
        vert[0], vert[1], vert[2], vert[3],
        horiz[0], horiz[1], horiz[2], horiz[3],
    ]
};

fuzz_target!(|data: &[u8]| {
    if data.len() > 65536 {
        return;
    }
    let mut budget = Budget::new(Limits::default());
    let Ok(packet) = Packet::from_slice(&mut budget, data) else {
        return;
    };
    let mut dec = Vc1Decoder::new(Limits::default());
    if dec.set_extradata(&EXTRADATA).is_err() {
        return;
    }
    if dec.send_packet(Some(&packet)).is_err() {
        return;
    }
    while dec.receive_frame().is_ok() {}
});
