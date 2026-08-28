//! fuzz-crate: vaco-protocol-srt
//!
//! Whole-buffer parsing over arbitrary bytes for every parser this crate
//! exposes ahead of any socket: [`vaco_protocol_srt::packet::SrtPacket`]
//! (the common header dispatch), [`vaco_protocol_srt::handshake::HandshakeCif`]
//! plus its extension walker, and [`vaco_protocol_srt::km::KmMessage`].
//!
//! Every one of these three has an attacker-controlled length field that
//! could run past the end of the buffer if a bounds check were missed —
//! `handshake::parse_extensions`'s per-extension `Extension Length`,
//! `km::KmMessage`'s `SLen`, and `packet`'s own declared-vs-actual data
//! length. Property: none of them ever panics, and none of them ever
//! returns a slice or `Vec` claiming more bytes than the input actually
//! had (checked implicitly — a bounds violation there would itself be a
//! panic on the immediately following `.to_vec()`/`.get()`, not a silent
//! wrong answer, since every read in this crate goes through
//! `slice::get`).
//!
//! This is issue #555's own named Acc criterion ("the fuzz target on
//! packet parsing is green for 24h") — built alongside the framing layer
//! rather than after it, and not yet run anywhere near that long in this
//! session (see the commit/PR this target lands in for how long it has
//! actually run).

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_limits::{Budget, Limits};
use vaco_protocol_srt::handshake::HandshakeCif;
use vaco_protocol_srt::km::KmMessage;
use vaco_protocol_srt::packet::SrtPacket;

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::strict());
    if let Ok(pkt) = SrtPacket::parse(data, &mut budget) {
        // Re-serializing and re-parsing must not panic either, and a
        // successfully-parsed packet's own serialize() must not exceed a
        // sane multiple of the input (guards against a length field being
        // echoed into an unbounded allocation).
        let bytes = pkt.serialize();
        assert!(bytes.len() <= data.len().saturating_add(4096));
        let mut budget2 = Budget::new(Limits::strict());
        let _ = SrtPacket::parse(&bytes, &mut budget2);
    }

    if let Ok((cif, consumed)) = HandshakeCif::parse(data) {
        assert!(consumed <= data.len());
        if let Some(rest) = data.get(consumed..) {
            let _ = vaco_protocol_srt::handshake::parse_extensions(rest);
        }
        let _ = cif.serialize();
    }

    if let Ok(km) = KmMessage::parse(data) {
        let bytes = km.serialize();
        assert!(bytes.len() <= data.len().saturating_add(64));
    }
});
