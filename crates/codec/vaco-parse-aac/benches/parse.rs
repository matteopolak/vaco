//! What header parsing costs, measured rather than assumed.
//!
//! A parser runs once per frame, and an AAC frame is 1024 samples — about
//! 23 ms at 44.1 kHz — so a single stream costs roughly 43 header parses per
//! second of audio. That is not a hot loop. What *is* worth measuring is the
//! resynchronisation path: a corrupt or non-AAC input makes the parser scan,
//! and a scan whose per-byte cost is wrong turns a large file into a hang.
//!
//! | Benchmark | What it isolates |
//! |---|---|
//! | `adts_header` | one header, the steady-state cost |
//! | `asc_*` | configuration parsing, by how much extension syntax it carries |
//! | `adts_stream` | framing a clean stream end to end |
//! | `adts_resync_*` | the scan, over inputs that never yield a frame |
//!
//! Run with `cargo bench -p vaco-parse-aac`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "benchmark code: fixtures are built in the file, not read from input"
)]

use divan::counter::BytesCount;
use vaco_codec_core::Parser;
use vaco_limits::Limits;
use vaco_parse_aac::{AdtsHeader, AdtsParser, AudioSpecificConfig};

fn main() {
    divan::main();
}

/// One ADTS header, `aac_frame_length` = 57.
const HEADER: [u8; 7] = [0xff, 0xf1, 0x50, 0x80, 0x07, 0x3f, 0xfc];

/// A synthetic ADTS stream: `n` frames of a fixed size, so the framing cost is
/// separated from any particular payload.
fn adts_stream(frames: usize, frame_len: u16) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..frames {
        let mut header = HEADER;
        header[3] = (header[3] & 0xfc) | ((frame_len >> 11) as u8 & 0x03);
        header[4] = (frame_len >> 3) as u8;
        header[5] = (((frame_len & 0x07) as u8) << 5) | (header[5] & 0x1f);
        out.extend_from_slice(&header);
        out.resize(out.len() + usize::from(frame_len) - 7, 0x42);
    }
    out
}

/// Bytes that offer a sync word every few hundred bytes but never a frame — the
/// shape a scan has to survive.
fn near_misses(len: usize) -> Vec<u8> {
    let mut out = vec![0x00u8; len];
    let mut i = 0;
    while i + 2 < len {
        out[i] = 0xff;
        out[i + 1] = 0xf1;
        i += 251;
    }
    out
}

#[divan::bench]
fn adts_header(bencher: divan::Bencher<'_, '_>) {
    bencher
        .counter(BytesCount::new(HEADER.len()))
        .bench(|| AdtsHeader::parse(divan::black_box(&HEADER)));
}

#[divan::bench(args = [
    ("lc", &[0x12u8, 0x10][..]),
    ("explicit_sbr", &[0x13, 0x90, 0x56, 0xe5, 0xa0][..]),
    ("explicit_ps", &[0x13, 0x88, 0x56, 0xe5, 0xa5, 0x48, 0x80][..]),
    ("hierarchical", &[0x2b, 0x92, 0x08, 0x00][..]),
])]
fn asc(bencher: divan::Bencher<'_, '_>, case: (&str, &[u8])) {
    let bytes = case.1;
    bencher
        .counter(BytesCount::new(bytes.len()))
        .bench(|| AudioSpecificConfig::parse(divan::black_box(bytes)));
}

#[divan::bench(args = [64, 512])]
fn adts_stream_frames(bencher: divan::Bencher<'_, '_>, frames: usize) {
    let data = adts_stream(frames, 384);
    bencher.counter(BytesCount::new(data.len())).bench(|| {
        let mut parser = AdtsParser::new(Limits::permissive());
        let mut offset = 0usize;
        let mut count = 0usize;
        while offset < data.len() {
            let Ok((packet, used)) = parser.parse(&data[offset..]) else {
                break;
            };
            if used == 0 {
                break;
            }
            offset += used;
            if packet.is_some() {
                count += 1;
            }
        }
        divan::black_box(count)
    });
}

#[divan::bench(args = [4096, 65536])]
fn adts_resync_zeros(bencher: divan::Bencher<'_, '_>, len: usize) {
    let data = vec![0u8; len];
    bencher.counter(BytesCount::new(data.len())).bench(|| {
        let mut parser = AdtsParser::new(Limits::permissive());
        divan::black_box(parser.parse(&data))
    });
}

#[divan::bench(args = [4096, 65536])]
fn adts_resync_near_misses(bencher: divan::Bencher<'_, '_>, len: usize) {
    let data = near_misses(len);
    bencher.counter(BytesCount::new(data.len())).bench(|| {
        let mut parser = AdtsParser::new(Limits::permissive());
        divan::black_box(parser.parse(&data))
    });
}
