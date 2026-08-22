//! Property tests for the invariants the unit tests can only sample.
//!
//! Three of these are the crate's load-bearing claims and are stated here
//! rather than in a unit test because a hand-written example proves them for
//! one table and `proptest` proves them for a few hundred:
//!
//! * random access and the cursor produce identical samples;
//! * `sample_at_dts` is a left inverse of `dts`;
//! * the byte offset of a sample is its chunk's offset plus the sizes before it
//!   *in that chunk*, which is the definition the mapping is supposed to
//!   implement rather than an artefact of how it is implemented.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::integer_division,
    reason = "test code"
)]

use proptest::prelude::*;
use vaco_format_isom::build::{StblSpec, first_box, stbl};
use vaco_format_isom::edit::{EditList, rescale_media_to_movie, rescale_movie_to_media};
use vaco_format_isom::esds::{read_descriptor, write_expandable};
use vaco_format_isom::lang::Language;
use vaco_format_isom::table::{EntryTable, MAX_CHECKPOINTS, RunIndex, decimation_stride};
use vaco_format_isom::{SampleTable, boxes};

/// A well-formed chunk layout: strictly increasing `first_chunk` starting at 1,
/// with a positive samples-per-chunk.
fn stsc_runs(max_runs: usize) -> impl Strategy<Value = Vec<(u32, u32, u32)>> {
    prop::collection::vec((1u32..6, 1u32..4), 1..=max_runs).prop_map(|steps| {
        let mut first = 1u32;
        let mut out = Vec::new();
        for (gap, spc) in steps {
            out.push((first, spc, 1));
            first = first.saturating_add(gap);
        }
        out
    })
}

/// A table with a coherent set of tables: enough chunk offsets for every run,
/// enough sizes for every sample, and a `stts` covering them.
fn coherent_table() -> impl Strategy<Value = StblSpec> {
    (
        stsc_runs(6),
        prop::collection::vec(1u32..4000, 1..80),
        prop::collection::vec(1u32..500, 1..6),
    )
        .prop_map(|(stsc, sizes, deltas)| {
            // Chunks needed: the last run's first_chunk plus a few.
            let chunks = stsc.last().map_or(1, |r| r.0).saturating_add(20);
            let stco: Vec<u32> = (0..chunks)
                .map(|i| 4096u32.saturating_add(i * 100_000))
                .collect();
            let stts: Vec<(u32, u32)> = deltas
                .iter()
                .map(|d| {
                    (
                        (sizes.len() as u32).div_ceil(deltas.len() as u32).max(1),
                        *d,
                    )
                })
                .collect();
            let stss: Vec<u32> = (0..sizes.len() as u32).step_by(7).map(|i| i + 1).collect();
            StblSpec {
                stts,
                stsc,
                stsz: sizes,
                stco,
                stss,
                ..StblSpec::default()
            }
        })
}

