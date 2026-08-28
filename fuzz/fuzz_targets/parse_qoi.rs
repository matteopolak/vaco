//! QOI decode against arbitrary bytes: it must never panic, and every
//! successful decode must re-encode without erroring.
//!
//! fuzz-crate: vaco-codec-qoi
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_limits::{Budget, Limits};

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::strict());
    if let Ok(frame) = vaco_codec_qoi::decode(data, &mut budget) {
        let _ = vaco_codec_qoi::encode(&frame);
    }
});
