//! What HEVC header parsing costs, measured rather than assumed.
//!
//! Four questions, none of which has an obvious answer from reading the code:
//!
//! 1. **How expensive is an HEVC SPS against an H.264 one?** It carries a
//!    twelve-byte `profile_tier_level()`, a variable number of reference
//!    picture sets and a longer VUI, so it should cost more — but every one of
//!    its elements goes through the same bounded `ue(v)` machinery, and
//!    `vaco-parse-h264` measured a whole ~40-element SPS at 135 ns. If the
//!    bookkeeping dominated, the two would be within noise of each other and
//!    the extra syntax would be free.
//! 2. **What does `profile_tier_level()` cost on its own?** Ninety-six bits in
//!    forty-odd separate reads, one of which is a 32-bit flag word. It is the
//!    single largest fixed-width block in the crate and it is read twice per
//!    stream (VPS and SPS), so it is worth knowing.
//! 3. **What does the whole-stream path cost per byte?** A parser that reads
//!    headers only still touches every byte, because it has to scan for start
//!    codes. The expectation was that the scan would dominate. **It does not**:
//!    measured, `parse_elementary_stream` is 1.35 ms against `scan_only`'s
//!    69 µs on the same megabyte, a ratio of **19.5x**.
//!
//!    That is not a bug, and it is worth writing down why. The fixture packs an
//!    access unit into every 520 bytes, so a megabyte holds ~2000 of them, and
//!    each costs one ~210 ns slice-header parse plus two copies of the unit
//!    (de-escaping into `RbspBuf`, then into the emitted `Packet`). A real 1080p
//!    stream carries ~30 KB access units — sixty times fewer per megabyte — so
//!    this fixture is a deliberate worst case for header density and the ratio
//!    on real media is far smaller. Read it as "what 2000 access units cost",
//!    not as "what a file costs".
//! 4. **Is HEVC's one-bit picture boundary actually cheaper than H.264's
//!    seven-field comparison?** H.264 must parse the *next* slice header to
//!    decide whether the current access unit ended; HEVC reads one bit. The
//!    ratio between `parse_elementary_stream` and `scan_only` is where that
//!    shows up.
//!
//! Reported as ratios, per plan 12's PF-0.1 rule: "1.76x" survives a different
//! machine and "faster" does not.
//!
//! Run with `cargo bench -p vaco-parse-hevc`.
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
use vaco_bitstream::{BitReader, annexb};
use vaco_codec_core::Parser;
use vaco_codec_golomb::BoundedGolomb;
use vaco_limits::{Budget, Limits};
use vaco_parse_hevc::{HevcParser, Pps, ProfileTierLevel, Sps, Vps, codec_parameters, params};

fn main() {
    verify();
    divan::main();
}

/// The VPS, SPS and PPS `x265` writes for 1920x1080. EBSP, exactly as they
/// appear in the stream, so the de-escaping cost is in the measurement where it
/// belongs — and these three all carry escapes, which an H.264 SPS often does
/// not.
const FHD_VPS: &[u8] = &[
    0x40, 0x01, 0x0c, 0x01, 0xff, 0xff, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03,
    0x00, 0x00, 0x03, 0x00, 0x78, 0x95, 0x98, 0x09,
];
const FHD_SPS: &[u8] = &[
    0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03,
    0x00, 0x78, 0xa0, 0x03, 0xc0, 0x80, 0x10, 0xe5, 0x96, 0x56, 0x69, 0x24, 0xca, 0xf0, 0x16, 0x80,
    0x80, 0x00, 0x00, 0x03, 0x00, 0x80, 0x00, 0x00, 0x0c, 0x84,
];
const FHD_PPS: &[u8] = &[0x44, 0x01, 0xc1, 0x72, 0xb4, 0x62, 0x40];

fn deescape(ebsp: &'static [u8]) -> Vec<u8> {
    let mut scratch = Vec::new();
    annexb::to_rbsp(ebsp, &mut scratch).to_vec()
}

static FHD_VPS_RBSP: LazyLock<Vec<u8>> = LazyLock::new(|| deescape(FHD_VPS));
static FHD_SPS_RBSP: LazyLock<Vec<u8>> = LazyLock::new(|| deescape(FHD_SPS));
static FHD_PPS_RBSP: LazyLock<Vec<u8>> = LazyLock::new(|| deescape(FHD_PPS));

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
    for set in [FHD_VPS, FHD_SPS, FHD_PPS] {
        v.extend_from_slice(&[0, 0, 0, 1]);
        v.extend_from_slice(set);
    }
    // Every slice sets `first_slice_segment_in_pic_flag`, so every one starts a
    // new access unit — which is the boundary case the parser is measured on.
    let mut first = true;
    while v.len() < (1 << 20) {
        v.extend_from_slice(&[0, 0, 0, 1]);
        if first {
            // IDR_N_LP, the real header from `sd.265`.
            v.extend_from_slice(&[0x28, 0x01, 0xaf, 0x1d, 0x30, 0xc6, 0x23, 0x40]);
            first = false;
        } else {
            // TRAIL_R, likewise.
            v.extend_from_slice(&[0x02, 0x01, 0xd0, 0x29, 0x4b, 0xe1, 0x0c, 0x63]);
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
    }
    v
});