proptest! {
    /// The crate's central invariant. Two entirely different code paths — a
    /// binary search over decimated summaries, and a carried running position
    /// — must produce byte-identical answers, because a seek uses the first
    /// and a demux loop uses the second.
    #[test]
    fn random_access_and_the_cursor_never_disagree(spec in coherent_table()) {
        let raw = stbl(&spec);
        let b = first_box(&raw);
        let table = SampleTable::parse(&b).unwrap();
        let mut cursor = table.cursor();
        for i in 0..table.sample_count() {
            prop_assert_eq!(table.sample(i), cursor.next(), "sample {}", i);
        }
        prop_assert_eq!(cursor.next(), None);
    }

    /// A cursor positioned at `n` must equal one walked to `n`.
    #[test]
    fn a_positioned_cursor_equals_a_walked_one(spec in coherent_table(), at in 0usize..40) {
        let raw = stbl(&spec);
        let b = first_box(&raw);
        let table = SampleTable::parse(&b).unwrap();
        let n = table.sample_count();
        prop_assume!(n > 0);
        let at = at.min(n as usize - 1);
        let walked: Vec<_> = table.cursor().skip(at).take(4).collect();
        let jumped: Vec<_> = table.cursor_at(at as u32).take(4).collect();
        prop_assert_eq!(walked, jumped);
    }

    /// The byte offset must be exactly the chunk's offset plus the sizes of the
    /// samples before it *within that chunk* — the definition, computed the
    /// slow way and compared against the fast one.
    #[test]
    fn offsets_are_the_chunk_base_plus_the_within_chunk_prefix(spec in coherent_table()) {
        let raw = stbl(&spec);
        let b = first_box(&raw);
        let table = SampleTable::parse(&b).unwrap();
        let mut expected: Option<u64> = None;
        let mut previous_chunk = 0u32;
        for i in 0..table.sample_count() {
            let Some(s) = table.sample(i) else { continue };
            if s.chunk != previous_chunk {
                expected = Some(s.offset);
                previous_chunk = s.chunk;
            }
            prop_assert_eq!(Some(s.offset), expected, "sample {}", i);
            expected = expected.map(|e| e + u64::from(s.size));
        }
    }

    /// `sample_at_dts` is a left inverse of `dts` whenever the deltas are
    /// positive. With a zero delta several samples share a time and only the
    /// "at or before" contract survives, which the next property covers.
    #[test]
    fn dts_lookup_inverts_dts(spec in coherent_table()) {
        let raw = stbl(&spec);
        let b = first_box(&raw);
        let table = SampleTable::parse(&b).unwrap();
        for i in 0..table.sample_count() {
            let ts = table.dts(i);
            let found = table.sample_at_dts(ts).unwrap();
            prop_assert_eq!(table.dts(found), ts, "sample {} at dts {}", i, ts);
        }
    }

    /// Whatever the table, `sample_at_dts` never overshoots.
    #[test]
    fn dts_lookup_never_overshoots(
        spec in coherent_table(),
        probe in prop::num::i64::ANY,
    ) {
        let raw = stbl(&spec);
        let b = first_box(&raw);
        let table = SampleTable::parse(&b).unwrap();
        if let Some(n) = table.sample_at_dts(probe) {
            prop_assert!(table.dts(n) <= probe);
            prop_assert!(n < table.sample_count().max(1));
        }
    }

    /// Sync-sample queries bracket their argument, always.
    #[test]
    fn sync_queries_are_ordered(spec in coherent_table(), n in prop::num::u32::ANY) {
        let raw = stbl(&spec);
        let b = first_box(&raw);
        let table = SampleTable::parse(&b).unwrap();
        if let Some(before) = table.sync_at_or_before(n) {
            prop_assert!(before <= n);
            prop_assert!(table.is_sync(before));
        }
        if let Some(after) = table.sync_at_or_after(n) {
            prop_assert!(after >= n);
            prop_assert!(table.is_sync(after));
        }
    }

    /// Arbitrary bytes must never make a table lie about itself or hang.
    #[test]
    fn an_arbitrary_stbl_stays_total(data in prop::collection::vec(prop::num::u8::ANY, 0..600)) {
        let mut raw = Vec::new();
        raw.extend_from_slice(&(data.len() as u32 + 8).to_be_bytes());
        raw.extend_from_slice(b"stbl");
        raw.extend_from_slice(&data);
        let b = first_box(&raw);
        if let Ok(table) = SampleTable::parse(&b) {
            let n = table.sample_count();
            let mut cursor = table.cursor();
            for i in 0..n.min(4096) {
                prop_assert_eq!(table.sample(i), cursor.next());
            }
            let _ = table.dts_shift();
            let _ = table.total_duration();
        }
    }

    /// Box iteration always terminates and always advances.
    #[test]
    fn box_iteration_advances(data in prop::collection::vec(prop::num::u8::ANY, 0..800)) {
        let mut last = 0u64;
        let mut n = 0usize;
        for item in boxes::BoxIter::new(&data, 0) {
            n += 1;
            prop_assert!(n < 4096);
            let Ok(b) = item else { break };
            prop_assert!(b.offset >= last);
            prop_assert!(b.header.size >= b.header.header_len);
            last = b.offset + b.header.size;
        }
    }

    /// A declared entry count can never buy more entries than bytes exist.
    #[test]
    fn a_declared_count_never_exceeds_the_payload(
        data in prop::collection::vec(prop::num::u8::ANY, 0..200),
        stride in 1usize..24,
        declared in prop::num::u32::ANY,
    ) {
        let t = EntryTable::new(&data, stride, declared);
        prop_assert!(t.len() as usize <= data.len() / stride);
        prop_assert!(t.len() <= declared);
        prop_assert!(t.entry(t.len()).is_none());
    }

    /// A summary is bounded in size and monotone in its checkpoints.
    #[test]
    fn run_summaries_are_bounded_and_monotone(
        runs in prop::collection::vec((0u32..20, 0i64..1000), 0..300),
    ) {
        let idx = RunIndex::build(runs.len() as u32, |i| runs[i as usize]);
        prop_assert!(idx.checkpoints() <= MAX_CHECKPOINTS);
        let mut samples = 0u64;
        let mut value = 0i64;
        for (count, per) in &runs {
            samples = samples.saturating_add(u64::from(*count));
            value = value.saturating_add(i64::from(*count).saturating_mul(*per));
        }
        prop_assert_eq!(idx.total_samples(), samples);
        prop_assert_eq!(idx.total_value(), value);
        // Every checkpoint is at or before the sample it is looked up with.
        for n in [0u64, samples / 2, samples, u64::MAX] {
            prop_assert!(idx.checkpoint_for_sample(n).samples <= n);
        }
    }

    /// The decimation stride always keeps the summary under its cap.
    #[test]
    fn decimation_keeps_the_summary_capped(runs in prop::num::u32::ANY) {
        let s = decimation_stride(runs);
        prop_assert!(s >= 1);
        let points = u64::from(runs).div_ceil(u64::from(s));
        prop_assert!(points <= MAX_CHECKPOINTS as u64, "{} points", points);
    }

    /// Mapping a presentation time into the media and back is **idempotent**.
    ///
    /// Not invertible: a legal edit list may present the same media region
    /// twice, and `media_to_presentation` documents that the first segment in
    /// presentation order wins. So `p -> m -> p'` need not give `p' == p`, but
    /// `p' -> m'` must give `m' == m`, and that is the property that actually
    /// guarantees the mapping is well defined. Found by this test failing on a
    /// three-segment overlapping list.
    #[test]
    fn the_edit_timeline_is_idempotent(
        entries in prop::collection::vec((1u32..5000, -1i32..5000), 0..6),
        media_duration in 1i64..100_000,
    ) {
        let mut body = (entries.len() as u32).to_be_bytes().to_vec();
        for (d, m) in &entries {
            body.extend_from_slice(&d.to_be_bytes());
            body.extend_from_slice(&m.to_be_bytes());
            body.extend_from_slice(&1i16.to_be_bytes());
            body.extend_from_slice(&0u16.to_be_bytes());
        }
        let mut raw = ((body.len() + 12) as u32).to_be_bytes().to_vec();
        raw.extend_from_slice(b"elst");
        raw.extend_from_slice(&[0, 0, 0, 0]);
        raw.extend_from_slice(&body);
        let el = EditList::parse(&first_box(&raw).full().unwrap());
        let tl = el.resolve(1000, 1000, media_duration);
        let duration = tl.duration();
        for p in [0i64, 1, duration / 2, duration - 1, duration] {
            let Some(m) = tl.presentation_to_media(p) else { continue };
            let Some(p2) = tl.media_to_presentation(m) else {
                return Err(TestCaseError::fail("a mapped media time mapped back to nothing"));
            };
            prop_assert_eq!(tl.presentation_to_media(p2), Some(m), "at {}", p);
        }
    }

    /// Rescaling between the movie and media timescales round-trips whenever
    /// the value is a whole number of the coarser unit.
    #[test]
    fn timescale_rescaling_round_trips_on_exact_values(
        movie_ts in 1u32..100_000,
        ratio in 1u32..1000,
        whole in 0i64..10_000,
    ) {
        let media_ts = movie_ts.saturating_mul(ratio).max(1);
        let there = rescale_movie_to_media(whole, movie_ts, media_ts);
        let back = rescale_media_to_movie(there, media_ts, movie_ts);
        prop_assert_eq!(back, whole);
    }

    /// Every packable language round-trips, and every unpackable value stays
    /// unpackable rather than becoming three invented letters.
    #[test]
    fn language_packing_round_trips(packed in prop::num::u16::ANY) {
        let l = Language::unpack(packed);
        match l {
            Language::Iso639(c) => {
                prop_assert!(c.iter().all(u8::is_ascii_lowercase));
                prop_assert_eq!(l.pack(), packed);
                prop_assert_eq!(Language::unpack(l.pack()), l);
            }
            Language::Macintosh(v) => prop_assert_eq!(v, packed),
            Language::Undefined => prop_assert!(packed == 0 || packed == 0x55C4),
        }
    }

    /// The MPEG-4 expandable length encoding round-trips at every width, and a
    /// descriptor never claims more than the buffer holds.
    #[test]
    fn expandable_lengths_round_trip(len in 0u32..4096) {
        let mut d = vec![0x05u8];
        d.extend_from_slice(&write_expandable(len));
        d.resize(5 + len as usize, 0xAB);
        let (tag, body, used) = read_descriptor(&d).unwrap();
        prop_assert_eq!(tag, 0x05);
        prop_assert_eq!(body.len(), len as usize);
        prop_assert_eq!(used, 5 + len as usize);
    }

    /// A descriptor whose declared length exceeds the buffer is refused, never
    /// clamped — clamping would let a crafted `esds` alias its neighbour's
    /// bytes into its extradata.
    #[test]
    fn an_overlong_descriptor_is_refused(
        len in 1u32..1000,
        short_by in 1usize..64,
    ) {
        let mut d = vec![0x05u8];
        d.extend_from_slice(&write_expandable(len));
        let have = (len as usize).saturating_sub(short_by);
        d.resize(5 + have, 0);
        if have < len as usize {
            prop_assert!(read_descriptor(&d).is_none());
        }
    }
}
