//! `vaco-codec-aac`'s entire syntax layer against arbitrary bytes, through
//! its public `Decoder` API: `Decoder::set_extradata`
//! (`AudioSpecificConfig::parse`, reused from `vaco-parse-aac`),
//! `Decoder::send_packet` — which as of #444 drives the *full*
//! `raw_data_block()` parse (`ics_info`, `section_data`,
//! `scale_factor_data`'s three DPCM chains, `pulse_data`, `tns_data`'s
//! syntax, and `spectral_data`'s eleven Huffman codebooks including the
//! ESC codebook's variable-length escape sequences) before returning
//! `Error::Unsupported` at the reconstruction boundary — and
//! `Decoder::flush`.
//!
//! `AacDecoder` never produces a `Frame` (#445 is reconstruction), so there
//! is no decoded output to check for `NaN`/`inf` the way
//! `vaco-codec-mpegaudio`'s own fuzz target does. What this target asserts
//! is what every parser in this workspace is held to on untrusted input: no
//! panic, no hang — the last of which matters more here than in most of
//! this crate's siblings, since `section_data`'s escape-accumulation loop
//! and `spectral_data`'s ESC-codebook escape-sequence read are exactly the
//! shape that produces a libFuzzer `slow-unit-` rather than a crash if a
//! bound is missing (both are bounded — see `section.rs`'s loop and
//! `spectral.rs::read_escape`'s `n > 20` bailout — but a fuzz run is the
//! check that bound is real, not merely believed to be). The first half of
//! the input seeds `set_extradata` (the `AudioSpecificConfig`/out-of-band
//! path); the second half is the packet payload (the raw-ADTS path, and —
//! when a payload's `channelConfiguration` is 0 — the
//! `program_config_element()` reader).
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
