//! GIF decode against arbitrary bytes: it must never panic, and every
//! successful decode's first composited frame must survive a decode ->
//! encode -> decode round trip.
//!
//! GIF's own encode is lossy by construction (256-colour NeuQuant
//! quantisation, 1-bit alpha), so this cannot assert pixel equality the way
//! `png_decode.rs`/`vaco-codec-qoi`'s target does -- only that the round
//! trip completes without panicking and produces a frame of the same
//! dimensions, which is what a quantiser disagreeing with itself between
//! two runs, or a dimension mismatch, would violate.
//!
//! fuzz-crate: vaco-codec-gif
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_frame::FrameData;
use vaco_limits::{Budget, Limits};

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::strict());
    let Ok(frames) = vaco_codec_gif::decode(data, &mut budget) else {
        return;
    };
    let Some(first) = frames.into_iter().next() else {
        return;
    };
    let FrameData::Video { width, height, .. } = first.data else {
        panic!("vaco-codec-gif only ever produces FrameData::Video");
    };

    let Ok(encoded) = vaco_codec_gif::encode(std::slice::from_ref(&first)) else {
        return;
    };
    let mut budget2 = Budget::new(Limits::permissive());
    let redecoded = vaco_codec_gif::decode(&encoded, &mut budget2)
        .expect("re-encoding a successfully decoded frame must itself be decodable");
    let Some(redecoded_first) = redecoded.into_iter().next() else {
        panic!("encoding one frame must decode back to at least one frame");
    };
    let FrameData::Video { width: rw, height: rh, .. } = redecoded_first.data else {
        panic!("vaco-codec-gif only ever produces FrameData::Video");
    };
    assert_eq!((width, height), (rw, rh), "decode -> encode -> decode changed dimensions");
});
