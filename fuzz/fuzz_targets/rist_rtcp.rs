//! fuzz-crate: vaco-protocol-rist
//!
//! Whole-buffer parsing over arbitrary bytes for every RIST-specific RTCP
//! parser this crate exposes:
//! [`vaco_protocol_rist::rtcp::RttEcho::parse`],
//! [`vaco_protocol_rist::rtcp::GenericNack::parse`], and
//! [`vaco_protocol_rist::rtcp::RangeNack::parse`] — each fed the same
//! arbitrary bytes both as an arbitrary `count_or_fmt` (first byte) plus
//! body (the rest), the same split `vaco_rtp::rtcp::parse_one` hands a
//! caller for any payload type it does not itself interpret. Also fuzzes
//! `vaco_rtp::rtcp::iter_compound` directly over the whole buffer, since a
//! RIST compound packet is exactly that iterator's own input shape.
//!
//! Every field here (SSRC, name, timestamp, NACK entry counts, range
//! lengths) is attacker-controlled — a hostile RIST sender chooses every
//! byte an RTCP packet arrives with. Property: none of these parsers ever
//! panics, and a successful parse's own `serialize()` round-trips.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_protocol_rist::rtcp::{GenericNack, RangeNack, RttEcho};

fuzz_target!(|data: &[u8]| {
    let Some((&count_or_fmt, body)) = data.split_first() else {
        return;
    };

    if let Ok(echo) = RttEcho::parse(count_or_fmt, body) {
        let (cof, out) = echo.serialize();
        let _ = RttEcho::parse(cof, &out);
    }

    if let Ok(nack) = GenericNack::parse(count_or_fmt, body) {
        let (cof, out) = nack.serialize();
        let reparsed = GenericNack::parse(cof, &out);
        assert!(reparsed.is_ok(), "a successfully parsed Generic NACK must re-parse after serialize()");
    }

    if let Ok(nack) = RangeNack::parse(count_or_fmt, body) {
        let (cof, out) = nack.serialize();
        let reparsed = RangeNack::parse(cof, &out);
        assert!(reparsed.is_ok(), "a successfully parsed range NACK must re-parse after serialize()");
    }

    // The compound-packet walk itself, over the whole buffer unmodified —
    // this is the shape `vaco-rtp` actually hands a RIST implementation a
    // packet in.
    for result in vaco_rtp::rtcp::iter_compound(data) {
        let _ = result;
    }
});
