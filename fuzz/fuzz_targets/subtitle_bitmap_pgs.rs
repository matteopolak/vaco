//! Arbitrary bytes walked as a `"PG"`-framed segment stream
//! (`vaco-subtitle-bitmap::sup::iter_segments`, the same framing this
//! format's real demuxer hands out), every segment record pushed through
//! `PgsDecoder::push_segment` -- PCS/PDS/ODS parsing, multi-segment object
//! reassembly and the object run-length grammar all read attacker-controlled
//! lengths before any bound is known to be safe.
//!
//! fuzz-crate: vaco-codec-subtitle-bitmap

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_subtitle_bitmap::pgs::PgsDecoder;
use vaco_limits::Limits;
use vaco_subtitle_bitmap::sup;

fuzz_target!(|data: &[u8]| {
    let limits = Limits::permissive();
    let mut decoder = PgsDecoder::new();
    for (_, record) in sup::iter_segments(data) {
        if decoder.push_segment(record, &limits).is_err() {
            break;
        }
    }
});
