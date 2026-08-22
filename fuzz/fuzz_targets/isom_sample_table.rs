//! The sample tables in isolation, driven by structured input.
//!
//! `isom_file` reaches the tables through a whole file, so most of its
//! executions are spent getting a `moov` to parse at all. This target builds a
//! *valid box structure* around **arbitrary table contents**, which puts every
//! byte the fuzzer controls into `stts`, `ctts`, `stsc`, `stsz`/`stz2`,
//! `stco`/`co64` and `stss` — the tables whose cross-references are the crate's
//! real attack surface.
//!
//! The shapes it is looking for, all of which are cheap to write and none of
//! which a well-formed file contains:
//!
//! * a `stsc` run whose chunk span multiplies past `u32`;
//! * `stsz` counts that disagree with `stts` and with `stsc`;
//! * chunk offsets plus running sizes that overflow `u64`;
//! * `stts` deltas that make the cumulative decode time saturate;
//! * a `stz2` field size of 4 with an odd sample count;
//! * `stss` entries that are unsorted, zero, or past the end.
//!
//! The oracle is the same central invariant `isom_file` uses — random access
//! and the cursor must agree — plus the total-function requirement on every
//! query.
//! fuzz-crate: vaco-format-isom

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_format_isom::build::{StblSpec, stbl};
use vaco_format_isom::{SampleTable, build};

/// Entries per table. Small enough that the fuzzer explores shapes rather than
/// sizes; the size dimension is covered by `isom_file` and by the unit tests
/// that clamp declared counts.
const MAX_ENTRIES: usize = 48;

#[derive(Debug, arbitrary::Arbitrary)]
struct Input {
    stts: Vec<(u32, u32)>,
    ctts: Vec<(u32, i32)>,
    ctts_version_one: bool,
    stsc: Vec<(u32, u32, u32)>,
    stsz: Vec<u32>,
    uniform_size: Option<(u32, u32)>,
    stz2: Option<(u8, Vec<u8>, u32)>,
    stco: Vec<u32>,
    co64: Option<Vec<u64>>,
    stss: Vec<u32>,
    has_stss: bool,
    cslg: Option<(i64, i64, i64, i64, i64)>,
    probe_sample: u32,
    probe_dts: i64,
}

fn truncate<T>(mut v: Vec<T>) -> Vec<T> {
    v.truncate(MAX_ENTRIES);
    v
}

fuzz_target!(|input: Input| {
    let spec = StblSpec {
        stts: truncate(input.stts),
        ctts_v0: if input.ctts_version_one {
            Vec::new()
        } else {
            truncate(input.ctts.clone())
        },
        ctts_v1: if input.ctts_version_one {
            truncate(input.ctts)
        } else {
            Vec::new()
        },
        cslg: input.cslg,
        stss: truncate(input.stss),
        has_stss: input.has_stss,
        stsc: truncate(input.stsc),
        stsz: truncate(input.stsz),
        stsz_uniform: input.uniform_size,
        stz2: input.stz2.map(|(f, mut d, c)| {
            d.truncate(MAX_ENTRIES);
            (f, d, c)
        }),
        stco: truncate(input.stco),
        co64: input.co64.map(truncate),
        ..StblSpec::default()
    };
    let raw = stbl(&spec);
    let b = build::first_box(&raw);
    // A `stsc` whose runs do not increase is rejected by design; that is the
    // one structural refusal in the crate and it is a legitimate outcome.
    let Ok(table) = SampleTable::parse(&b) else {
        return;
    };

    // Every query is total.
    let count = table.sample_count();
    let _ = table.chunk_count();
    let _ = table.total_duration();
    let _ = table.dts_shift();

    for n in [
        0u32,
        input.probe_sample,
        count.saturating_sub(1),
        count,
        u32::MAX,
    ] {
        let _ = table.dts(n);
        let _ = table.cts_offset(n);
        let _ = table.offset(n);
        let _ = table.is_sync(n);
        if let Some(before) = table.sync_at_or_before(n) {
            assert!(before <= n, "sync_at_or_before went forwards from {n}");
        }
        if let Some(after) = table.sync_at_or_after(n) {
            assert!(after >= n, "sync_at_or_after went backwards from {n}");
        }
        if let Some(s) = table.sample(n) {
            assert_eq!(s.index, n);
            assert!(s.end() >= s.offset, "sample {n} wraps its own extent");
        }
    }

    for ts in [i64::MIN, -1, 0, input.probe_dts, i64::MAX] {
        if let Some(n) = table.sample_at_dts(ts) {
            assert!(n < count.max(1), "sample_at_dts returned {n} of {count}");
            assert!(
                table.dts(n) <= ts,
                "sample_at_dts({ts}) overshot to dts {}",
                table.dts(n)
            );
        }
    }

    // The central invariant: the two access paths must not diverge.
    //
    // Stated as "every sample the cursor yields is the sample random access
    // yields for that index, and the indices strictly increase". On a
    // well-formed table that is equivalent to `cursor[i] == sample(i)`, which
    // is what `tests/properties.rs` asserts; here it also has to survive a
    // table with holes in it, where the cursor skips indices random access
    // cannot resolve.
    // Gaps are checked exhaustively only when they are small. The first
    // version scanned `0..=last` densely, and because the cursor may skip a
    // whole chunk in one step, `last` could be near `u32::MAX` — the target
    // dropped to 74 executions per second and libFuzzer filed two `slow-unit`
    // artifacts against it. The cost was entirely in this oracle, not in the
    // crate: the same inputs run in under a millisecond once the scan is
    // bounded.
    const MAX_GAP_SCAN: u32 = 4096;
    let checked = count.min(MAX_ENTRIES as u32 * 4);
    let mut previous: Option<u32> = None;
    for s in table.cursor().take(checked as usize) {
        assert_eq!(
            table.sample(s.index),
            Some(s),
            "the cursor and random access disagree at sample {}",
            s.index
        );
        let from = previous.map_or(0, |p| p.saturating_add(1));
        if let Some(p) = previous {
            assert!(s.index > p, "the cursor went backwards: {p} then {}", s.index);
        }
        // Nothing the cursor skipped may be resolvable by random access.
        if s.index.saturating_sub(from) <= MAX_GAP_SCAN {
            for n in from..s.index {
                assert!(
                    table.sample(n).is_none(),
                    "the cursor skipped sample {n}, which random access resolves"
                );
            }
        }
        previous = Some(s.index);
    }

    // Termination, stated correctly.
    //
    // The first version of this asserted the cursor yields fewer than 2^20
    // samples, and the fuzzer refuted it in 25 executions with a *uniform*
    // `stsz` — `sample_size = 9727, sample_count = 4294967048` — in a twenty-
    // byte box. That table is legal and genuinely describes four billion
    // samples: a uniform `stsz` is the one count in the format with no payload
    // to clamp it against (`SampleSizes::uniform` documents this). So the
    // number of samples is a property of the file, and bounding it is not this
    // layer's job.
    //
    // What *is* this layer's job is that the cursor stops at the end rather
    // than wrapping back round, which is the actual termination proof: `next`
    // advances the index by one every call and refuses any index at or past
    // the sample count.
    assert!(
        table.cursor_at(count).next().is_none(),
        "the cursor produced a sample at or past the sample count"
    );
    assert!(
        table.cursor_at(u32::MAX).next().is_none() || count == u32::MAX,
        "the cursor produced a sample past u32::MAX"
    );
});
