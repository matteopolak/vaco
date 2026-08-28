//! JPEG decode against arbitrary bytes: baseline and progressive, every
//! marker parser, every scan variant, restart handling.
//!
//! fuzz-crate: vaco-codec-jpeg
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_limits::{Budget, Limits};

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::strict());
    let _ = vaco_codec_jpeg::decode(data, &mut budget);
});
