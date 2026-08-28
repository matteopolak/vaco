//! What header parsing costs, measured rather than assumed. None of these
//! codecs frame their own packets (the container does), so there is no
//! resync loop to bench the way `vaco-parse-mpegaudio`/`vaco-parse-aac` need
//! — only the header-parse cost itself.
//!
//! Run with `cargo bench -p vaco-parse-audio-misc`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "benchmark code: fixtures are built in the file, not read from input"
)]

use divan::counter::BytesCount;
use vaco_parse_audio_misc::alac::AlacSpecificConfig;
use vaco_parse_audio_misc::flac::StreamInfo;
use vaco_parse_audio_misc::vorbis::IdentificationHeader;

fn main() {
    divan::main();
}

fn vorbis_ident() -> Vec<u8> {
    let mut v = vec![0x01u8];
    v.extend_from_slice(b"vorbis");
    v.extend_from_slice(&0u32.to_le_bytes()); // version
    v.push(2); // channels
    v.extend_from_slice(&44_100u32.to_le_bytes());
    v.extend_from_slice(&0i32.to_le_bytes()); // bitrate_maximum
    v.extend_from_slice(&128_000i32.to_le_bytes()); // bitrate_nominal
    v.extend_from_slice(&0i32.to_le_bytes()); // bitrate_minimum
    v.push((11 << 4) | 8); // blocksize
    v.push(1); // framing bit
    v
}

fn flac_streaminfo() -> [u8; 34] {
    let mut b = [0u8; 34];
    b[0..2].copy_from_slice(&4608u16.to_be_bytes());
    b[2..4].copy_from_slice(&4608u16.to_be_bytes());
    b[10] = 0x0a;
    b[11] = 0xc4;
    b[12] = 0x42;
    b[13] = 0xf0;
    b[16] = 0xac;
    b[17] = 0x44;
    b
}

const ALAC_COOKIE: [u8; 24] = [
    0, 0, 0x10, 0, 0, 16, 40, 10, 14, 2, 0, 0, 0, 0, 0x40, 4, 0, 0x15, 0x88, 0x80, 0, 0, 0xac, 0x44,
];

#[divan::bench]
fn vorbis_identification_header(bencher: divan::Bencher<'_, '_>) {
    let data = vorbis_ident();
    bencher
        .counter(BytesCount::new(data.len()))
        .bench(|| IdentificationHeader::parse(divan::black_box(&data)));
}

#[divan::bench]
fn flac_streaminfo_bench(bencher: divan::Bencher<'_, '_>) {
    let data = flac_streaminfo();
    bencher
        .counter(BytesCount::new(data.len()))
        .bench(|| StreamInfo::parse(divan::black_box(&data)));
}

#[divan::bench]
fn alac_specific_config(bencher: divan::Bencher<'_, '_>) {
    bencher
        .counter(BytesCount::new(ALAC_COOKIE.len()))
        .bench(|| AlacSpecificConfig::parse(divan::black_box(&ALAC_COOKIE)));
}
