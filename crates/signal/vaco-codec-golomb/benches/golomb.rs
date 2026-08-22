//! What the Exp-Golomb paths actually cost, measured rather than assumed.
//!
//! Two questions, and neither has an obvious answer from reading the code:
//!
//! 1. **Is the one-peek `ue(v)` in this crate faster than the two-step one in
//!    `vaco-bitstream`?** Both are branch-light and neither loops; the
//!    difference is one cache extraction against two, plus one fewer possible
//!    refill. That is small enough that it could easily be noise.
//! 2. **Does the codeword-length batch loop vectorise?** `leading_zeros` maps to
//!    a lane-wise instruction on every target we ship to, so it should — but
//!    D12's addendum is a standing reminder that what LLVM does with a loop is a
//!    measurement, not a deduction.
//!
//! Run with `cargo bench -p vaco-codec-golomb`.
#![allow(
    clippy::unwrap_used,
    missing_debug_implementations,
    reason = "benchmark code"
)]

use divan::counter::ItemsCount;
use std::sync::LazyLock;
use vaco_bitstream::{BitReader, BitWriter, GolombRead, Padded};
use vaco_codec_golomb::{GolombDecode, GolombEncode, map};

fn main() {
    verify();
    divan::main();
}

/// Values with the distribution real syntax elements have: mostly tiny, with a
/// long tail. A benchmark over uniform `u32` would measure the cold path, which
/// is not the path that matters.
fn realistic_values(n: usize) -> Vec<u32> {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut out = Vec::new();
    for _ in 0..n {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        // Geometric-ish: a run length drawn from the low bits decides the
        // magnitude, so ~half the values are under 4 and a few reach 2^20.
        let mag = (state & 0x1F) as u32;
        let mag = if mag < 16 { 2 } else { mag.min(20) };
        out.push((state >> 20) as u32 & ((1u32 << mag) - 1));
    }
    out
}

/// Uniform values, to show the cold-path cost honestly alongside the hot one.
fn wide_values(n: usize) -> Vec<u32> {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut out = Vec::new();
    for _ in 0..n {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        // Up to 2^31, so prefixes of 16..31 zeros are common.
        out.push((state >> 33) as u32);
    }
    out
}

const N: usize = 4096;

struct Corpus {
    values: Vec<u32>,
    /// The values encoded as `ue(v)`, with the 64 zero bytes `Padded` wants.
    padded: Vec<u8>,
    logical: usize,
}

fn encode(values: &[u32]) -> Corpus {
    let mut w = BitWriter::new();
    for &v in values {
        w.put_ue_v(v);
    }
    let bytes = w.finish();
    let logical = bytes.len();
    let mut padded = bytes;
    padded.resize(logical + Padded::PAD, 0);
    Corpus {
        values: values.to_vec(),
        padded,
        logical,
    }
}

static NARROW: LazyLock<Corpus> = LazyLock::new(|| encode(&realistic_values(N)));
static WIDE: LazyLock<Corpus> = LazyLock::new(|| encode(&wide_values(N)));

impl Corpus {
    fn reader(&self) -> BitReader<'_> {
        match Padded::new(&self.padded, self.logical) {
            Some(p) => BitReader::new_padded(p),
            None => BitReader::new(&self.padded),
        }
    }
}

/// The benchmarks are only meaningful if both readers decode the corpus
/// correctly, so check that once before running any of them.
fn verify() {
    for c in [&*NARROW, &*WIDE] {
        let mut a = c.reader();
        let mut b = c.reader();
        for &want in &c.values {
            assert_eq!(GolombDecode::ue_v(&mut a), want);
            assert_eq!(GolombRead::ue(&mut b), want);
        }
        assert!(!a.overrun() && !b.overrun());
    }
    let vals = realistic_values(64);
    let mut lens = vec![0u32; vals.len()];
    map::ue_bit_len_batch(&vals, &mut lens);
    let total: u64 = lens.iter().map(|&l| u64::from(l)).sum();
    assert_eq!(total, map::ue_bits_total(&vals));
}

// ------------------------------------------------------------------ decoding

#[divan::bench(args = ["narrow", "wide"])]
fn ue_this_crate(bencher: divan::Bencher<'_, '_>, which: &str) {
    let c = if which == "narrow" { &*NARROW } else { &*WIDE };
    bencher.counter(ItemsCount::new(N)).bench_local(|| {
        let mut r = c.reader();
        let mut acc = 0u64;
        for _ in 0..N {
            acc = acc.wrapping_add(u64::from(GolombDecode::ue_v(&mut r)));
        }
        acc
    });
}

#[divan::bench(args = ["narrow", "wide"])]
fn ue_bitstream(bencher: divan::Bencher<'_, '_>, which: &str) {
    let c = if which == "narrow" { &*NARROW } else { &*WIDE };
    bencher.counter(ItemsCount::new(N)).bench_local(|| {
        let mut r = c.reader();
        let mut acc = 0u64;
        for _ in 0..N {
            acc = acc.wrapping_add(u64::from(GolombRead::ue(&mut r)));
        }
        acc
    });
}

