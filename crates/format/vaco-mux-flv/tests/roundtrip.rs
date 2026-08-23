//! Mux an H.264+AAC pair, then demux it back with `vaco-demux-flv` — the
//! most direct check that the two crates' understanding of the format
//! agrees.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{MediaType, Timestamp};
use vaco_demux_flv::FlvDemuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::vacoraw::{MemorySink, SharedBytes};
use vaco_format_core::{Demuxer, FormatOptions, Muxer};
use vaco_io::MemorySource;
use vaco_limits::Budget;
use vaco_mux_flv::FlvMuxer;
use vaco_packet::{Packet, PacketFlags};

fn video_params(extradata: &[u8]) -> CodecParameters {
    let mut p = CodecParameters::video();
    p.codec_id = Some(CodecId::H264);
    p.extradata = Some(extradata.to_vec());
    p
}

fn audio_params(extradata: &[u8]) -> CodecParameters {
    let mut p = CodecParameters::audio();
    p.codec_id = Some(CodecId::Aac);
    p.extradata = Some(extradata.to_vec());
    p
}

fn packet(stream_index: u32, pts_ms: i64, dts_ms: i64, payload: &[u8], key: bool) -> Packet {
    let mut budget = Budget::new(vaco_limits::Limits::permissive());
    let mut pkt = Packet::from_slice(&mut budget, payload).unwrap();
    pkt.stream_index = stream_index;
    pkt.pts = Timestamp::new(pts_ms);
    pkt.dts = Timestamp::new(dts_ms);
    pkt.flags = if key {
        PacketFlags::KEY
    } else {
        PacketFlags::empty()
    };
    pkt
}

fn mux_sample() -> (Vec<u8>, u32, u32) {
    let sink = MemorySink::new();
    let shared: SharedBytes = sink.shared();
    let mut mux = FlvMuxer::new(Box::new(sink), &FormatOptions::default()).unwrap();

    let v = mux
        .add_stream(&video_params(&[0x01, 0x64, 0x00, 0x0a]))
        .unwrap();
    let a = mux.add_stream(&audio_params(&[0x12, 0x10])).unwrap();
    mux.write_header().unwrap();

    mux.write_packet(&packet(v, 0, 0, &[0xAA; 20], true))
        .unwrap();
    mux.write_packet(&packet(a, 23, 23, &[0xCC; 16], true))
        .unwrap();
    mux.write_packet(&packet(v, 140, 100, &[0xBB; 12], false))
        .unwrap();
    mux.write_trailer().unwrap();

    (shared.snapshot(), v, a)
}

fn open(bytes: Vec<u8>) -> FlvDemuxer {
    let src = Box::new(MemorySource::new(bytes));
    FlvDemuxer::open(src, &NoParsers, &FormatOptions::default()).expect("demux what we muxed")
}

#[test]
fn muxed_streams_demux_with_the_right_shape() {
    let (bytes, v, a) = mux_sample();
    assert_eq!((v, a), (0, 1));
    let mut demux = open(bytes);
    while demux.read_packet().is_ok() {}
    let streams = demux.streams();
    assert_eq!(streams.len(), 2);
    assert_eq!(streams[0].media_type(), Some(MediaType::Video));
    assert_eq!(streams[0].params.codec_id, Some(CodecId::H264));
    assert_eq!(
        streams[0].params.extradata,
        Some(vec![0x01, 0x64, 0x00, 0x0a])
    );
    assert_eq!(streams[1].media_type(), Some(MediaType::Audio));
    assert_eq!(streams[1].params.codec_id, Some(CodecId::Aac));
    assert_eq!(streams[1].params.extradata, Some(vec![0x12, 0x10]));
}

#[test]
fn muxed_packets_demux_with_the_right_timestamps() {
    let (bytes, _v, _a) = mux_sample();
    let mut demux = open(bytes);
    let mut got = Vec::new();
    loop {
        match demux.read_packet() {
            Ok(p) => got.push((
                p.stream_index,
                p.pts.ticks(),
                p.dts.ticks(),
                p.is_key(),
                p.len,
            )),
            Err(vaco_core::Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    assert_eq!(
        got,
        vec![
            (0, Some(0), Some(0), true, 20),
            (1, Some(23), Some(23), true, 16),
            (0, Some(140), Some(100), false, 12),
        ]
    );
}

#[test]
fn a_codec_with_no_flv_framing_is_rejected() {
    let sink = MemorySink::new();
    let mut mux = FlvMuxer::new(Box::new(sink), &FormatOptions::default()).unwrap();
    let mut p = CodecParameters::video();
    p.codec_id = Some(CodecId::Png); // a real video codec, but no FLV framing
    assert!(mux.add_stream(&p).is_err());
}

#[test]
fn a_second_video_stream_is_rejected() {
    let sink = MemorySink::new();
    let mut mux = FlvMuxer::new(Box::new(sink), &FormatOptions::default()).unwrap();
    mux.add_stream(&video_params(&[])).unwrap();
    assert!(mux.add_stream(&video_params(&[])).is_err());
}
