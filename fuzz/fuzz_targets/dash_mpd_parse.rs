//! DASH MPD parsing over arbitrary bytes: the XML tree pass
//! (`tree::parse`), the semantic interpretation (`mpd::interpret`), and
//! segment enumeration (`segments::enumerate`) for every representation the
//! document names.
//!
//! The specific finding this target is built to catch, named in the brief:
//! a `SegmentTimeline`'s `<S r="…">` with a huge or negative repeat count is
//! a handful of bytes of XML that could otherwise ask for an unbounded
//! number of segments. `vaco_format_adaptive::timeline::expand` bounds this
//! (`MAX_SEGMENTS`, and the caller's fuel budget) and `tree::parse` bounds
//! the node count separately (`tree::MAX_NODES`) — this target asserts both
//! bounds hold for genuinely arbitrary input, not just the hand-picked cases
//! the unit tests cover.
//! fuzz-crate: vaco-demux-dash

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_demux_dash::{mpd, segments, tree};
use vaco_limits::{Budget, Limits};

/// Segments a single fuzz iteration may produce in total, across every
/// representation — generous above any real manifest, and the assertion
/// that would catch `tree::MAX_NODES`/`timeline::MAX_SEGMENTS` being lost.
const MAX_TOTAL_SEGMENTS: usize = 1 << 21;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };

    // A fresh, generous budget per iteration: this target is checking the
    // *structural* bounds (node count, segment count), not the allocator
    // budget's own accounting, which `limit_budget` already fuzzes.
    let mut budget = Budget::new(Limits::permissive());
    let Ok(root) = tree::parse(text, &mut budget) else {
        return;
    };
    let Ok(parsed) = mpd::interpret(&root) else {
        return;
    };

    let mut total_segments = 0usize;
    for period in &parsed.periods {
        let period_end_seconds = period
            .duration
            .or(parsed.media_presentation_duration)
            .map(|d| d.as_micros() as f64 / 1_000_000.0);
        for aset in &period.adaptation_sets {
            for rep in &aset.representations {
                let mut b = Budget::new(Limits::permissive());
                if let Ok((_, segs)) = segments::enumerate(rep, "http://example.invalid/", period_end_seconds, &mut b) {
                    total_segments = total_segments.saturating_add(segs.len());
                    assert!(
                        total_segments <= MAX_TOTAL_SEGMENTS,
                        "{total_segments} segments from one MPD — a timeline bound was lost"
                    );
                    for s in &segs {
                        let _ = (&s.uri, s.duration, s.byte_range);
                    }
                }
            }
        }
    }
});
