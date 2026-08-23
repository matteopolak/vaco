//! Property tests for the page/packet layer and the granule timeline.
//!
//! These target the two pieces of arithmetic in this crate that take
//! attacker-controlled or otherwise arbitrary numeric input and must stay
//! total: lacing (`page::packet_spans`, driven from a 255-byte segment
//! table an Ogg page hands us verbatim) and the timestamp assignment engine
//! (`granule::GranuleTimeline::assign`, driven from a page's granule
//! position, a 64-bit field with no range restriction beyond the reserved
//! `-1`).

use proptest::prelude::*;
use vaco_demux_ogg::granule::{GranuleMapping, GranuleTimeline};
use vaco_demux_ogg::page::{self, PacketSpan};

proptest! {
    /// `packet_spans` never panics on any byte sequence, and its ranges are
    /// contiguous, non-overlapping, and cover exactly the sum of the table
    /// (lacing values are a `body_len` computation the same crate already
    /// trusts elsewhere, so agreement here is the invariant that matters).
    #[test]
    fn packet_spans_are_contiguous_and_cover_the_declared_body(
        table in prop::collection::vec(any::<u8>(), 0..=255)
    ) {
        let spans = page::packet_spans(&table);
        let declared: usize = table.iter().map(|&b| b as usize).sum();
        let mut expect_start = 0usize;
        for (i, s) in spans.iter().enumerate() {
            prop_assert_eq!(s.start, expect_start);
            prop_assert!(s.end >= s.start);
            expect_start = s.end;
            // Only the very last span may be incomplete, and only when the
            // table's own last byte is 255.
            if !s.complete {
                prop_assert_eq!(i, spans.len() - 1);
                prop_assert_eq!(table.last().copied(), Some(255));
            }
        }
        if let Some(last) = spans.last() {
            prop_assert_eq!(last.end, declared);
        } else {
            prop_assert_eq!(declared, 0);
        }
    }

    /// At most one span is incomplete, and only ever the last one — the
    /// property that makes "an incomplete span means continuation" a safe
    /// thing for `demux.rs` to assume without scanning the whole list.
    #[test]
    fn at_most_the_last_span_is_incomplete(
        table in prop::collection::vec(any::<u8>(), 0..=255)
    ) {
        let spans = page::packet_spans(&table);
        let incomplete = spans.iter().filter(|s: &&PacketSpan| !s.complete).count();
        prop_assert!(incomplete <= 1);
    }

    /// `GranuleTimeline::assign` never produces a negative duration and
    /// never produces fewer or more `(pts, duration)` pairs than it was
    /// given nominal durations for, whatever nonsense the nominal durations
    /// or the granule position are.
    #[test]
    fn assign_is_total_and_never_produces_a_negative_duration(
        nominal in prop::collection::vec(-1_000_000i64..1_000_000i64, 0..32),
        granule in prop::num::i64::ANY,
        mapping_kind in 0u8..5,
    ) {
        let mapping = match mapping_kind {
            0 => GranuleMapping::SampleCount,
            1 => GranuleMapping::Opus { pre_skip: 312 },
            2 => GranuleMapping::Vorbis { nominal: 1024 },
            3 => GranuleMapping::Speex { samples_per_packet: 320 },
            _ => GranuleMapping::Theora { granule_shift: 6 },
        };
        let mut tl = GranuleTimeline::new();
        let out = tl.assign(&mapping, granule, &nominal);
        prop_assert_eq!(out.len(), nominal.len());
        for (_, dur) in &out {
            prop_assert!(*dur >= 0);
        }
    }

    /// A page whose granule is genuinely unset (`-1`) is not snapped at
    /// all: the cursor after it is exactly the running sum of the nominal
    /// durations it was given, whatever they are — the snap must never
    /// fire on the one value RFC 3533 reserves to mean "nothing to snap to".
    #[test]
    fn an_unset_granule_never_triggers_a_snap(
        nominal in prop::collection::vec(0i64..100_000, 0..16),
        mapping_kind in 0u8..5,
    ) {
        let mapping = match mapping_kind {
            0 => GranuleMapping::SampleCount,
            1 => GranuleMapping::Opus { pre_skip: 312 },
            2 => GranuleMapping::Vorbis { nominal: 1024 },
            3 => GranuleMapping::Speex { samples_per_packet: 320 },
            _ => GranuleMapping::Theora { granule_shift: 6 },
        };
        let mut tl = GranuleTimeline::new();
        let start = tl.planned_cursor(&mapping);
        let _ = tl.assign(&mapping, page::GRANULE_UNSET, &nominal);
        let expected: i64 = start + nominal.iter().sum::<i64>();
        prop_assert_eq!(tl.cursor(), expected);
    }
}
