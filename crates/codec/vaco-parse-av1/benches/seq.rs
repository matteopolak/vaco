//! What AV1 header parsing costs, measured rather than assumed.
//!
//! Two questions, and the measured answers (one run, this machine; re-run
//! before quoting these on different hardware):
//!
//! 1. **How does a sequence header compare to an HEVC SPS?** AV1's sequence
//!    header is bit-oriented like HEVC's SPS, but has no Exp-Golomb at all —
//!    every field is `f(n)`, `uvlc()` or a handful of fixed-width branches.
//!    Measured: **~31 ns median** for the real 11-byte `libsvtav1` sequence
//!    header. `vaco-parse-hevc`'s real-fixture SPS is a fifth again as many
//!    bytes, so the honest reading is "cheap either way", not a cross-codec
//!    speed claim — the two fixtures are not the same size.
//! 2. **What does OBU splitting cost against a NAL start-code scan?** AV1 has
//!    no start codes — every `next_obu_stream_unit` call is arithmetic on a
//!    declared length, not a byte search. Measured: **`split_only`** (a
//!    zero-allocation cursor) runs the 1 MB fixture at ~701 MB/s median;
//!    **`split_collecting`** (the `Vec`-allocating shape `CbsCodec::split`
//!    actually uses) at ~539 MB/s — allocation costs about **1.3x**.
//!
//!    **`parse_elementary_stream` is ~48 MB/s median — an order of magnitude
//!    slower than `split_only`.** Not a framing cost: this fixture packs a
//!    complete temporal unit (delimiter, sequence header, delimiter, frame)
//!    into 21 bytes, so a megabyte holds ~50,000 of them, each re-parsing the
//!    sequence header from scratch and allocating a `Packet` per access unit.
//!    `vaco-parse-hevc`'s own stream benchmark measures the identical shape of
//!    gap (19.5x, its fixture packs one access unit per 520 bytes) and calls
//!    it correctly: read this as "what 50,000 temporal units cost", not as
//!    "what a file costs" — a real stream's access units are tens of
//!    kilobytes apart, not 21 bytes.
//!
//! Reported as ratios, per plan 12's PF-0.1 rule: "1.76x" survives a different
//! machine and "faster" does not.
//!
//! Run with `cargo bench -p vaco-parse-av1`.
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
use vaco_codec_core::Parser;
use vaco_limits::{Budget, Limits};
use vaco_parse_av1::obu::{Av1Framing, next_obu_stream_unit, units};
use vaco_parse_av1::{Av1Parser, SequenceHeader};

fn main() {
    verify();
    divan::main();
}

/// The real `OBU_SEQUENCE_HEADER` payload measured throughout this crate's
/// tests: `libsvtav1`, 642x358, 8-bit 4:2:0, level 2.1.
const SEQ_HEADER_PAYLOAD: &[u8] = &[
    0x00, 0x00, 0x00, 0x0c, 0xc5, 0x03, 0x65, 0x00, 0xbe, 0x00, 0x10,
];

/// One temporal unit: delimiter, sequence header, second delimiter, a minimal
/// shown-key-frame `OBU_FRAME`. Repeated to build a megabyte-scale stream —
/// the OBU-splitting analogue of `vaco-parse-hevc::benches::sps::STREAM`.
static STREAM: LazyLock<Vec<u8>> = LazyLock::new(|| {
    let mut unit = vec![0x12, 0x00, 0x0a, 0x0b];
    unit.extend_from_slice(SEQ_HEADER_PAYLOAD);
    unit.extend_from_slice(&[0x12, 0x00, 0x32, 0x02, 0x10, 0x00]);
    let mut v = Vec::new();
    while v.len() < (1 << 20) {
        v.extend_from_slice(&unit);
    }
    v
});

fn verify() {
    let mut budget = Budget::new(Limits::permissive());
    let sh = SequenceHeader::parse(SEQ_HEADER_PAYLOAD, &mut budget).expect("fixture must parse");
    assert_eq!((sh.max_frame_width, sh.max_frame_height), (642, 358));

    let mut parser = Av1Parser::new(Limits::permissive());
    let (_, used) = parser.parse(&STREAM).expect("fixture stream must parse");
    assert_eq!(used, STREAM.len());
    assert!(parser.parameters().is_some());
}

#[divan::bench_group(name = "sequence_header")]
mod sequence_header {
    use super::*;

    #[divan::bench]
    fn parse(bencher: divan::Bencher<'_, '_>) {
        bencher.counter(ItemsCount::new(1usize)).bench_local(|| {
            let mut budget = Budget::new(Limits::permissive());
            SequenceHeader::parse(divan::black_box(SEQ_HEADER_PAYLOAD), &mut budget)
        });
    }

    /// Everything a `codec_parameters` call derives from a parsed sequence
    /// header: profile, level, pixel format, colour info.
    #[divan::bench]
    fn derive_codec_parameters(bencher: divan::Bencher<'_, '_>) {
        let mut budget = Budget::new(Limits::permissive());
        let sh = SequenceHeader::parse(SEQ_HEADER_PAYLOAD, &mut budget).unwrap();
        bencher
            .counter(ItemsCount::new(1usize))
            .bench(|| divan::black_box(vaco_parse_av1::codec_parameters(divan::black_box(&sh))));
    }
}

#[divan::bench_group(name = "obu_framing")]
mod obu_framing {
    use super::*;

    /// Splitting the whole stream into OBUs, with no header parsing — the
    /// floor `stream::parse_elementary_stream` is measured against, and the
    /// direct analogue of `vaco-parse-hevc`'s `scan_only`.
    #[divan::bench]
    fn split_only(bencher: divan::Bencher<'_, '_>) {
        let data = &*STREAM;
        bencher.counter(BytesCount::of_slice(data)).bench(|| {
            let mut n = 0usize;
            let mut pos = 0usize;
            while let Some(unit) = next_obu_stream_unit(divan::black_box(data), pos) {
                n += 1;
                pos += unit.total_len;
            }
            n
        });
    }

    /// The allocating variant `CbsCodec::split` uses, for comparison against
    /// the zero-allocation cursor above.
    #[divan::bench]
    fn split_collecting(bencher: divan::Bencher<'_, '_>) {
        let data = &*STREAM;
        bencher
            .counter(BytesCount::of_slice(data))
            .bench_local(|| units(divan::black_box(data), Av1Framing::ObuStream).len());
    }
}

#[divan::bench_group(name = "stream")]
mod stream {
    use super::*;

    /// The whole path: OBU framing, sequence-header parsing, frame-header
    /// peeking for the key-frame flag, temporal-unit splitting. What
    /// `-show_streams` over a raw `.obu` elementary stream costs per byte.
    #[divan::bench]
    fn parse_elementary_stream(bencher: divan::Bencher<'_, '_>) {
        let data = &*STREAM;
        bencher
            .counter(BytesCount::of_slice(data))
            .bench_local(|| feed(divan::black_box(data), data.len()));
    }

    #[divan::bench(args = [1024, 4096, 65536])]
    fn parse_chunked(bencher: divan::Bencher<'_, '_>, chunk: usize) {
        let data = &*STREAM;
        bencher
            .counter(BytesCount::of_slice(data))
            .bench_local(|| feed(divan::black_box(data), chunk));
    }

    fn feed(data: &[u8], chunk: usize) -> usize {
        let mut parser = Av1Parser::new(Limits::permissive());
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
