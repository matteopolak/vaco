//! `vaco_protocol_rtmp::control::ControlMessage::decode` on an arbitrary
//! message type ID and payload.
//!
//! Round-tripping through `encode_payload`/`decode` is checked for value
//! equality, not just for panic-freedom: a decoder and re-encoder that agree
//! on a *wrong* field (e.g. swapped bytes) would still pass a check that
//! only asked "did decoding the re-encoded bytes succeed."
//! fuzz-crate: vaco-protocol-rtmp
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_protocol_rtmp::control::ControlMessage;

fuzz_target!(|data: &[u8]| {
    let Some((&type_id, payload)) = data.split_first() else {
        return;
    };
    if let Ok(Some(msg)) = ControlMessage::decode(type_id, payload) {
        let re_encoded = msg.encode_payload();
        match ControlMessage::decode(msg.message_type_id(), &re_encoded) {
            Ok(Some(redecoded)) => {
                assert_eq!(
                    redecoded, msg,
                    "encode_payload -> decode did not round-trip the message"
                );
            }
            other => panic!(
                "re-decoding our own encode_payload output failed or was empty: {other:?}"
            ),
        }
    }
});