#[divan::bench]
fn se_this_crate(bencher: divan::Bencher<'_, '_>) {
    let c = &*NARROW;
    bencher.counter(ItemsCount::new(N)).bench_local(|| {
        let mut r = c.reader();
        let mut acc = 0i64;
        for _ in 0..N {
            acc = acc.wrapping_add(i64::from(GolombDecode::se_v(&mut r)));
        }
        acc
    });
}

#[divan::bench]
fn ue_k3(bencher: divan::Bencher<'_, '_>) {
    let c = &*NARROW;
    bencher.counter(ItemsCount::new(N)).bench_local(|| {
        let mut r = c.reader();
        let mut acc = 0u64;
        for _ in 0..N {
            acc = acc.wrapping_add(u64::from(GolombDecode::ue_k(&mut r, 3)));
        }
        acc
    });
}

/// The bounded form: the same read plus a ceiling comparison and a `Result`.
/// This is what a parser that validates every field pays over one that does not.
#[divan::bench]
fn ue_max(bencher: divan::Bencher<'_, '_>) {
    let c = &*NARROW;
    bencher.counter(ItemsCount::new(N)).bench_local(|| {
        let mut r = c.reader();
        let mut acc = 0u64;
        for _ in 0..N {
            acc = acc.wrapping_add(u64::from(r.ue_v_max(u32::MAX).unwrap_or(0)));
        }
        acc
    });
}

// ------------------------------------------------------------------ encoding

#[divan::bench]
fn put_ue(bencher: divan::Bencher<'_, '_>) {
    let vals = &NARROW.values;
    bencher.counter(ItemsCount::new(N)).bench_local(|| {
        let mut w = BitWriter::new();
        for &v in vals {
            w.put_ue_v(v);
        }
        w.finish()
    });
}

// --------------------------------------------------------------- cost models

#[divan::bench]
fn bits_total_batch(bencher: divan::Bencher<'_, '_>) {
    let vals = &NARROW.values;
    bencher
        .counter(ItemsCount::new(N))
        .bench_local(|| map::ue_bits_total(divan::black_box(vals)));
}

/// The same total computed the way a naive encoder would: encode each value and
/// ask how long the output got. This is the baseline `ue_bits_total` replaces.
#[divan::bench]
fn bits_total_by_encoding(bencher: divan::Bencher<'_, '_>) {
    let vals = &NARROW.values;
    bencher.counter(ItemsCount::new(N)).bench_local(|| {
        let mut w = BitWriter::new();
        for &v in divan::black_box(vals) {
            w.put_ue_v(v);
        }
        w.bit_len()
    });
}

// ------------------------------------------------------- ue(v) shape study
//
// The two candidate shapes, written side by side in this file so the
// comparison cannot be confused by a crate boundary, an `#[inline]` decision
// or a differently-shaped fallback. Both are exactly what the corresponding
// library function does; whichever wins here is the one `read.rs` should keep.

/// Shape A — two extractions: peek, count zeros, skip the prefix, read the
/// suffix. What `vaco_bitstream::GolombRead::ue` does.
#[inline]
fn ue_two_step(r: &mut BitReader<'_>) -> u32 {
    let lz = r.peek(32).leading_zeros();
    if lz > 31 {
        r.flag_malformed();
        return 0;
    }
    r.skip(lz + 1);
    ((1u32 << lz) - 1).wrapping_add(r.get(lz))
}

/// Shape B — one extraction: a codeword with a prefix of 15 zeros or fewer is
/// at most 31 bits, so it is already inside the peeked word.
#[inline]
fn ue_one_peek(r: &mut BitReader<'_>) -> u32 {
    let word = r.peek(32);
    let lz = word.leading_zeros();
    if lz <= 15 {
        r.skip(2 * lz + 1);
        (word >> (31 - 2 * lz)) - 1
    } else if lz <= 31 {
        r.skip(lz + 1);
        ((1u32 << lz) - 1).wrapping_add(r.get(lz))
    } else {
        r.flag_malformed();
        0
    }
}

#[divan::bench(args = ["narrow", "wide"])]
fn shape_a_two_step(bencher: divan::Bencher<'_, '_>, which: &str) {
    let c = if which == "narrow" { &*NARROW } else { &*WIDE };
    bencher.counter(ItemsCount::new(N)).bench_local(|| {
        let mut r = c.reader();
        let mut acc = 0u64;
        for _ in 0..N {
            acc = acc.wrapping_add(u64::from(ue_two_step(&mut r)));
        }
        acc
    });
}

#[divan::bench(args = ["narrow", "wide"])]
fn shape_b_one_peek(bencher: divan::Bencher<'_, '_>, which: &str) {
    let c = if which == "narrow" { &*NARROW } else { &*WIDE };
    bencher.counter(ItemsCount::new(N)).bench_local(|| {
        let mut r = c.reader();
        let mut acc = 0u64;
        for _ in 0..N {
            acc = acc.wrapping_add(u64::from(ue_one_peek(&mut r)));
        }
        acc
    });
}
