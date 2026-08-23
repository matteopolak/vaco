//! End-to-end demuxing of a hand-built AVI file: one video stream
//! (`dwSampleSize == 0`) and one audio stream (`dwSampleSize != 0`),
//! interleaved in `movi`, with a trailing `idx1` using the movi-relative
//! offset convention this crate measured against `ffmpeg 8.1` (see
//! `src/index.rs`'s module docs).
//!
//! This is not `ffmpeg`'s own byte layout reproduced — it is this crate's
//! understanding of the specification, built independently, which is exactly
//! what should demux correctly if that understanding is right.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::trivially_copy_pass_by_ref,
    reason = "test code"
)]

use vaco_core::{MediaType, Timestamp};
use vaco_demux_avi::AviDemuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::MemorySource;

fn chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(id);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    if data.len() % 2 == 1 {
        out.push(0);
    }
    out
}

fn list(list_type: &[u8; 4], children: &[u8]) -> Vec<u8> {
    let mut payload = list_type.to_vec();
    payload.extend_from_slice(children);
    chunk(b"LIST", &payload)
}

fn avih(streams: u32, total_frames: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&100_000u32.to_le_bytes()); // 10 fps
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0x10u32.to_le_bytes()); // AVIF_HASINDEX
    out.extend_from_slice(&total_frames.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&streams.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&64u32.to_le_bytes());
    out.extend_from_slice(&48u32.to_le_bytes());
    out.extend_from_slice(&[0; 16]);
    out
}

fn strh(fcc_type: &[u8; 4], scale: u32, rate: u32, length: u32, sample_size: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(fcc_type);
    out.extend_from_slice(b"FMP4");
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&scale.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&length.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&sample_size.to_le_bytes());
    out.extend_from_slice(&[0; 8]);
    out
}

fn strf_video() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&64i32.to_le_bytes());
    out.extend_from_slice(&48i32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(b"FMP4");
    out.extend_from_slice(&[0; 20]);
    out
}

fn strf_audio() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_le_bytes()); // WAVE_FORMAT_PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&8000u32.to_le_bytes());
    out.extend_from_slice(&16000u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes());
    out
}

/// Build a minimal but structurally faithful AVI file: `hdrl` (one video
/// stream at 10 fps, one audio stream at 8000 Hz mono 16-bit PCM), `movi`
/// with three video chunks and two audio chunks, and `idx1`.
fn build_avi() -> Vec<u8> {
    let strl_v = list(b"strl", &{
        let mut c = chunk(b"strh", &strh(b"vids", 1, 10, 3, 0));
        c.extend_from_slice(&chunk(b"strf", &strf_video()));
        c
    });
    let strl_a = list(b"strl", &{
        let mut c = chunk(b"strh", &strh(b"auds", 1, 8000, 2000, 2));
        c.extend_from_slice(&chunk(b"strf", &strf_audio()));
        c
    });
    let mut hdrl_children = chunk(b"avih", &avih(2, 3));
    hdrl_children.extend_from_slice(&strl_v);
    hdrl_children.extend_from_slice(&strl_a);
    let hdrl = list(b"hdrl", &hdrl_children);

    // movi: v0(key) a0 v1 a1 v2(non-key marked via idx1)
    let v0 = chunk(b"00dc", &[0xAA; 10]);
    let a0 = chunk(b"01wb", &[0u8; 4000]); // 2000 samples * 2 bytes
    let v1 = chunk(b"00dc", &[0xBB; 8]);
    let a1 = chunk(b"01wb", &[0u8; 4000]);
    let v2 = chunk(b"00dc", &[0xCC; 6]);
    let mut movi_children = Vec::new();
    let mut offsets = Vec::new(); // (fourcc, flags, offset_from_movi_fourcc, size)
    let mut movi_relative = 4u32; // right after the "movi" 4-byte marker
    for (id, data, key) in [
        (*b"00dc", &v0, true),
        (*b"01wb", &a0, true),
        (*b"00dc", &v1, false),
        (*b"01wb", &a1, true),
        (*b"00dc", &v2, false),
    ] {
        let size = (data.len() - 8 - usize::from(data.len() % 2 == 1)) as u32;
        offsets.push((id, u32::from(key) * 0x10, movi_relative, size));
        movi_relative += data.len() as u32;
        movi_children.extend_from_slice(data);
    }
    let movi = list(b"movi", &movi_children);

    let mut idx1_payload = Vec::new();
    for (id, flags, offset, size) in offsets {
        idx1_payload.extend_from_slice(&id);
        idx1_payload.extend_from_slice(&flags.to_le_bytes());
        idx1_payload.extend_from_slice(&offset.to_le_bytes());
        idx1_payload.extend_from_slice(&size.to_le_bytes());
    }
    let idx1 = chunk(b"idx1", &idx1_payload);

    let mut body = b"AVI ".to_vec();
    body.extend_from_slice(&hdrl);
    body.extend_from_slice(&movi);
    body.extend_from_slice(&idx1);

    let mut file = b"RIFF".to_vec();
    file.extend_from_slice(&(body.len() as u32).to_le_bytes());
    file.extend_from_slice(&body);
    file
}

