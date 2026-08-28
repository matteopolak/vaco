//! fuzz-crate: vaco-protocol-rist
//!
//! Whole-buffer parsing over arbitrary bytes for `#559`'s GRE/tunnelling
//! parsers: [`vaco_protocol_rist::gre::GreHeader::parse`] (attacker-
//! controlled `C`/`K`/`S` flags choose whether the parser reads 4, 8, 12
//! or 16 bytes), [`vaco_protocol_rist::gre::VsfHeader::parse`],
//! [`vaco_protocol_rist::gre::ReducedUdpHeader::parse`], and
//! [`vaco_protocol_rist::keepalive::KeepAliveMessage::parse`] (whose
//! trailing JSON payload is arbitrary-length attacker-controlled bytes
//! this crate never interprets as JSON, only carries).
//!
//! Property: none of these parsers ever panics, and every successful
//! parse's own `serialize()` reproduces a buffer whose length is
//! consistent with what was actually consumed.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_protocol_rist::gre::{GreHeader, ReducedUdpHeader, VsfHeader};
use vaco_protocol_rist::keepalive::KeepAliveMessage;

fuzz_target!(|data: &[u8]| {
    if let Ok((header, consumed)) = GreHeader::parse(data) {
        assert!(consumed <= data.len());
        let bytes = header.serialize();
        assert_eq!(bytes.len(), consumed);
        let _ = GreHeader::parse(&bytes);
    }

    if let Ok((vsf, consumed)) = VsfHeader::parse(data) {
        assert_eq!(consumed, 4);
        assert_eq!(vsf.serialize().len(), 4);
    }

    if let Ok((udp, consumed)) = ReducedUdpHeader::parse(data) {
        assert_eq!(consumed, 4);
        assert_eq!(udp.serialize().len(), 4);
    }

    if let Ok(keepalive) = KeepAliveMessage::parse(data) {
        let bytes = keepalive.serialize();
        assert!(bytes.len() >= 8);
        let reparsed = KeepAliveMessage::parse(&bytes);
        assert!(reparsed.is_ok(), "a successfully parsed Keep-Alive message must re-parse after serialize()");
    }
});
