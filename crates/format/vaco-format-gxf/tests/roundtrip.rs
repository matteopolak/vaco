//! Round-trips `GxfMuxer`'s own output through `GxfDemuxer` — the same
//! "measure the two halves against each other" verification
//! `vaco-mux-mxf`'s own test suite uses, since neither side interprets
//! essence content, only frames it: synthetic payload bytes are enough to
//! prove packet counts, positions, field numbers and codec parameters
//! survive the round trip.
//!
//! There is no automated cross-check against real `ffmpeg` here (this
//! workspace's tests must be self-contained and reproducible without an
//! external binary installed) — that check was done manually against this
//! machine's `ffmpeg 8.1` during development and is recorded in
//! `planning/TECH-DEBT.md`, the same posture every other muxer in this
//! workspace's test suite takes for the same reason.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::cast_possible_wrap,
    reason = "test code"
)]

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{MediaType, Rational, Timestamp};
use vaco_format_core::{Demuxer, Muxer};
use vaco_format_gxf::{GxfDemuxer, GxfMuxer};
use vaco_io::{MemorySource, SharedDynBuf};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

fn video_params() -> CodecParameters {
    let mut p = CodecParameters::video();
    p.codec_id = Some(CodecId::Mpeg2video);
    if let Some(v) = p.video.as_mut() {
        v.width = 720;
        v.height = 576;
        v.frame_rate = Rational::new(25, 1);
    }
    p
}

fn audio_params() -> CodecParameters {
    let mut p = CodecParameters::audio();
    p.codec_id = Some(CodecId::PcmS16le);
    if let Some(a) = p.audio.as_mut() {
        a.sample_rate = 48_000;
        a.layout = vaco_chlayout::ChannelLayout::default_for(1);
        a.bits_per_coded_sample = Some(16);
    }
    p
}

fn packet(stream_index: u32, bytes: &[u8], key: bool) -> Packet {
    let mut budget = Budget::new(Limits::permissive());
    let mut pkt = Packet::from_slice(&mut budget, bytes).unwrap();
    pkt.stream_index = stream_index;
    if key {
        pkt.flags |= PacketFlags::KEY;
    }
    pkt
}

#[test]
fn a_video_and_audio_file_round_trips_through_the_sibling_demuxer() {
    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let mut mux = GxfMuxer::new(Box::new(sink.clone()));
    let video_idx = mux.add_stream(&video_params()).unwrap();
    let audio_idx = mux.add_stream(&audio_params()).unwrap();
    mux.write_header().unwrap();

    // Five video frames, one key then four non-key, distinct sizes so a
    // wrong offset shows up as a wrong length.
    let frame_sizes: [(usize, bool); 5] = [(4000, true), (900, false), (800, false), (750, false), (820, false)];
    for &(size, key) in &frame_sizes {
        let payload = vec![0xAB; size];
        mux.write_packet(&packet(video_idx, &payload, key)).unwrap();
    }

    // 70,000 audio bytes: one full 65,536-byte packet plus one short
    // (partial) final packet — exercises the padding/valid-sample path.
    let audio_bytes = vec![0x11u8; 70_000];
    mux.write_packet(&packet(audio_idx, &audio_bytes, true)).unwrap();

    mux.write_trailer().unwrap();
    let bytes = sink.snapshot();
    assert!(!bytes.is_empty());

    let mut demux = GxfDemuxer::open(Box::new(MemorySource::new(bytes)), &vaco_format_core::discovery::NoParsers).unwrap();
    assert_eq!(demux.streams().len(), 2);
    assert_eq!(demux.streams()[0].media_type(), Some(MediaType::Video));
    assert_eq!(demux.streams()[0].params.codec_id, Some(CodecId::Mpeg2video));
    assert_eq!(demux.streams()[1].media_type(), Some(MediaType::Audio));
    assert_eq!(demux.streams()[1].params.codec_id, Some(CodecId::PcmS16le));
    for s in demux.streams() {
        assert_eq!(s.time_base, Rational::new(1, 50));
    }

    // Reading order: MAP -> UMF -> video packets (this muxer writes all
    // video before all audio) -> audio packets -> EOS.
    for (i, &(size, key)) in frame_sizes.iter().enumerate() {
        let pkt = demux.read_packet().unwrap();
        assert_eq!(pkt.stream_index, video_idx);
        assert_eq!(pkt.payload().len(), size);
        assert_eq!(pkt.payload(), vec![0xAB; size].as_slice());
        assert_eq!(pkt.pts, Timestamp::new((i * 2) as i64));
        assert_eq!(pkt.flags.contains(PacketFlags::KEY), key);
    }

    let audio1 = demux.read_packet().unwrap();
    assert_eq!(audio1.stream_index, audio_idx);
    assert_eq!(audio1.payload().len(), 65_536);
    assert_eq!(audio1.payload(), &audio_bytes[..65_536]);

    let audio2 = demux.read_packet().unwrap();
    assert_eq!(audio2.stream_index, audio_idx);
    assert_eq!(audio2.payload().len(), 65_536); // padded to the fixed packet size.
    assert_eq!(&audio2.payload()[..4_464], &audio_bytes[65_536..]);
    assert_eq!(&audio2.payload()[4_464..], vec![0u8; 65_536 - 4_464].as_slice());

    assert!(matches!(demux.read_packet(), Err(vaco_core::Error::Eof)));
}

#[test]
fn adding_a_second_video_stream_is_rejected() {
    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let mut mux = GxfMuxer::new(Box::new(sink.clone()));
    mux.add_stream(&video_params()).unwrap();
    assert!(mux.add_stream(&video_params()).is_err());
}

#[test]
fn an_unsupported_frame_rate_is_rejected() {
    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let mut mux = GxfMuxer::new(Box::new(sink.clone()));
    let mut params = video_params();
    params.video.as_mut().unwrap().frame_rate = Rational::new(13, 1);
    assert!(mux.add_stream(&params).is_err());
}