fn open(bytes: Vec<u8>) -> AviDemuxer {
    let src = Box::new(MemorySource::new(bytes));
    AviDemuxer::open(src, &NoParsers, &FormatOptions::default()).expect("open")
}

#[test]
fn streams_are_discovered_with_the_right_media_types_and_dimensions() {
    let demux = open(build_avi());
    let streams = demux.streams();
    assert_eq!(streams.len(), 2);
    assert_eq!(streams[0].media_type(), Some(MediaType::Video));
    assert_eq!(streams[0].params.video.as_ref().unwrap().width, 64);
    assert_eq!(streams[1].media_type(), Some(MediaType::Audio));
    assert_eq!(streams[1].params.audio.as_ref().unwrap().sample_rate, 8000);
    // Measured against ffprobe 8.1: AVI streams report no container id.
    assert_eq!(streams[0].id, None);
}

#[test]
fn packet_order_and_timestamps_follow_the_measured_clock_rules() {
    let mut demux = open(build_avi());
    let mut got = Vec::new();
    loop {
        match demux.read_packet() {
            Ok(p) => got.push((p.stream_index, p.pts.ticks(), p.is_key(), p.len)),
            Err(vaco_core::Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    // video: dwSampleSize == 0, so pts is a running chunk count (0, 1, 2).
    // audio: dwSampleSize == 2, so pts is bytes-so-far / 2 (0, 2000).
    assert_eq!(
        got,
        vec![
            (0, Some(0), true, 10),
            (1, Some(0), true, 4000),
            (0, Some(1), false, 8),
            (1, Some(2000), true, 4000),
            (0, Some(2), false, 6),
        ]
    );
}

#[test]
fn eof_is_sticky() {
    let mut demux = open(build_avi());
    loop {
        if matches!(demux.read_packet(), Err(vaco_core::Error::Eof)) {
            break;
        }
    }
    assert!(matches!(demux.read_packet(), Err(vaco_core::Error::Eof)));
    assert!(matches!(demux.read_packet(), Err(vaco_core::Error::Eof)));
}

#[test]
fn the_idx1_offset_ambiguity_is_resolved_movi_relative_for_this_file() {
    let demux = open(build_avi());
    // Every idx1 entry in `build_avi` was written movi-relative (matching
    // what this crate measured from ffmpeg 8.1); if `detect_offset_base` had
    // picked `Absolute` instead, the index would be empty or wrong.
    assert!(!demux.index().is_empty());
}

#[test]
fn seeking_to_a_keyframe_timestamp_lands_on_the_video_stream() {
    let mut demux = open(build_avi());
    // Drain once to be sure the file parses linearly before seeking.
    while demux.read_packet().is_ok() {}

    let target = SeekTarget::Timestamp {
        stream_index: 0,
        ts: Timestamp::new(0),
    };
    demux.seek(target, SeekFlags::empty()).expect("seek");
    let first = demux.read_packet().expect("packet after seek");
    assert!(first.is_key());
}

#[test]
fn byte_seek_resyncs_to_a_chunk_boundary() {
    let mut demux = open(build_avi());
    // Seeking to byte 0 must resync forward to the first real chunk inside
    // `movi`, not fail or return garbage.
    demux
        .seek(SeekTarget::Byte(0), SeekFlags::empty())
        .expect("byte seek");
    let pkt = demux.read_packet().expect("packet after byte seek");
    assert!(pkt.stream_index == 0 || pkt.stream_index == 1);
}
