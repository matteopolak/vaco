//! RFC 3389 comfort-noise SID payload parse and noise generation against
//! arbitrary bytes: parsing an untrusted network payload, and generating
//! audio from whatever it parsed to, must never panic — a SID payload's
//! reflection-coefficient count is attacker-controlled network input, and
//! [`comfortnoise::MAX_MODEL_ORDER`] is exactly the bound this target
//! checks is actually enforced.
//!
//! fuzz-crate: vaco-codec-simple-audio
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_simple_audio::comfortnoise::{self, Generator};
use vaco_limits::{Budget, Limits};

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::strict());
    let Ok(sid) = comfortnoise::parse(&mut budget, data) else {
        return;
    };
    assert!(
        sid.reflection.len() <= comfortnoise::MAX_MODEL_ORDER,
        "parse did not enforce its own documented model-order bound"
    );

    let mut budget2 = Budget::new(Limits::strict());
    let mut generator = Generator::new(0x9E37_79B9);
    let _ = generator.generate(&mut budget2, &sid, 320);
});
