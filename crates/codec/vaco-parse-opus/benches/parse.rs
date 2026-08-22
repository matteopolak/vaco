//! What Opus header and packet parsing costs.
//!
//! Unlike AAC, Opus packet parsing runs on *every* packet and a packet can be
//! 2.5 ms long, so a single stream can reach 400 parses per second — and a
//! 48-channel ambisonic stream multiplies that by its stream count, because
//! each stream is a separate self-delimited sub-packet. That is the one place
//! in this crate where per-packet cost is worth watching.
//!
//! Run with `cargo bench -p vaco-parse-opus`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "benchmark code: fixtures are built in the file, not read from input"
)]

use divan::counter::BytesCount;
use vaco_parse_opus::{CommentHeader, IdentificationHeader, OpusPacket, split_streams};

fn main() {
    divan::main();
}

const HEAD_STEREO: [u8; 19] = [
    0x4f, 0x70, 0x75, 0x73, 0x48, 0x65, 0x61, 0x64, 0x01, 0x02, 0x38, 0x01, 0x80, 0xbb, 0x00, 0x00,
    0x00, 0x00, 0x00,
];

const HEAD_51: [u8; 27] = [
    0x4f, 0x70, 0x75, 0x73, 0x48, 0x65, 0x61, 0x64, 0x01, 0x06, 0x38, 0x01, 0x80, 0xbb, 0x00, 0x00,
    0x00, 0x00, 0x01, 0x04, 0x02, 0x00, 0x04, 0x01, 0x02, 0x03, 0x05,
];

/// A code-0 packet: TOC plus one frame, the overwhelmingly common shape.
fn code0(payload: usize) -> Vec<u8> {
    let mut out = vec![0xfcu8];
    out.resize(payload + 1, 0x5a);
    out
}

/// A code-3 VBR packet of `frames` frames — the shape with the most length
/// fields to walk.
fn code3_vbr(frames: u8, each: usize) -> Vec<u8> {
    let mut out = vec![0xe4u8, 0x80 | frames];
    for _ in 1..frames {
        out.push(each as u8);
    }
    out.resize(out.len() + each * usize::from(frames), 0x5a);
    out
}

/// A multi-stream packet: `streams - 1` self-delimited sub-packets then one
/// plain one.
fn multistream(streams: usize, each: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..streams {
        out.push(0xfc);
        if i + 1 != streams {
            out.push(each as u8);
        }
        out.resize(out.len() + each, 0x5a);
    }
    out
}

#[divan::bench(args = [("stereo", &HEAD_STEREO[..]), ("5.1", &HEAD_51[..])])]
fn identification_header(bencher: divan::Bencher<'_, '_>, case: (&str, &[u8])) {
    let bytes = case.1;
    bencher
        .counter(BytesCount::new(bytes.len()))
        .bench(|| IdentificationHeader::parse(divan::black_box(bytes)));
}

#[divan::bench]
fn comment_header(bencher: divan::Bencher<'_, '_>) {
    let mut data = b"OpusTags".to_vec();
    data.extend(13u32.to_le_bytes());
    data.extend_from_slice(b"libopus 1.5.2");
    data.extend(4u32.to_le_bytes());
    for tag in [
        "TITLE=a benchmark",
        "ARTIST=nobody",
        "DATE=2026",
        "ALBUM=none",
    ] {
        data.extend((tag.len() as u32).to_le_bytes());
        data.extend_from_slice(tag.as_bytes());
    }
    bencher.counter(BytesCount::new(data.len())).bench(|| {
        let header = CommentHeader::parse(divan::black_box(&data));
        divan::black_box(header.map(|h| h.iter().count()))
    });
}

#[divan::bench(args = [40, 200, 1200])]
fn packet_code0(bencher: divan::Bencher<'_, '_>, payload: usize) {
    let data = code0(payload);
    bencher
        .counter(BytesCount::new(data.len()))
        .bench(|| OpusPacket::parse(divan::black_box(&data)));
}

#[divan::bench(args = [2, 6, 48])]
fn packet_code3_vbr(bencher: divan::Bencher<'_, '_>, frames: u8) {
    let data = code3_vbr(frames, 20);
    bencher
        .counter(BytesCount::new(data.len()))
        .bench(|| OpusPacket::parse(divan::black_box(&data)));
}

#[divan::bench(args = [2, 4, 16])]
fn packet_multistream(bencher: divan::Bencher<'_, '_>, streams: usize) {
    let data = multistream(streams, 40);
    bencher
        .counter(BytesCount::new(data.len()))
        .bench(|| split_streams(divan::black_box(&data), streams));
}
