//! `MpegAudioHeader`, `XingHeader`, `LameTag` and `VbriHeader` over arbitrary
//! bytes: none of these take a budget (they only read fixed-size fields out
//! of a caller-supplied slice, never allocate), so the only property to
//! check is the universal one — no panic, on any length including zero.
//!
//! fuzz-crate: vaco-format-mpegaudio

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_format_mpegaudio::{LameTag, MpegAudioHeader, VbriHeader, XingHeader};

fuzz_target!(|data: &[u8]| {
    if let Some(chunk) = data.first_chunk::<4>() {
        let word = u32::from_be_bytes(*chunk);
        if let Some(header) = MpegAudioHeader::parse(word) {
            let _ = header.frame_len();
            let _ = header.side_info_len();
            let _ = header.to_bytes();
        }
    }
    let _ = MpegAudioHeader::parse_bytes(data);
    let _ = XingHeader::parse(data);
    let _ = LameTag::parse(data);
    let _ = VbriHeader::parse(data);
});
