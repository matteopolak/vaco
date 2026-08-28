//! MPEG-1/2/2.5 Layer I/II/III decode against arbitrary bytes: header
//! parse, bit allocation, side info, Huffman decode, requantisation and the
//! synthesis filterbank — the whole `MpegAudioDecoder::send_packet` /
//! `receive_frame` pipeline, run on one packet at a time so the fuzzer
//! never has to synthesise a demuxer's framing to reach any of it.
//!
//! What is asserted beyond "does not panic": every sample a produced frame
//! carries is finite — a malformed bitstream driving the Huffman decoder's
//! escape/`linbits` path or the `2^x` gain terms to `NaN`/`inf` must not
//! silently produce that, since it would propagate into a mixer or a file
//! write downstream.
//!
//! fuzz-crate: vaco-codec-mpegaudio

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::Decoder;
use vaco_codec_mpegaudio::MpegAudioDecoder;
use vaco_frame::FrameData;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::strict());
    let Ok(packet) = Packet::from_slice(&mut budget, data) else {
        return;
    };
    let mut decoder = MpegAudioDecoder::new(Limits::strict());
    if decoder.send_packet(Some(&packet)).is_err() {
        return;
    }
    while let Ok(frame) = decoder.receive_frame() {
        let FrameData::Audio { planes, .. } = &frame.data else {
            continue;
        };
        for plane in planes {
            for chunk in plane.data.as_slice().chunks_exact(4) {
                let mut bytes = [0u8; 4];
                bytes.copy_from_slice(chunk);
                let sample = f32::from_le_bytes(bytes);
                assert!(sample.is_finite(), "decode produced a non-finite sample");
            }
        }
    }
});
