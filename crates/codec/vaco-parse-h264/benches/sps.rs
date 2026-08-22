//! What H.264 header parsing costs, measured rather than assumed.
//!
//! Three questions, none of which has an obvious answer from reading the code:
//!
//! 1. **How expensive is an SPS, really?** It is a few hundred bits, but every
//!    one of them goes through a bounded `ue(v)` that charges fuel against a
//!    budget. Two layers of bookkeeping over forty syntax elements could
//!    plausibly dominate the reads themselves — and if it does, that is an
//!    argument for the unbounded family plus one check at the end, which would
//!    be a *worse* design and needs to lose on evidence rather than on
//!    principle.
//! 2. **What does the whole-stream path cost per byte?** A parser that reads
//!    headers only still touches every byte of the file, because it has to scan
//!    for start codes. That scan should dominate; if header parsing shows up
//!    instead, something is being re-parsed.
//! 3. **Does re-parsing the slice header at every access-unit boundary hurt?**
//!    §7.4.1.2.4 needs the *next* slice's header to decide whether the current
//!    access unit has ended, and the obvious implementation parses that header
//!    twice — once to decide, once to keep. Measuring says whether caching it
//!    would be worth the state.
//!
//! Reported as ratios, per plan 12's PF-0.1 rule: "1.76x" survives a different
//! machine and "faster" does not.
//!
//! Run with `cargo bench -p vaco-parse-h264`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::wildcard_imports,
    missing_debug_implementations,
    reason = "benchmark code"
)]

use divan::counter::{BytesCount, ItemsCount};
use std::sync::LazyLock;
use vaco_bitstream::annexb;
use vaco_codec_core::Parser;
use vaco_limits::{Budget, Limits};
use vaco_parse_h264::{H264Parser, Pps, Sps, codec_parameters, params};

fn main() {
    verify();
    divan::main();
}

/// The SPS `libx264` writes for 1920x1080 — the one whose 1088-and-cropped
/// geometry is the crate's headline case. EBSP, exactly as it appears in the
/// stream, so the de-escaping cost is in the measurement where it belongs.
const FHD_SPS: &[u8] = &[
    0x67, 0x64, 0x00, 0x28, 0xac, 0xd9, 0x40, 0x78, 0x02, 0x27, 0xe5, 0xc0, 0x44, 0x00, 0x00, 0x03,
    0x00, 0x04, 0x00, 0x00, 0x03, 0x00, 0xc8, 0x3c, 0x60, 0xc6, 0x58,
];

/// The matching PPS.
const FHD_PPS: &[u8] = &[0x68, 0xEB, 0xE3, 0xCB, 0x22, 0xC0];

/// The SPS RBSP, de-escaped once so the parser benchmarks measure parsing
/// rather than escaping.
static FHD_SPS_RBSP: LazyLock<Vec<u8>> = LazyLock::new(|| {
    let mut scratch = Vec::new();
    annexb::to_rbsp(FHD_SPS, &mut scratch).to_vec()
});

static FHD_PPS_RBSP: LazyLock<Vec<u8>> = LazyLock::new(|| {
    let mut scratch = Vec::new();
    annexb::to_rbsp(FHD_PPS, &mut scratch).to_vec()
});

/// A synthetic elementary stream: parameter sets, then a run of access units
/// whose slices carry realistic-looking payload.
///
/// The payload is what a real file is mostly made of, and it is what the
/// start-code scan has to walk. Generated deterministically so the number is
/// comparable across runs and machines.
static STREAM: LazyLock<Vec<u8>> = LazyLock::new(|| {
    let mut s = 0x2545_F491_4F6C_DD1Du64;
    let mut rng = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let mut v = Vec::new();
    v.extend_from_slice(&[0, 0, 0, 1]);
    v.extend_from_slice(FHD_SPS);
    v.extend_from_slice(&[0, 0, 0, 1]);
    v.extend_from_slice(FHD_PPS);
    // The first slice is an IDR; the rest are not, and their frame_num rises so
    // §7.4.1.2.4 sees a genuine boundary at each one.
    let mut frame = 0u32;
    while v.len() < (1 << 20) {
        v.extend_from_slice(&[0, 0, 0, 1]);
        if frame == 0 {
            v.extend_from_slice(&[0x65, 0x88, 0x84, 0x00, 0x2F]);
        } else {
            // nal_ref_idc 2, type 1; slice_type P; pps 0; frame_num in the
            // low bits of the third byte.
            v.extend_from_slice(&[0x41, 0x9A, (frame as u8) | 0x02, 0x2F]);
        }
        for _ in 0..64 {
            let r = rng();
            for i in 0..8 {
                // Mask the low bits away so the payload never accidentally
                // contains a start code, which would move the boundaries and
                // make the benchmark measure a different stream.
                v.push(((r >> (i * 8)) as u8) | 0x40);
            }
        }
        frame = frame.wrapping_add(1) & 0x0F;
    }
    v
});

/// Nothing below means anything if the fixtures do not parse, and a benchmark
/// that silently measures an error path is worse than no benchmark.
fn verify() {
    let mut budget = Budget::new(Limits::permissive());
    let sps = Sps::parse(&FHD_SPS_RBSP, &mut budget).expect("the fixture SPS must parse");
    assert_eq!(sps.dimensions(), Some((1920, 1080)));
    assert_eq!(sps.coded_height(), 1088);
    Pps::parse(&FHD_PPS_RBSP, Some(&sps), &mut budget).expect("the fixture PPS must parse");

    let mut parser = H264Parser::new(Limits::permissive());
    let (_, used) = parser
        .parse(&STREAM)
        .expect("the fixture stream must parse");
    assert_eq!(used, STREAM.len());
    assert!(
        parser.parameters().is_some(),
        "the stream must yield parameters"
    );
}

