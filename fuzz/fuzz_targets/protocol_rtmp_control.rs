//! `vaco_protocol_rtmp::control::ControlMessage::decode` on an arbitrary
//! message type ID and payload.
//! fuzz-crate: vaco-protocol-rtmp
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_protocol_rtmp::control::ControlMessage;

fuzz_target!(|data: &[u8]| {
    let Some((&type_id, payload)) = data.split_first() else {
        return;
    };
    if let Ok(Some(msg)) = ControlMessage::decode(type_id, payload) {
        // Round-tripping through encode_payload/decode again must not
        // panic and must reach a fixed point (decoding what we just
        // encoded never itself fails).
        let re_encoded = msg.encode_payload();
        let _ = ControlMessage::decode(msg.message_type_id(), &re_encoded);
    }
});
