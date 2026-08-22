//! Where the time goes when reading a Matroska file.
//!
//! Three levels, because a regression in any one of them looks the same from
//! outside: the VINT primitives, the schema lookup that unknown-size
//! termination calls per element, and a whole-file demux.
//!
//! `cargo bench -p vaco-demux-matroska`

use vaco_demux_matroska::MatroskaDemuxer;
use vaco_demux_matroska::ebml::{self, schema as el};
use vaco_demux_matroska::synth::{self, SegmentSize};
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::{MediaSource, MemorySource};

fn main() {
    divan::main();
}

fn audio_track() -> Vec<u8> {
    let mut audio = synth::float(el::SAMPLINGFREQUENCY, 48000.0);
    audio.extend_from_slice(&synth::uint(el::CHANNELS, 2));
    let mut body = synth::uint(el::TRACKNUMBER, 1);
    body.extend_from_slice(&synth::uint(el::TRACKUID, 1));
    body.extend_from_slice(&synth::uint(el::TRACKTYPE, 2));
    body.extend_from_slice(&synth::string(el::CODECID, "A_PCM/INT/LIT"));
    body.extend_from_slice(&synth::uint(el::FLAGLACING, 1));
    body.extend_from_slice(&synth::uint(el::DEFAULTDURATION, 20_000_000));
    body.extend_from_slice(&synth::element(el::AUDIO, &audio));
    synth::element(el::TRACKENTRY, &body)
}

/// `clusters` clusters of `blocks` unlaced blocks each.
fn file(clusters: usize, blocks: usize, lacing: Option<u8>, size: SegmentSize) -> Vec<u8> {
    let frame = vec![0x5A; 256];
    let laced: Vec<&[u8]> = vec![&frame; 8];
    let built: Vec<Vec<u8>> = (0..clusters)
        .map(|c| {
            let children: Vec<Vec<u8>> = (0..blocks)
                .map(|b| {
                    let payload = match lacing {
                        Some(0x02) => synth::xiph_lace(&laced),
                        Some(0x06) => synth::ebml_lace(&laced),
                        Some(0x04) => synth::fixed_lace(&laced),
                        _ => frame.clone(),
                    };
                    synth::element(
                        el::SIMPLEBLOCK,
                        &synth::block_body(
                            1,
                            i16::try_from(b).unwrap_or(0),
                            0x80 | lacing.unwrap_or(0),
                            &payload,
                        ),
                    )
                })
                .collect();
            synth::cluster((c * 1000) as u64, &children, size)
        })
        .collect();
    synth::file(
        "matroska",
        &synth::uint(el::TIMESTAMPSCALE, 1_000_000),
        &audio_track(),
        &built,
        size,
    )
}

fn count_packets(bytes: &[u8]) -> usize {
    let src: Box<dyn MediaSource> = Box::new(MemorySource::new(bytes.to_vec()));
    let Ok(mut d) = MatroskaDemuxer::open(src, &NoParsers, &FormatOptions::default()) else {
        return 0;
    };
    let mut n = 0;
    while d.read_packet().is_ok() {
        n += 1;
    }
    n
}

// --------------------------------------------------------------- primitives

#[divan::bench]
fn read_size_one_octet(bencher: divan::Bencher<'_, '_>) {
    let bytes = synth::vint_min(100);
    bencher.bench(|| ebml::read_size(divan::black_box(&bytes), 8));
}

#[divan::bench]
fn read_size_eight_octets(bencher: divan::Bencher<'_, '_>) {
    let bytes = synth::vint(1 << 40, 8);
    bencher.bench(|| ebml::read_size(divan::black_box(&bytes), 8));
}

/// The lookup unknown-size termination performs for every element it sees.
#[divan::bench]
fn schema_lookup(bencher: divan::Bencher<'_, '_>) {
    bencher.bench(|| {
        (
            ebml::lookup(divan::black_box(el::SIMPLEBLOCK)),
            ebml::lookup(divan::black_box(el::MASTERINGMETADATA)),
            ebml::lookup(divan::black_box(0x1234_5678)),
        )
    });
}

// ------------------------------------------------------------------- lacing

#[divan::bench(args = [0x00u8, 0x02, 0x04, 0x06])]
fn lace(bencher: divan::Bencher<'_, '_>, flags: u8) {
    let frame = vec![0x5A; 256];
    let frames: Vec<&[u8]> = vec![&frame; 8];
    let payload = match flags {
        0x02 => synth::xiph_lace(&frames),
        0x06 => synth::ebml_lace(&frames),
        0x04 => synth::fixed_lace(&frames),
        _ => frame.clone(),
    };
    let data = synth::block_body(1, 0, 0x80 | flags, &payload);
    let Ok(header) = vaco_demux_matroska::block::parse_header(&data, true) else {
        return;
    };
    bencher.bench(|| vaco_demux_matroska::block::frames(divan::black_box(&data), &header));
}

// ------------------------------------------------------------- whole file

#[divan::bench]
fn demux_known_size(bencher: divan::Bencher<'_, '_>) {
    let bytes = file(64, 32, None, SegmentSize::Known);
    bencher
        .counter(divan::counter::BytesCount::of_slice(&bytes))
        .bench(|| count_packets(divan::black_box(&bytes)));
}

/// The streaming path, where every element consults the schema to decide
/// whether it ends the open cluster.
#[divan::bench]
fn demux_unknown_size(bencher: divan::Bencher<'_, '_>) {
    let bytes = file(64, 32, None, SegmentSize::Unknown);
    bencher
        .counter(divan::counter::BytesCount::of_slice(&bytes))
        .bench(|| count_packets(divan::black_box(&bytes)));
}

#[divan::bench]
fn demux_ebml_laced(bencher: divan::Bencher<'_, '_>) {
    let bytes = file(16, 32, Some(0x06), SegmentSize::Known);
    bencher
        .counter(divan::counter::BytesCount::of_slice(&bytes))
        .bench(|| count_packets(divan::black_box(&bytes)));
}
