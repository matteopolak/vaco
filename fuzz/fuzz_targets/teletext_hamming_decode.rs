//! Arbitrary bytes into both Hamming decoders (EN 300 706 §8.2/§8.3).
//!
//! Property: neither decoder ever panics, and [`Correction::Uncorrectable`]
//! is reported exactly when the by-construction round trip in
//! `vaco-codec-subtitle-teletext`'s own unit tests says a double-bit error
//! should be — this target does not re-derive that oracle, it only checks
//! the decoders survive attacker-controlled input, since every byte in a
//! Teletext packet passes through one of them.
//!
//! fuzz-crate: vaco-codec-subtitle-teletext

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_subtitle_teletext::hamming;

fuzz_target!(|data: &[u8]| {
    if let Some(&byte) = data.first() {
        let (_nibble, correction) = hamming::decode8(byte);
        let _ = correction.is_usable();
    }
    if let Some(triplet) = data.get(..3) {
        if let Ok(bytes) = <[u8; 3]>::try_from(triplet) {
            let (value, correction) = hamming::decode24(bytes);
            assert!(value <= 0x3_FFFF, "18-bit payload must fit 18 bits");
            let _ = correction.is_usable();
            let _ = hamming::triplet_address(value);
            let _ = hamming::triplet_mode(value);
            let _ = hamming::triplet_data(value);
        }
    }
});
