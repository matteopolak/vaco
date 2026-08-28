//! Mux a small mp3 stream, then demux it back with
//! `vaco-demux-mpegaudio` — the most direct check that the two crates'
//! understanding of the Xing/LAME header they share agrees.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{AudioParameters, CodecId, CodecParameters};
use vaco_core::MediaType;
use vaco_demux_mpegaudio::MpegAudioDemuxer;
use vaco_format_core::vacoraw::{MemorySink, SharedBytes};
use vaco_format_core::{Demuxer, FormatOptions, Muxer};
use vaco_format_mpegaudio::{ChannelMode, Emphasis, Layer, MpegAudioHeader, Version};
use vaco_io::MemorySource;
use vaco_limits::Budget;
use vaco_mux_mpegaudio::MpegAudioMuxer;
use vaco_packet::{Packet, PacketSideData};
use vaco_sampfmt::SampleFmt;

/// A syntactically valid MPEG-1 Layer III mono 44100 Hz frame at 128 kbps
/// (417 bytes), filled with a repeating byte so frames are distinguishable.
fn cbr_frame(fill: u8) -> Vec<u8> {
    let header = MpegAudioHeader {
        version: Version::Mpeg1,
        layer: Layer::III,
        has_crc: false,
        bitrate_index: 9,
        sample_rate_index: 0,
        padding: false,
        private_bit: false,
        channel_mode: ChannelMode::Mono,
        mode_extension: 0,
        copyright: false,
        original: false,
        emphasis: Emphasis::None,
    };
    let len = header.frame_len().expect("cbr frame has a length") as usize;
    let mut frame = vec![fill; len];
    frame[..4].copy_from_slice(&header.to_bytes());
    frame
}

fn audio_params() -> CodecParameters {
    CodecParameters {
        media_type: Some(MediaType::Audio),
        codec_id: Some(CodecId::Mp3),
        audio: Some(AudioParameters {
            sample_rate: 44100,
            format: Some(SampleFmt::F32P),
            layout: Some(ChannelLayout::MONO),
            bits_per_coded_sample: Some(0),
            bits_per_raw_sample: None,
            initial_padding: 0,
        }),
        bit_rate: Some(128_000),
        ..CodecParameters::default()
    }
}

fn packet(payload: &[u8], skip_start: u32, skip_end: u32) -> Packet {
    let mut budget = Budget::new(vaco_limits::Limits::permissive());
    let mut pkt = Packet::from_slice(&mut budget, payload).unwrap();
    if skip_start != 0 || skip_end != 0 {
        pkt.side_data.push(PacketSideData::SkipSamples {
            start: skip_start,
            end: skip_end,
            skip_reason: 0,
            discard_reason: 0,
        });
    }
    pkt
}

fn mux_sample() -> Vec<u8> {
    let sink = MemorySink::new();
    let shared: SharedBytes = sink.shared();
    let mut mux = MpegAudioMuxer::new(Box::new(sink)).unwrap();
    mux.add_stream(&audio_params()).unwrap();
    mux.write_header().unwrap();

    mux.write_packet(&packet(&cbr_frame(0xAA), 1105, 0))
        .unwrap();
    mux.write_packet(&packet(&cbr_frame(0xBB), 0, 0)).unwrap();
    mux.write_packet(&packet(&cbr_frame(0xCC), 0, 551)).unwrap();
    mux.write_trailer().unwrap();

    shared.snapshot()
}

#[test]
fn the_written_xing_frame_has_a_valid_header_at_the_streams_own_rate() {
    let bytes = mux_sample();
    // 20-byte empty ID3v2 tag precedes the synthesized Xing frame.
    let header = MpegAudioHeader::parse_bytes(&bytes[20..]).expect("valid header");
    assert_eq!(header.version, Version::Mpeg1);
    assert_eq!(header.layer, Layer::III);
    assert_eq!(header.channel_mode, ChannelMode::Mono);
    assert_eq!(header.sample_rate_hz(), 44100);
}

#[test]
fn demuxing_the_muxed_stream_recovers_three_packets_and_the_gapless_trim() {
    let bytes = mux_sample();
    let src = MemorySource::forward_only(bytes);
    let mut demux = MpegAudioDemuxer::open(Box::new(src), &FormatOptions::default()).unwrap();

    let first = demux.read_packet().unwrap();
    assert_eq!(first.payload()[4], 0xAA);
    let skip = first
        .side_data
        .iter()
        .find_map(|sd| match sd {
            PacketSideData::SkipSamples { start, .. } => Some(*start),
            _ => None,
        })
        .unwrap();
    assert_eq!(skip, 1105);

    let second = demux.read_packet().unwrap();
    assert_eq!(second.payload()[4], 0xBB);

    let third = demux.read_packet().unwrap();
    assert_eq!(third.payload()[4], 0xCC);
    let discard = third
        .side_data
        .iter()
        .find_map(|sd| match sd {
            PacketSideData::SkipSamples { end, .. } => Some(*end),
            _ => None,
        })
        .unwrap();
    assert_eq!(discard, 551);

    assert!(matches!(demux.read_packet(), Err(vaco_core::Error::Eof)));
}
