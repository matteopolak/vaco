//! HLS master and media playlist parsing over arbitrary bytes, plus a full
//! `HlsDemuxer::open` for whichever kind the input happens to be.
//!
//! This is the highest-value target in the crate: an M3U8 playlist is text
//! straight off the network, parsed by a hand-written tokenizer
//! (`attrs::parse_attribute_list`, `master::parse`, `media::parse`) before
//! anything else touches it. What is asserted beyond "does not panic":
//!
//! * Every accessor on a successfully parsed [`vaco_demux_hls::MasterPlaylist`]/
//!   [`vaco_demux_hls::MediaPlaylist`] is reachable without panicking.
//! * `HlsDemuxer::open` over the same bytes, with `NoSegmentDemuxers` and no
//!   protocol access, either succeeds (parses) or fails cleanly — never
//!   hangs or panics — matching the crate's documented degraded-mode
//!   behaviour for a demuxer with nowhere to fetch a segment from.
//! * A media playlist's segment count cannot run away from the input size:
//!   each segment needs at least one `#EXTINF:` tag, so the number of
//!   segments is bounded by the number of newlines in the input.
//! fuzz-crate: vaco-demux-hls

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_demux_hls::{HlsDemuxer, HlsOptions, master, media};
use vaco_format_adaptive::NoSegmentDemuxers;
use vaco_format_core::discovery::NoParsers;
use vaco_io::MemorySource;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    let line_count = text.lines().count() as u64;

    if let Ok(playlist) = master::parse(text, "http://example.invalid/master.m3u8") {
        for v in &playlist.variants {
            let _ = (v.bandwidth, v.width, v.height, &v.codecs, &v.uri);
        }
        for r in &playlist.renditions {
            let _ = (&r.group_id, &r.language, r.is_default);
        }
    }

    if let Ok(playlist) = media::parse(text, "http://example.invalid/media.m3u8") {
        assert!(
            playlist.segments.len() as u64 <= line_count.saturating_add(1),
            "{} segments from {line_count} lines of input",
            playlist.segments.len()
        );
        for s in &playlist.segments {
            let _ = (&s.uri, s.duration, s.byte_range, s.discontinuity);
        }
        let _ = playlist.is_live();
    }

    let src = Box::new(MemorySource::new(data.to_vec()));
    let _ = HlsDemuxer::open(
        src,
        "http://example.invalid/top.m3u8",
        None,
        Box::new(NoParsers),
        Box::new(NoSegmentDemuxers),
        &HlsOptions::default(),
    );
});
