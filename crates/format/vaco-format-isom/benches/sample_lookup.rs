//! The sample-lookup path, which a seek walks repeatedly.
//!
//! Three shapes, chosen because they bound the design:
//!
//! * `compact` — one `stts` run, one `stsc` run, one sample per chunk. What a
//!   normally-muxed file looks like, and the case that must be fast.
//! * `fragmented_tables` — one `stts` run per sample and one `stsc` run per
//!   chunk. The adversarial shape the decimated summaries exist for; the point
//!   is that it stays bounded, not that it stays fast.
//! * `uniform` — a constant sample size, where the within-chunk offset is a
//!   multiplication rather than a prefix sum.
//!
//! `random_access` is the number that matters: a seek does one
//! `sample_at_dts` plus one `sample`, and a scrubbing UI does that per frame.
//! `sequential` is the demux loop's cost and is there to show the cursor is
//! genuinely a different path rather than a wrapper over the random one.

#![allow(
    missing_debug_implementations,
    unreachable_pub,
    clippy::integer_division,
    reason = "benchmark harness; the divisors are literal constants"
)]

use vaco_format_isom::build::{StblSpec, stbl};
use vaco_format_isom::{IsoBox, SampleTable};

const SAMPLES: u32 = 50_000;

fn compact() -> Vec<u8> {
    stbl(&StblSpec {
        stts: vec![(SAMPLES, 512)],
        stsc: vec![(1, 1, 1)],
        stsz: (0..SAMPLES).map(|i| 1000 + (i % 977)).collect(),
        stco: (0..SAMPLES).map(|i| 4096 + i * 2048).collect(),
        stss: (0..SAMPLES / 25).map(|i| i * 25 + 1).collect(),
        ..StblSpec::default()
    })
}

fn fragmented_tables() -> Vec<u8> {
    stbl(&StblSpec {
        stts: (0..SAMPLES).map(|i| (1, 500 + (i % 7))).collect(),
        stsc: (0..SAMPLES).map(|i| (i + 1, 1, 1)).collect(),
        stsz: (0..SAMPLES).map(|i| 1000 + (i % 977)).collect(),
        stco: (0..SAMPLES).map(|i| 4096 + i * 2048).collect(),
        stss: (0..SAMPLES / 25).map(|i| i * 25 + 1).collect(),
        ..StblSpec::default()
    })
}

fn uniform() -> Vec<u8> {
    stbl(&StblSpec {
        stts: vec![(SAMPLES, 1024)],
        stsc: vec![(1, 16, 1)],
        stsz_uniform: Some((1024, SAMPLES)),
        stsz: vec![],
        stco: (0..SAMPLES / 16).map(|i| 4096 + i * 16384).collect(),
        stss: vec![1],
        ..StblSpec::default()
    })
}

fn table(raw: &[u8]) -> SampleTable<'_> {
    let b: IsoBox<'_> = vaco_format_isom::build::first_box(raw);
    // The fixtures above are constructed, so this always parses. A mistake in
    // one would show up as a zero-sample benchmark rather than as a panic,
    // which is the right failure mode for a harness the lint policy forbids
    // `unwrap` in.
    SampleTable::parse(&b).unwrap_or_else(|_| SampleTable::empty())
}

#[divan::bench(args = ["compact", "fragmented_tables", "uniform"])]
fn parse(bencher: divan::Bencher<'_, '_>, shape: &str) {
    let raw = match shape {
        "fragmented_tables" => fragmented_tables(),
        "uniform" => uniform(),
        _ => compact(),
    };
    bencher.bench(|| divan::black_box(table(divan::black_box(&raw))).sample_count());
}

#[divan::bench(args = ["compact", "fragmented_tables", "uniform"])]
fn random_access(bencher: divan::Bencher<'_, '_>, shape: &str) {
    let raw = match shape {
        "fragmented_tables" => fragmented_tables(),
        "uniform" => uniform(),
        _ => compact(),
    };
    let t = table(&raw);
    // A deterministic scatter over the whole track, so no run of the loop
    // benefits from the previous one's position.
    let probes: Vec<u32> = (0..256u32).map(|i| (i * 7919) % SAMPLES).collect();
    bencher.bench(|| {
        let mut acc = 0u64;
        for &n in &probes {
            if let Some(s) = t.sample(divan::black_box(n)) {
                acc = acc.wrapping_add(s.offset);
            }
        }
        acc
    });
}

#[divan::bench(args = ["compact", "fragmented_tables", "uniform"])]
fn seek_by_time(bencher: divan::Bencher<'_, '_>, shape: &str) {
    let raw = match shape {
        "fragmented_tables" => fragmented_tables(),
        "uniform" => uniform(),
        _ => compact(),
    };
    let t = table(&raw);
    let total = t.total_duration().max(1);
    let probes: Vec<i64> = (0..256i64).map(|i| (i * 7919) % total).collect();
    bencher.bench(|| {
        let mut acc = 0u64;
        for &ts in &probes {
            // What a seek actually does: find the sample at the time, then
            // walk back to the sync sample, then resolve it.
            if let Some(n) = t.sample_at_dts(divan::black_box(ts))
                && let Some(key) = t.sync_at_or_before(n)
                && let Some(s) = t.sample(key)
            {
                acc = acc.wrapping_add(s.offset);
            }
        }
        acc
    });
}

#[divan::bench(args = ["compact", "fragmented_tables", "uniform"])]
fn sequential(bencher: divan::Bencher<'_, '_>, shape: &str) {
    let raw = match shape {
        "fragmented_tables" => fragmented_tables(),
        "uniform" => uniform(),
        _ => compact(),
    };
    let t = table(&raw);
    bencher.bench(|| {
        let mut acc = 0u64;
        for s in t.cursor() {
            acc = acc.wrapping_add(s.offset);
        }
        acc
    });
}

fn main() {
    divan::main();
}
