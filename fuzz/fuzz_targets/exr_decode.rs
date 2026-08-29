//! OpenEXR decode against arbitrary bytes: it must never panic, and every
//! successful decode must survive a decode -> encode -> decode round trip
//! within a small floating-point tolerance.
//!
//! Exact equality is not the right property here even for `f32` samples:
//! `exr`'s own default write compression need not be bit-identical to
//! whatever compression the fuzzed input declared, so this checks that
//! every sample stays within `1e-4` of its original value rather than
//! asserting the encoded bytes decode to numerically identical `f32`s.
//!
//! fuzz-crate: vaco-codec-exr
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_limits::{Budget, Limits};

fn frame_floats(frame: &vaco_frame::Frame) -> Option<Vec<f32>> {
    let plane = frame.plane(0)?;
    let mut out = Vec::new();
    for row in plane.rows_iter() {
        for chunk in row.chunks_exact(4) {
            let bytes: [u8; 4] = chunk.try_into().ok()?;
            out.push(f32::from_ne_bytes(bytes));
        }
    }
    Some(out)
}

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::strict());
    let Ok(frame) = vaco_codec_exr::decode(data, &mut budget) else {
        return;
    };

    let Ok(encoded) = vaco_codec_exr::encode(&frame, &vaco_codec_exr::EncodeOptions::default()) else {
        return;
    };
    let mut budget2 = Budget::new(Limits::permissive());
    let redecoded = vaco_codec_exr::decode(&encoded, &mut budget2)
        .expect("re-encoding a successfully decoded frame must itself be decodable");

    let (Some(a), Some(b)) = (frame_floats(&frame), frame_floats(&redecoded)) else {
        panic!("both frames have plane 0 and a stride that is a multiple of 4 bytes");
    };
    assert_eq!(a.len(), b.len(), "decode -> encode -> decode changed the sample count");
    for (x, y) in a.iter().zip(&b) {
        assert!(
            (x - y).abs() < 1e-4 || (x.is_nan() && y.is_nan()),
            "decode -> encode -> decode changed a sample beyond tolerance: {x} vs {y}"
        );
    }
});
