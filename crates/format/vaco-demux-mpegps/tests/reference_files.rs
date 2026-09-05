//! Demux real `ffmpeg -f vob` / `-f mpeg` output (D17/plan 13 §1b: measure,
//! do not recall).
//!
//! `tests/fixtures/vob_sample.vob` is the first 20480 bytes (10 packs) of
//! `ffmpeg -f lavfi -i testsrc=size=352x288:rate=25:duration=1 -f lavfi -i
//! sine=frequency=1000:duration=1 -c:v mpeg2video -c:a ac3 -f vob out.vob`,
//! captured 2026-08-23 with ffmpeg 8.1. It carries one MPEG-2 video stream
//! (`stream_id` `0xE0`) and one AC-3 audio stream multiplexed through
//! `private_stream_1` (`0xBD`, sub-id `0x80`).
//!
//! `tests/fixtures/mpeg1_sample.mpg` is the first 65536 bytes of the same
//! command with `-c:a mp2 -f mpeg`, which the reference implements as an
//! MPEG-1 Systems stream (verified in `pack.rs`/`pes.rs`'s unit tests):
//! video on `stream_id` `0xE0`, MPEG audio directly on `0xC0` (no private
//! stream needed — MP2 has a plain systems-layer stream id).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use vaco_core::{Duration, MediaType};
use vaco_demux_mpegps::MpegPsDemuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::MemorySource;

fn open(bytes: &'static [u8]) -> MpegPsDemuxer {
    let src = Box::new(MemorySource::new(bytes.to_vec()));
    MpegPsDemuxer::open(src, &NoParsers, &FormatOptions::default()).expect("open")
}

#[test]
fn a_real_vob_sample_yields_video_and_ac3_audio_streams() {
    let bytes: &'static [u8] = include_bytes!("fixtures/vob_sample.vob");
    let demux = open(bytes);
    let streams = demux.streams();
    assert!(
        streams
            .iter()
            .any(|s| s.params.media_type == Some(MediaType::Video)),
        "expected a video stream, got {streams:?}"
    );
    assert!(
        streams
            .iter()
            .any(|s| s.params.media_type == Some(MediaType::Audio)),
        "expected the AC-3 substream to register as audio, got {streams:?}"
    );
    // The AC-3 substream's synthesised id is `0xBD00 | 0x80` (private_stream_1,
    // sub-id 0x80 per `substream::classify`'s AC-3 range).
    assert!(
        streams.iter().any(|s| s.id == Some(0xBD00 | 0x80)),
        "expected the AC-3 substream's synthesised id, got {:?}",
        streams.iter().map(|s| s.id).collect::<Vec<_>>()
    );
}

#[test]
fn a_real_vob_sample_yields_at_least_one_packet_per_stream() {
    let bytes: &'static [u8] = include_bytes!("fixtures/vob_sample.vob");
    let mut demux = open(bytes);
    let nstreams = demux.streams().len();
    let mut seen = vec![false; nstreams];
    let mut total = 0u32;
    loop {
        match demux.read_packet() {
            Ok(p) => {
                if let Some(slot) = seen.get_mut(p.stream_index as usize) {
                    *slot = true;
                }
                total += 1;
                assert!(
                    !p.payload().is_empty(),
                    "packet {total} has an empty payload"
                );
            }
            Err(vaco_core::Error::Eof) => break,
            Err(e) => panic!("read_packet: {e:?}"),
        }
    }
    assert!(total > 0);
    assert!(
        seen.iter().all(|&s| s),
        "every registered stream should have produced a packet: {seen:?}"
    );
}

#[test]
fn a_real_mpeg1_systems_sample_uses_the_mpeg1_pes_envelope_throughout() {
    let bytes: &'static [u8] = include_bytes!("fixtures/mpeg1_sample.mpg");
    let mut demux = open(bytes);
    assert!(
        demux
            .streams()
            .iter()
            .any(|s| s.params.media_type == Some(MediaType::Video))
    );
    assert!(
        demux
            .streams()
            .iter()
            .any(|s| s.params.media_type == Some(MediaType::Audio))
    );
    // If the MPEG-1/MPEG-2 PES-syntax dispatch in `pes.rs` were wrong, this
    // would misframe by several bytes and either error out or return
    // garbage-sized packets well before end of file.
    let mut total = 0u32;
    loop {
        match demux.read_packet() {
            Ok(p) => {
                assert!(
                    p.payload().len() < 1 << 20,
                    "a misframed packet would be huge"
                );
                total += 1;
            }
            Err(vaco_core::Error::Eof) => break,
            Err(e) => panic!("read_packet: {e:?}"),
        }
    }
    assert!(
        total > 5,
        "expected several packets from a 64 KiB sample, got {total}"
    );
}

#[test]
fn a_real_mpeg1_systems_sample_keeps_the_scr_duration_exact() {
    let bytes: &'static [u8] = include_bytes!("fixtures/mpeg1_sample.mpg");
    let demux = open(bytes);

    // The seven pack headers span 59,374 ticks of MPEG-PS's 90 kHz SCR
    // clock. Both APIs must retain the source clock's fraction.
    assert_eq!(
        demux.duration().map(Duration::as_ratio),
        Some((29_687, 45_000))
    );
    assert_eq!(
        demux
            .duration_exact()
            .map(vaco_core::ExactDuration::as_ratio),
        Some((29_687, 45_000))
    );
}
