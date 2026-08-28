//! fuzz-crate: vaco-protocol-rtmp
//!
//! Whole-buffer parsing for `#553`'s AMF0/command layer:
//! [`vaco_protocol_rtmp::amf0::decode`] and
//! [`vaco_protocol_rtmp::command::Command::decode`] must never panic on
//! arbitrary bytes. Property: decoding then re-encoding reaches a fixed
//! point — encoding the re-decoded value reproduces the same bytes.
//!
//! Compares **bytes**, not `Command`/`Value` equality: an arbitrary
//! 8-byte AMF0 Number can decode to a NaN `f64`, and `f64`'s `PartialEq`
//! is IEEE-754-correct (`NaN != NaN`), so `assert_eq!(re_decoded, cmd)`
//! is a false crash on a NaN payload even though the byte-level
//! round-trip is perfect — found by this exact fuzz target on its first
//! real run (input `[2,0,0,0,0,0,0,0,0,2,61,0,1,0,0,255,255,18,0,236,255,255,255]`,
//! whose trailing 8 bytes are an AMF0 Number with all-1s exponent bits, a
//! NaN bit pattern) before being fixed to this byte-comparison shape.
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_protocol_rtmp::amf0;
use vaco_protocol_rtmp::command::Command;

fuzz_target!(|data: &[u8]| {
    let _ = amf0::decode(data);

    if let Ok(cmd) = Command::decode(data) {
        let re_encoded = cmd.encode();
        if let Ok(re_decoded) = Command::decode(&re_encoded) {
            assert_eq!(re_decoded.encode(), re_encoded);
        }
    }
});
