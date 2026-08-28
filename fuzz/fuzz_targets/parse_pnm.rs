//! Every PNM-family decoder against arbitrary bytes, each format on the same
//! input since a header's first two bytes decide which one even looks at it.
//!
//! fuzz-crate: vaco-codec-pnm
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_limits::{Budget, Limits};

fuzz_target!(|data: &[u8]| {
    for decode in [
        vaco_codec_pnm::decode_pbm,
        vaco_codec_pnm::decode_pgm,
        vaco_codec_pnm::decode_ppm,
        vaco_codec_pnm::decode_pam,
        vaco_codec_pnm::decode_pfm,
        vaco_codec_pnm::decode_phm,
    ] {
        let mut budget = Budget::new(Limits::strict());
        let _ = decode(data, &mut budget);
    }
});
