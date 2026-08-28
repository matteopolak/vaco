//! WebP decode against arbitrary bytes: it must never panic, and every
//! successful decode's first frame must survive a decode -> encode ->
//! decode round trip with pixel-identical output.
//!
//! `vaco_codec_webp::encode` is lossless-only and takes one frame, which is
//! exactly what `decode` produces per frame (`Rgb24` or `Rgba`, both in its
//! accepted set), so — unlike GIF — this asserts exact pixel equality, the
//! same shape as `parse_qoi.rs`.
//!
//! fuzz-crate: vaco-codec-webp
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_limits::{Budget, Limits};

fn frame_bytes(frame: &vaco_frame::Frame) -> Option<Vec<u8>> {
    let plane = frame.plane(0)?;
    let mut out = Vec::new();
    for row in plane.rows_iter() {
        out.extend_from_slice(row);
    }
    Some(out)
}

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::strict());
    let Ok(frames) = vaco_codec_webp::decode(data, &mut budget) else {
        return;
    };
    let Some(first) = frames.into_iter().next() else {
        return;
    };

    let Ok(encoded) = vaco_codec_webp::encode(&first) else {
        return;
    };
    let mut budget2 = Budget::new(Limits::permissive());
    let redecoded = vaco_codec_webp::decode(&encoded, &mut budget2)
        .expect("re-encoding a successfully decoded frame must itself be decodable");
    let Some(redecoded_first) = redecoded.into_iter().next() else {
        panic!("encoding one frame must decode back to at least one frame");
    };

    assert_eq!(
        frame_bytes(&first),
        frame_bytes(&redecoded_first),
        "decode -> encode -> decode changed pixel content"
    );
});
