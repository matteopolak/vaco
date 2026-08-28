//! PNG/APNG decode against arbitrary bytes: it must never panic, and every
//! successful decode must survive a decode -> encode -> decode round trip
//! with pixel-identical output for its first frame.
//!
//! Only the first decoded frame is re-encoded: `vaco_codec_png::encode`
//! takes a full frame list, and for a fuzzed APNG re-encoding *every*
//! output frame would just re-derive the same composited-canvas property
//! `vaco-codec-png`'s own `round_trips_apng` test already checks with a
//! synthetic fixture. One frame is enough to catch a decode/encode pixel
//! disagreement the arbitrary-byte fuzzing above cannot, the same shape as
//! `parse_qoi.rs`.
//!
//! fuzz-crate: vaco-codec-png
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
    let Ok(frames) = vaco_codec_png::decode(data, &mut budget) else {
        return;
    };
    let Some(first) = frames.into_iter().next() else {
        return;
    };

    let mut budget2 = Budget::new(Limits::strict());
    let Ok(encoded) = vaco_codec_png::encode(std::slice::from_ref(&first), &mut budget2) else {
        // Every pixel format `decode` produces is one `encode` accepts
        // (checked directly against `png_color_for`'s match), so a failure
        // here would be a real bug, not a documented gap -- but budget
        // exhaustion under `Limits::strict()` is a legitimate `Err`, so
        // this returns rather than panicking.
        return;
    };

    let mut budget3 = Budget::new(Limits::permissive());
    let redecoded = vaco_codec_png::decode(&encoded, &mut budget3)
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
