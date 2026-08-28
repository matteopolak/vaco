//! Arbitrary bytes as a complete DVB display-set epoch, through
//! `dvb::decode_display_set` -- the segment walker, the full region/CLUT/
//! object-data parsing this crate adds beyond
//! `vaco-subtitle-bitmap::dvbsub::segments`, and the 2-/4-/8-bit pixel-code
//! run-length grammars, all of which read attacker-controlled lengths
//! (region/object width and height, run lengths, CLUT entry counts) before
//! any bound is known to be safe.
//!
//! fuzz-crate: vaco-codec-subtitle-bitmap

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_subtitle_bitmap::dvb;
use vaco_limits::Limits;

fuzz_target!(|data: &[u8]| {
    let _ = dvb::decode_display_set(data, &Limits::permissive());
});
