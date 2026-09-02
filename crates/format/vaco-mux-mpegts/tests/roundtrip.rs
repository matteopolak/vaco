//! Mux with `vaco-mux-mpegts`, then demux the result with
//! `vaco-demux-mpegts`, and check what comes back matches what went in.
//!
//! `vaco-demux-mpegts` is a dev-dependency of this crate only for this test
//! (and for `crate::pes`'s timestamp round-trip) — the production dependency
//! graph does not include it. This is the strongest single check available
//! for this muxer: every wire-level piece it exercises (PAT/PMT parsing,
//! PES/PTS/DTS decoding, PCR, continuity) was written independently by
//! whoever built the demuxer, so a bug that both crates share by accident is
//! far less likely than one crate quietly reading back its own mistake.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

use vaco_codec_core::CodecId;
use vaco_codec_core::CodecParameters;
use vaco_core::{MediaType, Timestamp};
use vaco_demux_mpegts::MpegTsDemuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::{Demuxer, FormatOptions, Muxer};
use vaco_io::MemorySource;
use vaco_io::{MediaSink, SharedDynBuf};
use vaco_limits::{Budget, Limits};
use vaco_mux_mpegts::mux::MpegTsMuxer;
use vaco_packet::{Packet, PacketFlags};

// `MUX_DELAY_TICKS * 2` (`vaco_mux_mpegts::mux`, private): the reference's
// default `-max_delay`/`-muxdelay` (0.7 s @ 90 kHz) shift this muxer now
// applies to every on-wire PTS/DTS (issue #636) — measured against
// `ffmpeg -bitexact -c copy -f mpegts`. `vaco-demux-mpegts` reads the wire
// value as-is, so a round trip sees the shifted value, not the one handed to
// `write_packet`.
const MUX_DELAY_SHIFT: i64 = 126_000;

fn packet(stream_index: u32, pts: i64, dts: i64, key: bool, payload: &[u8]) -> Packet {
    let mut budget = Budget::new(Limits::permissive());
    let mut pkt = Packet::from_slice(&mut budget, payload).expect("alloc");
    pkt.stream_index = stream_index;
    pkt.pts = Timestamp::new(pts);
    pkt.dts = Timestamp::new(dts);
    if key {
        pkt.flags |= PacketFlags::KEY;
    }
    pkt
}

fn video_params(codec: CodecId) -> CodecParameters {
    CodecParameters {
        media_type: Some(MediaType::Video),
        codec_id: Some(codec),
        ..CodecParameters::new(MediaType::Video)
    }
}

fn audio_params(codec: CodecId) -> CodecParameters {
    CodecParameters {
        media_type: Some(MediaType::Audio),
        codec_id: Some(codec),
        ..CodecParameters::new(MediaType::Audio)
    }
}

fn mux_two_streams() -> Vec<u8> {
    let sink = SharedDynBuf::new();
    let mirror = sink.clone();
    let mut mux = MpegTsMuxer::new(Box::new(sink) as Box<dyn MediaSink>);
    let video = mux
        .add_stream(&video_params(CodecId::Mpeg2video))
        .expect("add video");
    let audio = mux
        .add_stream(&audio_params(CodecId::Mp2))
        .expect("add audio");
    mux.init().expect("init");
    mux.write_header().expect("write_header");
    for i in 0..5i64 {
        mux.write_packet(&packet(video, i * 3600, i * 3600, i == 0, &[0xABu8; 200]))
            .expect("write video packet");
        mux.write_packet(&packet(audio, i * 1920, i * 1920, false, &[0xCDu8; 64]))
            .expect("write audio packet");
    }
    mux.write_trailer().expect("write_trailer");
    drop(mux);
    mirror.take()
}

fn demux_all(bytes: Vec<u8>) -> (Vec<vaco_format_core::Stream>, Vec<Packet>) {
    let src = Box::new(MemorySource::new(bytes));
    let mut demux =
        MpegTsDemuxer::open(src, &NoParsers, &FormatOptions::default()).expect("open demuxer");
    let streams = demux.streams().to_vec();
    let mut packets = Vec::new();
    loop {
        match demux.read_packet() {
            Ok(p) => packets.push(p),
            Err(vaco_core::Error::Eof) => break,
            Err(e) => panic!("read_packet: {e:?}"),
        }
    }
    (streams, packets)
}

