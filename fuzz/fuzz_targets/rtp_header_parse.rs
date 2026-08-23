//! `RtpPacket::parse` against arbitrary bytes.
//!
//! Every field of an RTP header is attacker-controlled — a compromised RTSP
//! server or a spoofed UDP source chooses every byte. Properties: the
//! parser never panics, a successfully parsed packet's `payload` (plus
//! `csrc` and any extension) never extends past the input buffer, and
//! `header.version` is always exactly 2 when parsing succeeds (the one
//! thing this module refuses unconditionally). A packet claiming more CSRCs
//! or extension words than the buffer holds must be rejected, never
//! indexed into.
//! fuzz-crate: vaco-format-rtp

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_format_rtp::RtpPacket;

fuzz_target!(|data: &[u8]| {
    let Ok(parsed) = RtpPacket::parse(data) else {
        return;
    };
    assert_eq!(parsed.header.version, 2, "parse accepted a non-2 version");

    let base = data.as_ptr() as usize;
    let end = base + data.len();

    let csrc_ptr = parsed.csrc.as_ptr() as usize;
    assert!(csrc_ptr >= base && csrc_ptr + parsed.csrc.len() <= end, "csrc escaped the input");

    if let Some(ext) = parsed.extension {
        let ext_ptr = ext.data.as_ptr() as usize;
        assert!(ext_ptr >= base && ext_ptr + ext.data.len() <= end, "extension escaped the input");
    }

    let payload_ptr = parsed.payload.as_ptr() as usize;
    assert!(
        payload_ptr >= base && payload_ptr + parsed.payload.len() <= end,
        "payload escaped the input"
    );
});