/// Nothing below means anything if the fixtures do not parse, and a benchmark
/// that silently measures an error path is worse than no benchmark.
fn verify() {
    let mut budget = Budget::new(Limits::permissive());
    Vps::parse(&FHD_VPS_RBSP, &mut budget).expect("the fixture VPS must parse");
    let sps = Sps::parse(&FHD_SPS_RBSP, &mut budget).expect("the fixture SPS must parse");
    assert_eq!(sps.dimensions(), Some((1920, 1080)));
    assert_eq!(sps.coded_width(), 1920);
    Pps::parse(&FHD_PPS_RBSP, &mut budget).expect("the fixture PPS must parse");

    let mut parser = HevcParser::new(Limits::permissive());
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

    /// One SPS, from an RBSP that is already de-escaped. The number to compare
    /// against `vaco-parse-h264`'s `sps_parse`.
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

    /// The VPS, which is mostly `profile_tier_level()` and little else.
    #[divan::bench]
    fn vps_parse(bencher: divan::Bencher<'_, '_>) {
        let rbsp = &*FHD_VPS_RBSP;
        bencher.counter(ItemsCount::new(1usize)).bench_local(|| {
            let mut budget = Budget::new(Limits::permissive());
            Vps::parse(divan::black_box(rbsp), &mut budget)
        });
    }

    /// The PPS. Much shorter than an SPS, and the ratio between them says
    /// whether per-element overhead or element count dominates.
    #[divan::bench]
    fn pps_parse(bencher: divan::Bencher<'_, '_>) {
        let rbsp = &*FHD_PPS_RBSP;
        bencher.counter(ItemsCount::new(1usize)).bench_local(|| {
            let mut budget = Budget::new(Limits::permissive());
            Pps::parse(divan::black_box(rbsp), &mut budget)
        });
    }

    /// `profile_tier_level()` alone — 96 bits in forty-odd reads, and the block
    /// both the VPS and the SPS pay for.
    #[divan::bench]
    fn profile_tier_level(bencher: divan::Bencher<'_, '_>) {
        // Bit 24 of the SPS RBSP is where the general layer starts.
        let rbsp = &*FHD_SPS_RBSP;
        bencher.counter(ItemsCount::new(1usize)).bench_local(|| {
            let mut reader = BitReader::new(divan::black_box(rbsp));
            reader.skip(24);
            let mut budget = Budget::new(Limits::permissive());
            let mut g = BoundedGolomb::new(&mut reader, &mut budget);
            ProfileTierLevel::parse(&mut g, true, 0)
        });
    }

    /// A real slice segment header, and the same header followed by random
    /// bytes instead of real slice data.
    ///
    /// The ratio between them is the security property: a malformed header must
    /// not cost meaningfully more than a real one, because a stream is mostly
    /// slice headers and an attacker chooses their contents.
    ///
    /// Measured at **1.26x** (263 ns against 208 ns). It was expected to be far
    /// worse before `num_entry_point_offsets` got §7.4.7.1's geometry-derived
    /// bound — the PPS here enables wavefront parallel processing, so every
    /// slice header ends with a count a malformed header controls. Measured A/B
    /// under the old flat ceiling of 8192 it was **252 ns**, i.e. no worse.
    /// The hypothesis was wrong; see `slice::max_entry_points` for the full
    /// note. The pair is kept because it is the right shape to notice a *future*
    /// unbounded field, which is what it was built to catch.
    #[divan::bench(args = ["real", "random_tail"])]
    fn slice_header(bencher: divan::Bencher<'_, '_>, kind: &str) {
        let mut budget = Budget::new(Limits::permissive());
        let sps = Sps::parse(&FHD_SPS_RBSP, &mut budget).unwrap();
        let pps = Pps::parse(&FHD_PPS_RBSP, &mut budget).unwrap();
        let mut nal = vec![0x02u8, 0x01, 0xd0, 0x29, 0x4b, 0xe1, 0x0c, 0x63];
        if kind == "random_tail" {
            // The adversarial shape: a plausible header followed by bytes that
            // make every open-ended field as large as its bound allows.
            nal.extend(std::iter::repeat_n(0x01u8, 64));
        } else {
            nal.extend_from_slice(&[0x86, 0x16, 0xd0, 0x1e, 0x32, 0xc3, 0xc2, 0x99]);
        }
        bencher.counter(ItemsCount::new(1usize)).bench_local(|| {
            let mut budget = Budget::new(Limits::permissive());
            vaco_parse_hevc::SliceHeader::parse(divan::black_box(&nal), &sps, &pps, &mut budget)
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
    /// scanner's resumption plus the buffer's read cursor are worth — and the
    /// shape `vaco-parse-h264` measured a 13.6x regression in before it replaced
    /// a per-access-unit `drain` with a cursor.
    #[divan::bench(args = [1024, 4096, 65536])]
    fn parse_chunked(bencher: divan::Bencher<'_, '_>, chunk: usize) {
        let data = &*STREAM;
        bencher
            .counter(BytesCount::of_slice(data))
            .bench_local(|| feed(divan::black_box(data), chunk));
    }

    /// Drive the parser correctly.
    ///
    /// `Parser::parse` hands back a queued access unit by returning it with a
    /// consumed count of **zero**, leaving the input for the next call. A caller
    /// that writes `off += used.max(1)` advances past a byte that has not been
    /// parsed and re-presents a shifted buffer — quadratic, and
    /// `vaco-parse-h264`'s first benchmark measured 19.15 ms against 120 µs on
    /// the same megabyte from exactly that. Worth a comment rather than a silent
    /// fix: the same mistake in a real caller is a real hazard.
    fn feed(data: &[u8], chunk: usize) -> usize {
        let mut parser = HevcParser::new(Limits::permissive());
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