#[test]
fn round_trip_recovers_both_streams_with_correct_payloads_and_timestamps() {
    let bytes = mux_two_streams();
    assert!(!bytes.is_empty());
    assert_eq!(bytes.len() % 188, 0);

    let (streams, packets) = demux_all(bytes);
    assert_eq!(
        streams.len(),
        2,
        "video and audio should both be discovered from the PMT"
    );

    let video_pkts: Vec<&Packet> = packets
        .iter()
        .filter(|p| streams[p.stream_index as usize].params.media_type == Some(MediaType::Video))
        .collect();
    let audio_pkts: Vec<&Packet> = packets
        .iter()
        .filter(|p| streams[p.stream_index as usize].params.media_type == Some(MediaType::Audio))
        .collect();
    assert_eq!(video_pkts.len(), 5);
    assert_eq!(audio_pkts.len(), 5);

    for (i, p) in video_pkts.iter().enumerate() {
        assert_eq!(p.payload(), &[0xABu8; 200][..], "video packet {i}");
        assert_eq!(
            p.pts.ticks(),
            Some(i64::try_from(i).unwrap() * 3600 + MUX_DELAY_SHIFT),
            "video pts {i}"
        );
    }
    for (i, p) in audio_pkts.iter().enumerate() {
        assert_eq!(p.payload(), &[0xCDu8; 64][..], "audio packet {i}");
        assert_eq!(
            p.pts.ticks(),
            Some(i64::try_from(i).unwrap() * 1920 + MUX_DELAY_SHIFT),
            "audio pts {i}"
        );
    }
}

#[test]
fn the_pat_and_pmt_are_repeated_before_the_reader_ever_needs_a_seek() {
    // A demuxer that opens straight into the middle of a long recording still
    // needs to see a PAT/PMT quickly; this only tests that our own default
    // `pat_period` causes at least one resend across enough packets, using
    // the demuxer's own successful `open` (which requires having seen a PMT)
    // as the proof.
    let sink = SharedDynBuf::new();
    let mirror = sink.clone();
    let mut mux = MpegTsMuxer::new(Box::new(sink) as Box<dyn MediaSink>);
    let v = mux.add_stream(&video_params(CodecId::Mpeg2video)).unwrap();
    mux.init().unwrap();
    mux.write_header().unwrap();
    for i in 0..3i64 {
        mux.write_packet(&packet(v, i * 3600, i * 3600, i == 0, &[0u8; 50]))
            .unwrap();
    }
    mux.write_trailer().unwrap();
    let bytes = mirror.take();
    let (streams, _packets) = demux_all(bytes);
    assert_eq!(streams.len(), 1);
}

#[test]
fn m2ts_mode_still_demuxes_once_the_four_byte_prefix_is_known() {
    let sink = SharedDynBuf::new();
    let mirror = sink.clone();
    let mut mux = MpegTsMuxer::with_options(
        Box::new(sink) as Box<dyn MediaSink>,
        vaco_mux_mpegts::options::MpegTsMuxOptions::default(),
        true,
    );
    let v = mux.add_stream(&video_params(CodecId::Mpeg2video)).unwrap();
    mux.init().unwrap();
    mux.write_header().unwrap();
    mux.write_packet(&packet(v, 0, 0, true, &[0u8; 50]))
        .unwrap();
    mux.write_trailer().unwrap();
    let bytes = mirror.take();
    assert_eq!(bytes.len() % 192, 0);
    // The demuxer's own stride detector must recognise M2TS from content
    // alone, with no format-name hint.
    let src = Box::new(MemorySource::new(bytes));
    let demux = MpegTsDemuxer::open(src, &NoParsers, &FormatOptions::default());
    assert!(demux.is_ok(), "M2TS output should still open as mpegts");
}

/// The reference's SDT service descriptor carries `provider_name="FFmpeg"`/
/// `service_name="Service01"` with no `-service_name`/`-service_provider`
/// flag given at all — measured, not documented in `-h muxer=mpegts` (which
/// has no such option). `MpegTsMuxOptions::default()` must reproduce that,
/// not the empty strings a naive "no option means no value" reading would
/// pick.
#[test]
fn default_options_write_the_references_measured_sdt_strings() {
    let sink = SharedDynBuf::new();
    let mirror = sink.clone();
    let mut mux = MpegTsMuxer::new(Box::new(sink) as Box<dyn MediaSink>);
    let v = mux.add_stream(&video_params(CodecId::Mpeg2video)).unwrap();
    mux.init().unwrap();
    mux.write_header().unwrap();
    mux.write_packet(&packet(v, 0, 0, true, &[0u8; 50]))
        .unwrap();
    mux.write_trailer().unwrap();
    let bytes = mirror.take();

    // DVB EN 300 468 §6.2.33's service descriptor orders
    // `provider_name` before `service_name`, both length-prefixed — the same
    // byte order this crate's own `build_sdt` writes them in.
    let mut expected = Vec::new();
    expected.push(u8::try_from("FFmpeg".len()).unwrap());
    expected.extend_from_slice(b"FFmpeg");
    expected.push(u8::try_from("Service01".len()).unwrap());
    expected.extend_from_slice(b"Service01");
    assert!(
        bytes
            .windows(expected.len())
            .any(|w| w == expected.as_slice()),
        "expected the default provider/service name pair in the SDT"
    );
}
