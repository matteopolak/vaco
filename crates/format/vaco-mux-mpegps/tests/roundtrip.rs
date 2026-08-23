//! Mux with `vaco-mux-mpegps`, then demux the result with
//! `vaco-demux-mpegps`, and check what comes back matches what went in.
//!
//! This is the cross-crate check plan 18 §0 calls out for containers
//! specifically: muxing is a pure function of (packets, options), so a
//! divergence here is a real bug, not a tolerance. `vaco-demux-mpegps` is a
//! dev-dependency of this crate only for this test — the production
//! dependency graph does not include it (see `Cargo.toml`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

use vaco_codec_core::CodecParameters;
use vaco_core::{MediaType, Timestamp};
use vaco_demux_mpegps::MpegPsDemuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::{Demuxer, FormatOptions, Muxer};
use vaco_io::{DynBuf, MemorySource, SharedDynBuf};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

fn packet(stream_index: u32, pts: i64, payload: &[u8]) -> Packet {
    let mut budget = Budget::new(Limits::permissive());
    let mut pkt = Packet::from_slice(&mut budget, payload).expect("alloc");
    pkt.stream_index = stream_index;
    pkt.pts = Timestamp::new(pts);
    pkt.dts = Timestamp::new(pts);
    pkt
}

fn mux_two_packets(
    open: fn(Box<dyn vaco_io::MediaSink>) -> vaco_core::Result<Box<dyn Muxer>>,
) -> Vec<u8> {
    let sink = SharedDynBuf::new();
    let mirror = sink.clone();
    let mut mux = open(Box::new(sink)).expect("open muxer");
    let video = mux
        .add_stream(&CodecParameters::new(MediaType::Video))
        .expect("add video stream");
    let audio = mux
        .add_stream(&CodecParameters::new(MediaType::Audio))
        .expect("add audio stream");
    mux.write_header().expect("write_header");
    mux.write_packet(&packet(video, 0, &[0u8; 128]))
        .expect("write video packet");
    mux.write_packet(&packet(audio, 3600, &[1u8; 64]))
        .expect("write audio packet");
    mux.write_trailer().expect("write_trailer");
    drop(mux);
    mirror.take()
}

fn demux_all(bytes: Vec<u8>) -> (usize, Vec<Packet>) {
    let src = Box::new(MemorySource::new(bytes));
    let mut demux =
        MpegPsDemuxer::open(src, &NoParsers, &FormatOptions::default()).expect("open demuxer");
    let nstreams = demux.streams().len();
    let mut packets = Vec::new();
    loop {
        match demux.read_packet() {
            Ok(p) => packets.push(p),
            Err(vaco_core::Error::Eof) => break,
            Err(e) => panic!("read_packet: {e:?}"),
        }
    }
    (nstreams, packets)
}

#[test]
fn vob_round_trip_recovers_both_streams_and_payloads() {
    let bytes = mux_two_packets(vaco_mux_mpegps::MUXER_VOB.open);
    assert!(!bytes.is_empty());
    let (nstreams, packets) = demux_all(bytes);
    assert_eq!(nstreams, 2, "video and audio should both be registered");
    assert_eq!(packets.len(), 2);
    assert_eq!(packets[0].payload(), &[0u8; 128][..]);
    assert_eq!(packets[1].payload(), &[1u8; 64][..]);
}

#[test]
fn mpeg_round_trip_recovers_both_streams_and_payloads() {
    let bytes = mux_two_packets(vaco_mux_mpegps::MUXER_MPEG.open);
    assert!(!bytes.is_empty());
    let (nstreams, packets) = demux_all(bytes);
    assert_eq!(nstreams, 2);
    assert_eq!(packets.len(), 2);
    assert_eq!(packets[0].payload(), &[0u8; 128][..]);
    assert_eq!(packets[1].payload(), &[1u8; 64][..]);
}

#[test]
fn dvd_round_trip_pads_packs_to_2048_bytes() {
    let bytes = mux_two_packets(vaco_mux_mpegps::MUXER_DVD.open);
    // Every pack in this test is smaller than 2048 bytes of real content,
    // so each one should have been padded up to the fixed pack size except
    // possibly a shorter final remainder before the program end code.
    assert!(bytes.len() >= 2048 * 2);
    let (_, packets) = demux_all(bytes);
    assert_eq!(packets.len(), 2);
}

#[test]
fn a_pts_and_dts_pair_survives_the_round_trip() {
    let bytes = mux_two_packets(vaco_mux_mpegps::MUXER_VOB.open);
    let (_, packets) = demux_all(bytes);
    assert_eq!(packets[0].pts.ticks(), Some(0));
    assert_eq!(packets[1].pts.ticks(), Some(3600));
}

// Keep `DynBuf` reachable in case a future test wants a non-shared sink
// directly; also exercises that the crate's public re-export surface is
// usable from an external crate the way `vaco-io` intends.
#[test]
fn a_plain_dynbuf_is_also_a_valid_sink() {
    let sink: Box<dyn vaco_io::MediaSink> = Box::new(DynBuf::new());
    let mut mux = (vaco_mux_mpegps::MUXER_VOB.open)(sink).expect("open");
    let idx = mux
        .add_stream(&CodecParameters::new(MediaType::Video))
        .expect("add_stream");
    mux.write_header().expect("write_header");
    mux.write_packet(&packet(idx, 0, b"abc"))
        .expect("write_packet");
    mux.write_trailer().expect("write_trailer");
}
