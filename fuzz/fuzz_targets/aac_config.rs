//! `vaco-codec-aac`'s configuration layer against arbitrary bytes:
//! `Decoder::set_extradata` (`AudioSpecificConfig::parse`, reused from
//! `vaco-parse-aac`), `Decoder::send_packet` (a leading `AdtsHeader` parse,
//! then — for `channelConfiguration == 0` — `find_leading_program_config_element`
//! and the new `program_config_element()` reader), and `Decoder::flush`.
//!
//! `AacDecoder` never decodes spectral data yet (#444/#445), so there is no
//! produced `Frame` to check for `NaN`/`inf` the way `vaco-codec-mpegaudio`'s
//! own fuzz target does — every real input either errors during
//! configuration or errors at the disclosed "spectral decode not
//! implemented" boundary. What this target asserts is simply what every
//! parser in this workspace is held to on untrusted input: no panic, no
//! hang. The first half of the input seeds `set_extradata` (exercising the
//! `AudioSpecificConfig`/out-of-band path); the second half is the packet
//! payload (exercising the raw-ADTS path, and — when a payload's
//! `channelConfiguration` is 0 — the new `program_config_element()` reader).
//!
//! fuzz-crate: vaco-codec-aac

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_aac::AacDecoder;
use vaco_codec_core::Decoder;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::strict());
    let mut dec = AacDecoder::new(Limits::strict());

    let split = data.len() / 2;
    let (extradata, payload) = data.split_at(split);

    // Errors here are expected and frequent (most byte strings are not a
    // valid `AudioSpecificConfig`) and intentionally not treated as fatal —
    // `set_extradata`'s own contract says a caller offering extradata should
    // treat a rejection as "this record told me nothing", not a reason to
    // stop.
    let _ = dec.set_extradata(extradata);

    if let Ok(packet) = Packet::from_slice(&mut budget, payload) {
        let _ = dec.send_packet(Some(&packet));
        let _ = dec.send_packet(None);
    }

    dec.flush();
});
