//! What start-code scanning costs, measured rather than assumed.
//!
//! Scanning is the one genuinely hot loop in NAL framing: it touches every byte
//! of every file, and a 2 GB capture is 2 GB of scan. Three questions, none of
//! which has an obvious answer from reading the code:
//!
//! 1. **Is the word-skip scanner in `vaco-bitstream` actually faster than the
//!    textbook three-byte window?** It should be — video payload is
//!    overwhelmingly non-zero, so the eight-byte zero test skips seven bytes at
//!    a time — but D12's addendum and plan 12's PF-0.1 amendment are two
//!    recorded cases of exactly this reasoning measuring backwards.
//! 2. **Would `memchr::memmem` beat it?** `memchr` is a pre-declared workspace
//!    dependency with a hand-tuned SIMD substring search, and `00 00 01` is a
//!    substring search. If it wins by enough, that is a finding for
//!    `vaco-bitstream`'s owner, not a licence to keep a second scanner here.
//! 3. **What does the corpus shape do to the answer?** A scanner that wins on
//!    dense payload and loses on zero-heavy data has a denial-of-service
//!    profile, since the attacker picks the corpus.
//!
//! Reported as ratios, per plan 12's PF-0.1 rule: "1.76x" survives a different
//! machine and "faster" does not.
//!
//! Run with `cargo bench -p vaco-format-nalu`.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::wildcard_imports,
    missing_debug_implementations,
    reason = "benchmark code"
)]

use divan::counter::BytesCount;
use std::sync::LazyLock;
use vaco_bitstream::annexb;
use vaco_format_nalu::{Framing, RbspBuf, units};
use vaco_limits::{Budget, Limits};

fn main() {
    verify();
    divan::main();
}

const N: usize = 1 << 20;

