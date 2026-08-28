//! fuzz-crate: vaco-protocol-rtp
//!
//! Whole-buffer classification/parsing for `#551`'s RFC 5761 mux/demux:
//! [`vaco_protocol_rtp::demux`] must never panic on arbitrary bytes,
//! whichever of RTP, RTCP or "too short to classify" it decides a buffer
//! is.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_protocol_rtp::demux;

fuzz_target!(|data: &[u8]| {
    let _ = demux(data);
});
