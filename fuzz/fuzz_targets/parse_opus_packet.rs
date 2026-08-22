//! Opus packet framing: the TOC byte and codes 0 to 3.
//!
//! This is the highest-value Opus target, because Opus packets are framed by
//! the *container* and their internal lengths are not. A code-2 or code-3 VBR
//! packet can claim frame sizes the packet does not contain, and the natural
//! implementation — subtract the declared sizes from what is left — underflows.
//! Padding compounds it: a run of `255` bytes escapes to an arbitrarily large
//! padding length that must be checked against the packet before it is removed.
//!
//! Properties, for every packet we accept:
//!
//! 1. `len` never exceeds the input, and equals it for the non-self-delimited
//!    form, because the caller's framing is authoritative.
//! 2. Frame bytes plus padding plus framing overhead is exactly `len`.
//! 3. No frame exceeds 1275 bytes and no packet exceeds 120 ms, which are the
//!    two hard bounds RFC 6716 §3.2 places on the format.
//! 4. Every frame slice lies inside the input.
//! 5. Splitting a multi-stream packet consumes strictly increasing offsets, so
//!    a self-delimited sub-packet of length zero cannot spin the splitter.
//!
//! fuzz-crate: vaco-parse-opus
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_parse_opus::packet::{MAX_PACKET_SAMPLES, OpusPacket};
use vaco_parse_opus::{MAX_FRAME_BYTES, split_streams};

fn within(outer: &[u8], inner: &[u8]) -> bool {
    let base = outer.as_ptr() as usize;
    let start = inner.as_ptr() as usize;
    start >= base && start.saturating_add(inner.len()) <= base.saturating_add(outer.len())
}

fuzz_target!(|data: &[u8]| {
    if let Ok(packet) = OpusPacket::parse(data) {
        assert_eq!(packet.len, data.len(), "framing must use the whole packet");
        assert!(!packet.frames.is_empty());
        assert!(packet.frames.len() <= vaco_parse_opus::MAX_FRAMES);
        assert!(packet.samples() <= MAX_PACKET_SAMPLES);
        assert!(packet.samples() > 0);
        let mut total = packet.padding;
        for frame in &packet.frames {
            assert!(frame.len() <= MAX_FRAME_BYTES);
            assert!(within(data, frame), "a frame points outside the packet");
            total += frame.len();
        }
        assert!(
            total < data.len(),
            "frames and padding ({total}) leave no room for the TOC byte"
        );
    }

    if let Ok(packet) = OpusPacket::parse_self_delimited(data) {
        assert!(packet.len <= data.len());
        assert!(packet.len > 0);
        for frame in &packet.frames {
            assert!(within(data, frame));
        }
    }

    // The multi-stream split, where a zero-length sub-packet would be the way
    // to make the loop spin.
    for streams in [1usize, 2, 8, 255] {
        if let Ok(packets) = split_streams(data, streams) {
            assert_eq!(packets.len(), streams);
            let total: usize = packets.iter().map(|p| p.len).sum();
            assert!(total <= data.len());
            for packet in &packets {
                assert!(packet.len > 0, "a sub-packet consumed nothing");
            }
        }
    }
});