/// A xorshift, so the corpora are deterministic across runs and machines.
fn rng(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// Realistic payload: entropy-coded bytes, near-uniform, with a start code
/// every few kilobytes. This is what a real file looks like.
static DENSE: LazyLock<Vec<u8>> = LazyLock::new(|| {
    let mut s = 0x2545_F491_4F6C_DD1D;
    let mut v = Vec::new();
    while v.len() < N {
        let r = rng(&mut s);
        for i in 0..8 {
            v.push((r >> (i * 8)) as u8);
        }
        if v.len() % 4096 < 8 {
            v.extend_from_slice(&[0, 0, 0, 1, 0x41]);
        }
    }
    v.truncate(N);
    v
});

/// Zero-heavy: a quarter of all bytes are zero, so every word-skip fails and
/// the byte path carries the scan. The adversarial shape.
static SPARSE: LazyLock<Vec<u8>> = LazyLock::new(|| {
    let mut s = 0x9E37_79B9_7F4A_7C15;
    let mut v = Vec::new();
    while v.len() < N {
        let r = rng(&mut s);
        for i in 0..8 {
            let b = (r >> (i * 8)) as u8;
            v.push(if b.trailing_zeros() >= 2 { 0 } else { b });
        }
    }
    v.truncate(N);
    v
});

/// All zeros: every position is a candidate and none is a match. The worst case
/// the scanner can be handed, and the one a fuzzer finds first.
static ZEROS: LazyLock<Vec<u8>> = LazyLock::new(|| vec![0u8; N]);

/// Many tiny units: the framing overhead rather than the scan dominates.
static MANY_UNITS: LazyLock<Vec<u8>> = LazyLock::new(|| {
    let mut s = 0xDEAD_BEEF_CAFE_F00D;
    let mut v = Vec::new();
    while v.len() < N {
        v.extend_from_slice(&[0, 0, 1]);
        let r = rng(&mut s);
        for i in 0..8 {
            v.push(((r >> (i * 8)) as u8) | 0x40);
        }
    }
    v.truncate(N);
    v
});

// ------------------------------------------------------------ the candidates

/// The definition, scanned the obvious way: a three-byte window at every
/// offset. Correct by inspection, and the baseline everything else must beat.
fn naive_count(buf: &[u8]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i + 3 <= buf.len() {
        if buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 1 {
            n += 1;
            i += 3;
        } else {
            i += 1;
        }
    }
    n
}

/// `vaco-bitstream`'s word-skip scanner: the project's definition of where a
/// start code is, and what this crate calls.
fn word_skip_count(buf: &[u8]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while let Some(sc) = annexb::find_start_code(buf, i) {
        n += 1;
        i = sc + 3;
    }
    n
}

/// `memchr`'s SIMD substring search over the three-byte needle.
fn memmem_count(buf: &[u8]) -> usize {
    let finder = memchr::memmem::Finder::new(&[0u8, 0, 1]);
    let mut n = 0;
    let mut i = 0;
    while let Some(off) = finder.find(&buf[i..]) {
        n += 1;
        i += off + 3;
    }
    n
}

/// `memchr`'s single-byte search for the leading zero, confirming by hand.
///
/// The shape people reach for when they know `memchr` is fast but have not
/// noticed `memmem` exists. Included because it is the obvious wrong turn and
/// measuring it costs nothing.
fn memchr_zero_count(buf: &[u8]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while let Some(off) = memchr::memchr(0, &buf[i..]) {
        let at = i + off;
        if buf.get(at + 1) == Some(&0) && buf.get(at + 2) == Some(&1) {
            n += 1;
            i = at + 3;
        } else {
            i = at + 1;
        }
    }
    n
}

/// All four scanners must agree on every corpus before any timing means
/// anything. A fast scanner that finds a different set of boundaries is not a
/// faster scanner.
fn verify() {
    for (name, corpus) in [
        ("dense", &*DENSE),
        ("sparse", &*SPARSE),
        ("zeros", &*ZEROS),
        ("many_units", &*MANY_UNITS),
    ] {
        let n = naive_count(corpus);
        assert_eq!(word_skip_count(corpus), n, "word_skip disagrees on {name}");
        assert_eq!(memmem_count(corpus), n, "memmem disagrees on {name}");
        assert_eq!(memchr_zero_count(corpus), n, "memchr disagrees on {name}");
    }
}

// ---------------------------------------------------------------- benchmarks

#[divan::bench_group(name = "scan")]
mod scan {
    use super::*;

    #[divan::bench(args = ["dense", "sparse", "zeros", "many_units"])]
    fn naive(bencher: divan::Bencher<'_, '_>, corpus: &str) {
        run(bencher, corpus, naive_count);
    }

    #[divan::bench(args = ["dense", "sparse", "zeros", "many_units"])]
    fn word_skip(bencher: divan::Bencher<'_, '_>, corpus: &str) {
        run(bencher, corpus, word_skip_count);
    }

    #[divan::bench(args = ["dense", "sparse", "zeros", "many_units"])]
    fn memmem(bencher: divan::Bencher<'_, '_>, corpus: &str) {
        run(bencher, corpus, memmem_count);
    }

    #[divan::bench(args = ["dense", "sparse", "zeros", "many_units"])]
    fn memchr_zero(bencher: divan::Bencher<'_, '_>, corpus: &str) {
        run(bencher, corpus, memchr_zero_count);
    }

    fn run(bencher: divan::Bencher<'_, '_>, corpus: &str, f: fn(&[u8]) -> usize) {
        let buf = pick(corpus);
        bencher
            .counter(BytesCount::of_slice(buf))
            .bench(|| divan::black_box(f(divan::black_box(buf))));
    }
}

/// The whole framing path: scan, iterate, de-escape into a padded buffer. What
/// a parser actually pays per byte.
#[divan::bench_group(name = "frame")]
mod frame {
    use super::*;

    #[divan::bench(args = ["dense", "many_units"])]
    fn iterate_only(bencher: divan::Bencher<'_, '_>, corpus: &str) {
        let buf = pick(corpus);
        bencher
            .counter(BytesCount::of_slice(buf))
            .bench(|| units(divan::black_box(buf), Framing::AnnexB).count());
    }

    #[divan::bench(args = ["dense", "many_units"])]
    fn iterate_and_deescape(bencher: divan::Bencher<'_, '_>, corpus: &str) {
        let buf = pick(corpus);
        bencher.counter(BytesCount::of_slice(buf)).bench_local(|| {
            let mut budget = Budget::new(Limits::permissive());
            let mut rbsp = RbspBuf::new();
            let mut total = 0usize;
            for nal in units(divan::black_box(buf), Framing::AnnexB) {
                rbsp.fill(nal.data, &mut budget).unwrap();
                total += rbsp.len();
            }
            total
        });
    }

    /// The copy `RbspBuf` exists to remove: de-escape into one buffer, then
    /// copy again into a padded one.
    #[divan::bench(args = ["dense", "many_units"])]
    fn iterate_and_deescape_two_copies(bencher: divan::Bencher<'_, '_>, corpus: &str) {
        let buf = pick(corpus);
        bencher.counter(BytesCount::of_slice(buf)).bench_local(|| {
            let mut scratch = Vec::new();
            let mut padded = Vec::new();
            let mut total = 0usize;
            for nal in units(divan::black_box(buf), Framing::AnnexB) {
                let r = annexb::to_rbsp(nal.data, &mut scratch);
                let p = vaco_bitstream::Padded::from_slice_copying(r, &mut padded);
                total += p.logical_len();
            }
            total
        });
    }
}

fn pick(name: &str) -> &'static [u8] {
    match name {
        "dense" => &DENSE,
        "sparse" => &SPARSE,
        "zeros" => &ZEROS,
        _ => &MANY_UNITS,
    }
}