// ------------------------------------------------------------ parameter sets

#[divan::bench_group(name = "parameter_sets")]
mod parameter_sets {
    use super::*;

    /// One SPS, from an RBSP that is already de-escaped.
    #[divan::bench]
    fn sps_parse(bencher: divan::Bencher<'_, '_>) {
        let rbsp = &*FHD_SPS_RBSP;
        bencher.counter(ItemsCount::new(1usize)).bench_local(|| {
            let mut budget = Budget::new(Limits::permissive());
            Sps::parse(divan::black_box(rbsp), &mut budget)
        });
    }

    /// The same SPS including the de-escaping pass, which is what a parser
    /// actually pays per parameter set.
    #[divan::bench]
    fn sps_parse_with_deescape(bencher: divan::Bencher<'_, '_>) {
        bencher.counter(ItemsCount::new(1usize)).bench_local(|| {
            let mut scratch = Vec::new();
            let rbsp = annexb::to_rbsp(divan::black_box(FHD_SPS), &mut scratch);
            let mut budget = Budget::new(Limits::permissive());
            Sps::parse(rbsp, &mut budget)
        });
    }

    /// One PPS. Much shorter than an SPS, and the ratio between them says
    /// whether per-element overhead or element count dominates.
    #[divan::bench]
    fn pps_parse(bencher: divan::Bencher<'_, '_>) {
        let rbsp = &*FHD_PPS_RBSP;
        bencher.counter(ItemsCount::new(1usize)).bench_local(|| {
            let mut budget = Budget::new(Limits::permissive());
            Pps::parse(divan::black_box(rbsp), None, &mut budget)
        });
    }

    /// Everything derived from a parsed SPS: geometry, pixel format, aspect
    /// ratio, colour, frame rate. A demuxer calls this once per stream, so it
    /// is allowed to be slow — the number exists to notice if it stops being.
    #[divan::bench]
    fn derive_codec_parameters(bencher: divan::Bencher<'_, '_>) {
        let mut budget = Budget::new(Limits::permissive());
        let sps = Sps::parse(&FHD_SPS_RBSP, &mut budget).unwrap();
        bencher.counter(ItemsCount::new(1usize)).bench(|| {
            let p = codec_parameters(divan::black_box(&sps));
            divan::black_box((p, params::sample_aspect_ratio(&sps)))
        });
    }
}

// ---------------------------------------------------------------- the stream

#[divan::bench_group(name = "stream")]
mod stream {
    use super::*;

    /// The whole path: scan, frame, de-escape, parse headers, split into access
    /// units. What a `-show_streams` run over an elementary stream costs per
    /// byte.
    #[divan::bench]
    fn parse_elementary_stream(bencher: divan::Bencher<'_, '_>) {
        let data = &*STREAM;
        bencher
            .counter(BytesCount::of_slice(data))
            .bench_local(|| feed(divan::black_box(data), data.len()));
    }

    /// The framing alone, with no header parsing at all: the floor the number
    /// above is measured against. If the two are close, header parsing is free
    /// and the scan is the whole cost.
    #[divan::bench]
    fn scan_only(bencher: divan::Bencher<'_, '_>) {
        let data = &*STREAM;
        bencher.counter(BytesCount::of_slice(data)).bench(|| {
            let mut n = 0usize;
            let mut i = 0usize;
            while let Some(sc) = annexb::find_start_code(divan::black_box(data), i) {
                n += 1;
                i = sc + 3;
            }
            n
        });
    }

    /// Feeding the same stream in small chunks, which is what a network source
    /// does. The gap against `parse_elementary_stream` is what the incremental
    /// scanner's resumption is worth.
    #[divan::bench(args = [1024, 4096, 65536])]
    fn parse_chunked(bencher: divan::Bencher<'_, '_>, chunk: usize) {
        let data = &*STREAM;
        bencher
            .counter(BytesCount::of_slice(data))
            .bench_local(|| feed(divan::black_box(data), chunk));
    }

    /// Drive the parser correctly, which the first version of this benchmark
    /// did **not**.
    ///
    /// `Parser::parse` hands back a queued access unit by returning it with a
    /// consumed count of **zero**, leaving the input for the next call. The
    /// first version wrote `off += used.max(1)`, which advanced past a byte
    /// that had not been parsed and re-presented a shifted buffer — quadratic,
    /// and it measured 19.15 ms against this version's 120 µs on the same
    /// megabyte. A 160x error, from four characters.
    ///
    /// That is worth a comment rather than a silent fix: the same mistake in a
    /// caller is a real hazard, which is why `H264Parser`'s documentation
    /// states the contract explicitly.
    fn feed(data: &[u8], chunk: usize) -> usize {
        let mut parser = H264Parser::new(Limits::permissive());
        let mut n = 0usize;
        for c in data.chunks(chunk.max(1)) {
            let mut rest = c;
            while !rest.is_empty() {
                let Ok((unit, used)) = parser.parse(rest) else {
                    return n;
                };
                if unit.is_some() {
                    n += 1;
                }
                if used == 0 && unit.is_none() {
                    return n;
                }
                rest = &rest[used..];
            }
        }
        while let Ok((Some(_), _)) = parser.parse(&[]) {
            n += 1;
        }
        n
    }
}
