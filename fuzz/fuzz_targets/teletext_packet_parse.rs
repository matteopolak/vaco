//! Arbitrary bytes through the full EN 300 472 data-unit walk and
//! `TeletextDecoder` state machine.
//!
//! Property: `push` never panics and never grows unboundedly — the page
//! grid is a fixed `[[Cell; 40]; 25]` per magazine and there are at most
//! eight magazines, so nothing here should allocate more than a handful of
//! `Vec<PageEvent>` entries proportional to `data.len() / 46`.
//!
//! fuzz-crate: vaco-codec-subtitle-teletext

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_subtitle_teletext::TeletextDecoder;

fuzz_target!(|data: &[u8]| {
    let mut decoder = TeletextDecoder::new();
    // Split into a few pushes rather than one, to exercise the
    // cross-push carry buffer the same way a real demuxer's fixed-size
    // chunking would.
    for chunk in data.chunks(97) {
        let events = decoder.push(chunk);
        assert!(
            events.len() <= 8,
            "at most one page can finish per magazine per push"
        );
    }
    let _ = decoder.finish();
});
