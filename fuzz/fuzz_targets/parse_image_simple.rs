//! Every `vaco-codec-image-simple` decoder against arbitrary bytes.
//!
//! fuzz-crate: vaco-codec-image-simple
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_limits::{Budget, Limits};

fuzz_target!(|data: &[u8]| {
    for decode in [
        vaco_codec_image_simple::decode_bmp,
        vaco_codec_image_simple::decode_pcx,
        vaco_codec_image_simple::decode_tga,
        vaco_codec_image_simple::decode_sgi,
        vaco_codec_image_simple::decode_xwd,
        vaco_codec_image_simple::decode_xbm,
    ] {
        let mut budget = Budget::new(Limits::strict());
        let _ = decode(data, &mut budget);
    }
});
