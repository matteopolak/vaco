//! JPEG XL decode against arbitrary bytes: it must never panic.
//!
//! `vaco-codec-jpegxl` is decode-only (`jxl-oxide` provides no encoder), so
//! there is no encode leg to round-trip through, unlike this batch's other
//! five image-codec fuzz targets. Panic-freedom on arbitrary input is the
//! whole property this target checks.
//!
//! fuzz-crate: vaco-codec-jpegxl
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_limits::{Budget, Limits};

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::strict());
    let _ = vaco_codec_jpegxl::decode(data, &mut budget);
});
