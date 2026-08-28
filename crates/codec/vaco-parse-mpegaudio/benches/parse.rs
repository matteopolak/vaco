//! What header parsing and resynchronisation cost, measured rather than
//! assumed. Run with `cargo bench -p vaco-parse-mpegaudio`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "benchmark code: fixtures are built in the file, not read from input"
)]

use divan::counter::BytesCount;
use vaco_codec_core::Parser;
use vaco_limits::Limits;
use vaco_parse_mpegaudio::{Ac3Parser, MpegAudioParser};

fn main() {
    divan::main();
}

fn mp3_stream(frames: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..frames {
        let mut frame = vec![0x42u8; 417];
        frame[0] = 0xff;
        frame[1] = 0xfb;
        frame[2] = 0x90;
        frame[3] = 0x00;
        out.extend_from_slice(&frame);
    }
    out
}

fn ac3_stream(frames: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..frames {
        let mut frame = vec![0x42u8; 768];
        frame[0] = 0x0b;
        frame[1] = 0x77;
        frame[4] = 20;
        frame[5] = 8 << 3;
        frame[6] = 0xe1;
        out.extend_from_slice(&frame);
    }
    out
}

fn near_misses(len: usize) -> Vec<u8> {
    let mut out = vec![0x00u8; len];
    let mut i = 0;
    while i + 2 < len {
        out[i] = 0xff;
        out[i + 1] = 0xfb;
        i += 251;
    }
    out
}

#[divan::bench(args = [64, 512])]
fn mp3_stream_frames(bencher: divan::Bencher<'_, '_>, frames: usize) {
    let data = mp3_stream(frames);
    bencher.counter(BytesCount::new(data.len())).bench(|| {
        let mut parser = MpegAudioParser::new(Limits::permissive());
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

#[divan::bench(args = [64, 512])]
fn ac3_stream_frames(bencher: divan::Bencher<'_, '_>, frames: usize) {
    let data = ac3_stream(frames);
    bencher.counter(BytesCount::new(data.len())).bench(|| {
        let mut parser = Ac3Parser::new(Limits::permissive());
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
fn mp3_resync_near_misses(bencher: divan::Bencher<'_, '_>, len: usize) {
    let data = near_misses(len);
    bencher.counter(BytesCount::new(data.len())).bench(|| {
        let mut parser = MpegAudioParser::new(Limits::permissive());
        divan::black_box(parser.parse(&data))
    });
}

#[divan::bench(args = [4096, 65536])]
fn ac3_resync_zeros(bencher: divan::Bencher<'_, '_>, len: usize) {
    let data = vec![0u8; len];
    bencher.counter(BytesCount::new(data.len())).bench(|| {
        let mut parser = Ac3Parser::new(Limits::permissive());
        divan::black_box(parser.parse(&data))
    });
}
