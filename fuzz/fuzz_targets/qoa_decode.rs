//! QOA frame decode against arbitrary bytes: it must never panic regardless
//! of how the (attacker-controlled) frame header, LMS state or slice bytes
//! are malformed, and a successful decode's own `encode` must itself
//! decode back to something of the same shape (channel count preserved, no
//! panic on the round trip either) — the same "decode -> encode -> decode
//! must survive" shape `parse_qoi`'s target uses.
//!
//! fuzz-crate: vaco-codec-simple-audio
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_simple_audio::qoa::{self, LmsState};
use vaco_limits::{Budget, Limits};

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::strict());
    let Ok(frame) = qoa::decode(&mut budget, data) else {
        return;
    };

    if frame.num_channels == 0 || frame.samples_per_channel == 0 {
        return;
    }
    let ch = frame.num_channels as usize;
    if ch > 64 {
        // vaco_limits::Limits::strict caps channels well below this; a
        // decode that got past `decode` with more is itself the bug to
        // find, not something to re-encode.
        return;
    }

    let mut states = vec![LmsState::default(); ch];
    let mut budget2 = Budget::new(Limits::strict());
    let Ok(encoded) = qoa::encode(&mut budget2, &mut states, frame.num_channels, frame.sample_rate, &frame.interleaved)
    else {
        return;
    };

    let mut budget3 = Budget::new(Limits::strict());
    let redecoded =
        qoa::decode(&mut budget3, &encoded).expect("re-encoding a decoded frame must itself decode");
    assert_eq!(redecoded.num_channels, frame.num_channels);
});
