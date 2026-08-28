//! fuzz-crate: vaco-protocol-sctp
//!
//! Whole-buffer parsing for `#561`'s framing layer:
//! [`vaco_protocol_sctp::packet::CommonHeader::parse`] and
//! [`vaco_protocol_sctp::chunk::parse_one`] (looped over the packet's
//! whole chunk area, the same way [`vaco_protocol_sctp::association::
//! Association::on_packet`] does) must never panic on arbitrary bytes.
//! Also drives a live `Association` (server role) with the raw bytes, on
//! the theory that a fuzzer-discovered input reaching deep into the
//! handshake state machine is worth more than one that only ever hits
//! `Closed`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_protocol_sctp::association::Association;
use vaco_protocol_sctp::chunk;
use vaco_protocol_sctp::packet::CommonHeader;

fuzz_target!(|data: &[u8]| {
    if let Ok(header) = CommonHeader::parse(data) {
        let mut cursor = vaco_protocol_sctp::packet::COMMON_HEADER_LEN;
        let mut guard = 0;
        while cursor < data.len() && guard < 64 {
            let Some(rest) = data.get(cursor..) else { break };
            let Ok((_chunk, consumed)) = chunk::parse_one(rest) else { break };
            if consumed == 0 {
                break;
            }
            cursor += consumed;
            guard += 1;
        }
        let _ = header;
    }

    let mut server = Association::new_server(1, 2, 0x1234_5678, 1000);
    let _ = server.on_packet(data);
});
