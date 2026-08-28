//! `vaco-codec-aac`'s entire pipeline against arbitrary bytes, through its
//! public `Decoder` API: `Decoder::set_extradata`
//! (`AudioSpecificConfig::parse`, reused from `vaco-parse-aac`),
//! `Decoder::send_packet` — which drives the *full* `raw_data_block()`
//! parse (`ics_info`, `section_data`, `scale_factor_data`'s three DPCM
//! chains, `pulse_data`, `tns_data`'s syntax, and `spectral_data`'s eleven
//! Huffman codebooks including the ESC codebook's variable-length escape
//! sequences) and then, since #445, reconstruction on whatever that parse
//! produced: inverse quantisation, PNS, joint stereo, TNS *application*,
//! and the IMDCT/windowing/overlap-add filterbank — followed by
//! `Decoder::receive_frame` to drain whatever `send_packet` queued, and
//! `Decoder::flush`.
//!
//! An attacker controls every scalefactor, TNS coefficient and spectral
//! magnitude reaching the IMDCT, so this target's job past #445 is not
//! just "no panic, no hang" on the parse (`section_data`'s
//! escape-accumulation loop and `spectral_data`'s ESC-codebook escape read
//! are the shapes that produce a libFuzzer `slow-unit-` rather than a
//! crash if a bound is missing — both are bounded, see `section.rs`'s loop
//! and `spectral.rs::read_escape`'s `n > 20` bailout) but also that
//! reconstruction itself cannot be driven into a hang or a panic (a
//! division, a `sqrt` of a negative, an unbounded loop keyed off untrusted
//! `max_sfb`/`window_group_length`) even though there is no decoded-output
//! shape to assert on the way `vaco-codec-mpegaudio`'s own fuzz target
//! checks `NaN`/`inf` — AAC's own compliance tolerance means a garbage
//! bitstream is entitled to garbage (but finite, terminating) samples. The
//! first half of the input seeds `set_extradata` (the
//! `AudioSpecificConfig`/out-of-band path); the second half is the packet
//! payload (the raw-ADTS path, and — when a payload's
//! `channelConfiguration` is 0 — the `program_config_element()` reader).
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
        // Drain whatever `send_packet` queued — since #445 this runs
        // reconstruction (IMDCT, TNS application, overlap-add) on
        // attacker-controlled spectral data, not just the syntax parse.
        while dec.receive_frame().is_ok() {}
        let _ = dec.send_packet(None);
        while dec.receive_frame().is_ok() {}
    }

    dec.flush();
});
