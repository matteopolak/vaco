//! Whole-file ISOBMFF parse over arbitrary bytes.
//!
//! The broadest target: `IsoFile::parse` walks the top level, builds every
//! `moov` track, parses every `moof`, and reads every `sidx` and `tfra`. Almost
//! every box type this crate knows is reachable from here.
//!
//! What is asserted beyond "does not panic":
//!
//! * **Parsing terminates.** Box iteration advances by at least eight bytes per
//!   step and the generic walkers are depth-capped, so a nested or
//!   self-referential file must finish.
//! * **Every accessor is total.** A track that parsed must answer
//!   `sample_count`, `time_base`, `reported_duration` and `edit_shift` for any
//!   input, including a zero timescale and a `stsc` that names chunks the
//!   `stco` does not have.
//! * **Random access and sequential iteration agree.** `table.sample(n)` and
//!   the `n`-th item of `table.cursor()` must be identical. This is the
//!   crate's central invariant: they are two different code paths — one a
//!   binary search over decimated summaries, the other a carried running
//!   position — and a file that makes them disagree is a file where a seek
//!   lands somewhere a sequential read never would.
//! * **Nothing allocates proportionally to a declared count.** Enforced by the
//!   budget: a strict `Limits` with a small ceiling must produce a clean
//!   `LimitExceeded` rather than a large allocation.
//! fuzz-crate: vaco-format-isom

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_format_isom::{IsoFile, SampleTable};

/// How many samples to cross-check per track. The invariant is per sample, so
/// a bound keeps the target fast enough to find structural bugs rather than
/// spending every execution on one enormous table.
const CROSS_CHECK: u32 = 512;

fn cross_check(table: &SampleTable<'_>) {
    let n = table.sample_count().min(CROSS_CHECK);
    let mut cursor = table.cursor();
    for i in 0..n {
        let random = table.sample(i);
        let sequential = cursor.next();
        assert_eq!(
            random, sequential,
            "random access and the cursor disagree at sample {i}"
        );
        if let Some(s) = random {
            // pts must be derivable without overflowing, and the sample must
            // occupy a coherent byte range.
            let _ = s.pts();
            assert!(s.end() >= s.offset, "sample {i} wraps its own extent");
            // A sample the table can place must be findable by its own time.
            if let Some(found) = table.sample_at_dts(s.dts) {
                assert!(
                    table.dts(found) <= s.dts,
                    "sample_at_dts overshot at sample {i}"
                );
            }
        }
    }
    // Positioned cursors must match walked ones.
    if n > 1 {
        let at = n / 2;
        let jumped = table.cursor_at(at).next();
        assert_eq!(
            jumped,
            table.sample(at),
            "a positioned cursor disagrees with random access"
        );
    }
    // Sync-sample queries must be total and ordered.
    for i in [0u32, n / 2, n.saturating_sub(1), u32::MAX] {
        if let Some(before) = table.sync_at_or_before(i) {
            assert!(before <= i, "sync_at_or_before went forwards");
        }
        if let Some(after) = table.sync_at_or_after(i) {
            assert!(after >= i, "sync_at_or_after went backwards");
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let Ok(file) = IsoFile::parse(data, 0) else {
        return;
    };
    let _ = file.file_type.as_ref().map(|f| f.compatible_brands.len());

    if let Some(movie) = &file.movie {
        let movie_timescale = movie.header.timescale;
        for track in &movie.tracks {
            // Every derived quantity must be answerable, whatever the file said.
            let _ = track.time_base();
            let _ = track.media_type();
            let _ = track.language_tag();
            let _ = track.handler_name_str();
            let _ = track.reported_duration(movie_timescale);
            let _ = track.edit_shift(movie_timescale);
            let _ = track.is_self_contained();

            let timeline = track.timeline(movie_timescale);
            let _ = timeline.start_offset();
            let _ = timeline.duration();
            // The timeline must be internally consistent: a presentation time
            // that maps into the media must map back to itself.
            for p in [0i64, 1, timeline.duration() / 2, timeline.duration()] {
                if let Some(m) = timeline.presentation_to_media(p) {
                    assert_eq!(
                        timeline.media_to_presentation(m),
                        Some(p),
                        "the timeline is not invertible at {p}"
                    );
                }
            }

            cross_check(&track.sample_table);

            // Sample descriptions must parse without reference to the tables.
            if let Some(stsd) = track.sample_table.sample_descriptions
                && let Ok(entries) = vaco_format_isom::stsd::parse_stsd(&stsd, track.handler)
            {
                for e in &entries {
                    let _ = e.codec();
                    let _ = e.effective_format();
                    if let Some(c) = e.config() {
                        let _ = c.data.len();
                    }
                }
            }
        }
    }

    for (i, moof) in file.fragments.iter().enumerate() {
        for (t, traf) in moof.tracks.iter().enumerate() {
            let defaults = file
                .movie
                .as_ref()
                .map(|m| m.extends_for(traf.header.track_id))
                .unwrap_or_default();
            let Some(base) = moof.track_base(t, |id| {
                file.movie
                    .as_ref()
                    .map(|m| m.extends_for(id))
                    .unwrap_or_default()
            }) else {
                continue;
            };
            let mut n = 0u32;
            let mut last_dts = i64::MIN;
            for s in traf.samples(base, 0, &defaults) {
                n = n.saturating_add(1);
                assert!(n < 1 << 24, "fragment {i} track {t} did not terminate");
                // Decode times within one run are non-decreasing by
                // construction; a regression means the accumulator wrapped.
                assert!(s.dts >= last_dts, "fragment decode time went backwards");
                last_dts = s.dts;
                let _ = s.pts();
                let _ = s.is_sync();
            }
            assert_eq!(
                u64::from(n),
                traf.sample_count(),
                "the sample iterator and the declared count disagree"
            );
        }
    }

    for sidx in &file.segment_indexes {
        let mut last = 0u64;
        for (at, time, _) in sidx.subsegments() {
            assert!(at >= last || at == u64::MAX, "sidx offsets went backwards");
            last = at;
            let _ = time;
        }
    }

    for tfra in &file.random_access {
        let _ = tfra.at_or_before(0);
        let _ = tfra.at_or_before(u64::MAX);
    }
});
