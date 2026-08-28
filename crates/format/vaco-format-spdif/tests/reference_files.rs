//! Demux real `ffmpeg -f spdif` output and remux it, checking for byte
//! identity (D17: measure, do not recall).
//!
//! `tests/fixtures/sample.spdif` is
//!
//! ```sh
//! ffmpeg -f lavfi -i "sine=frequency=1000:duration=1:sample_rate=48000" \
//!   -ac 2 -c:a ac3 -b:a 192k -frames:a 4 small_ac3.ac3
//! ffmpeg -i small_ac3.ac3 -c copy -bitexact sample.spdif
//! ```
//!
//! captured 2026-08-27 with ffmpeg 8.1: four real AC-3 frames, each in its
//! own 6144-byte IEC 61937 burst — four bursts because a single one cannot
//! show that the fixed burst size (not a scan) is what actually delimits
//! frames, and this fixture also has real non-zero AC-3 payload bytes for
//! every one of them, not just the first.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use vaco_format_core::{Demuxer, Muxer};
use vaco_format_spdif::{SpdifDemuxer, SpdifMuxer};
use vaco_io::{MemorySource, SharedDynBuf};

const SAMPLE: &[u8] = include_bytes!("fixtures/sample.spdif");

#[test]
fn a_real_sample_reports_ac3_stereo_48khz() {
    let src = Box::new(MemorySource::new(SAMPLE.to_vec()));
    let demux = SpdifDemuxer::open(src).expect("open");
    let audio = demux.streams().first().expect("an audio stream");
    assert_eq!(audio.params.codec_id, Some(vaco_codec_core::CodecId::Ac3));
    let ap = audio.params.audio.as_ref().expect("audio parameters");
    assert_eq!(ap.sample_rate, 48_000);
    assert_eq!(
        ap.layout.as_ref().and_then(vaco_chlayout::ChannelLayout::name),
        Some("stereo")
    );
}

#[test]
fn a_real_sample_yields_exactly_four_keyframe_packets() {
    let src = Box::new(MemorySource::new(SAMPLE.to_vec()));
    let mut demux = SpdifDemuxer::open(src).expect("open");
    let mut count = 0u32;
    loop {
        match demux.read_packet() {
            Ok(p) => {
                assert_eq!(p.stream_index, 0);
                assert!(p.flags.contains(vaco_packet::PacketFlags::KEY));
                assert!(p.payload().starts_with(&[0x0B, 0x77]));
                count += 1;
            }
            Err(vaco_core::Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    assert_eq!(count, 4);
}

/// The distinguishing check for this format: demux the reference's own
/// output and remux it with this crate's muxer using the default
/// little-endian byte order (which is what the reference file was produced
/// with), then compare bytes.
#[test]
fn remuxing_a_real_sample_reproduces_it_byte_for_byte() {
    let src = Box::new(MemorySource::new(SAMPLE.to_vec()));
    let mut demux = SpdifDemuxer::open(src).expect("open");

    let sink = SharedDynBuf::new();
    let mirror = sink.clone();
    let mut mux = SpdifMuxer::new(Box::new(sink));
    let audio = demux.streams().first().expect("an audio stream").clone();
    mux.add_stream(&audio.params).expect("add_stream");
    mux.write_header().expect("write_header");

    loop {
        match demux.read_packet() {
            Ok(p) => mux.write_packet(&p).expect("write_packet"),
            Err(vaco_core::Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    mux.write_trailer().expect("write_trailer");

    assert_eq!(mirror.take(), SAMPLE);
}
