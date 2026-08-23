//! Mux, then read back with the sibling demuxer.
//!
//! This is the strongest test either Ogg crate has: not "did we write bytes
//! that look right" but "does `vaco-demux-ogg`, developed independently
//! against RFC 3533 and measured `ffprobe` behaviour, agree with what this
//! crate wrote". Uses `vaco_format_core::vacoraw::MemorySink` — a seekable
//! in-memory `MediaSink` that already exists for exactly this kind of test
//! (see its own docs for why it lives there rather than in `vaco-io`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code"
)]

use vaco_codec_core::{AudioParameters, CodecId, CodecParameters};
use vaco_core::{Duration, Timestamp};
use vaco_demux_ogg::OggDemuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::vacoraw::MemorySink;
use vaco_format_core::{Demuxer, FormatOptions, Muxer};
use vaco_io::MemorySource;
use vaco_limits::Budget;
use vaco_mux_ogg::OggMuxer;
use vaco_packet::{Packet, PacketFlags};

/// The exact 19-byte `OpusHead` this crate's own `crc.rs` measured against
/// `ffmpeg -c:a libopus`: mono, `pre_skip` 312, input rate 48000.
const OPUS_HEAD: &[u8] = &[
    b'O', b'p', b'u', b's', b'H', b'e', b'a', b'd', 0x01, 0x01, 0x38, 0x01, 0x80, 0xBB, 0x00, 0x00,
    0x00, 0x00, 0x00,
];

fn opus_packet(budget: &mut Budget, stream: u32, pts: i64, dur: i64, payload: &[u8]) -> Packet {
    let mut pkt = Packet::from_slice(budget, payload).unwrap();
    pkt.stream_index = stream;
    pkt.pts = Timestamp::new(pts);
    pkt.dts = pkt.pts;
    // Duration ticks are 1/48000 here, matching the stream time base this
    // muxer declares for Opus.
    pkt.duration = Duration::from_micros(dur * 1_000_000 / 48_000);
    pkt.flags = PacketFlags::KEY;
    pkt
}

#[test]
fn opus_round_trips_through_the_sibling_demuxer() {
    let sink = Box::new(MemorySink::new());
    let bytes_handle = sink.shared();
    let mut mux = OggMuxer::new(sink).unwrap();

    let mut params = CodecParameters::new(vaco_core::MediaType::Audio).with_codec(CodecId::Opus);
    params.extradata = Some(OPUS_HEAD.to_vec());
    params.audio = Some(AudioParameters {
        sample_rate: 48_000,
        initial_padding: 312,
        ..AudioParameters::default()
    });
    let idx = mux.add_stream(&params).unwrap();
    mux.write_header().unwrap();

    let mut budget = Budget::new(vaco_limits::Limits::permissive());
    // A 20 ms (960-sample) TOC-only Opus packet: config 31 (fullband CELT,
    // 20 ms), mono, code 0 — one frame, no further framing bytes needed.
    let payload = [0xFCu8, 0x01, 0x02, 0x03];
    let mut pts = -312i64;
    for _ in 0..30 {
        let pkt = opus_packet(&mut budget, idx, pts, 960, &payload);
        mux.write_packet(&pkt).unwrap();
        pts += 960;
    }
    mux.write_trailer().unwrap();

    let written = bytes_handle.snapshot();
    assert!(!written.is_empty());
    assert_eq!(&written[0..4], b"OggS");

    let mut demux = OggDemuxer::open(
        Box::new(MemorySource::new(written)),
        &NoParsers,
        &FormatOptions::default(),
    )
    .expect("the sibling demuxer must accept what this crate wrote");

    assert_eq!(demux.streams().len(), 1);
    let stream = &demux.streams()[0];
    assert_eq!(stream.params.codec_id, Some(CodecId::Opus));
    assert_eq!(
        stream.params.audio.as_ref().map(|a| a.sample_rate),
        Some(48_000)
    );

    let mut count = 0u32;
    let mut first_pts = None;
    let mut last_pts = None;
    while let Ok(p) = demux.read_packet() {
        if first_pts.is_none() {
            first_pts = Some(p.pts.ticks());
        }
        last_pts = Some(p.pts.ticks());
        assert_eq!(
            p.payload(),
            payload,
            "packet {count} payload must survive intact"
        );
        count += 1;
    }
    assert_eq!(count, 30, "every packet written must be read back");
    assert_eq!(first_pts, Some(Some(-312)));
    assert_eq!(last_pts, Some(Some(-312 + 29 * 960)));
}

#[test]
fn flac_round_trips_through_the_sibling_demuxer() {
    let sink = Box::new(MemorySink::new());
    let bytes_handle = sink.shared();
    let mut mux = OggMuxer::new(sink).unwrap();

    // A minimal STREAMINFO: 44100 Hz, mono, 16 bits — the packed region is
    // the only part this demuxer's parser reads; the fixed block/frame-size
    // fields ahead of it can stay zero for this test.
    let mut streaminfo = vec![0u8; 34];
    let packed: u64 = (44_100u64 << 44) | (15u64 << 36);
    streaminfo[10..18].copy_from_slice(&packed.to_be_bytes());

    let mut params = CodecParameters::new(vaco_core::MediaType::Audio).with_codec(CodecId::Flac);
    params.extradata = Some(streaminfo);
    params.audio = Some(AudioParameters {
        sample_rate: 44_100,
        ..AudioParameters::default()
    });
    let idx = mux.add_stream(&params).unwrap();
    mux.write_header().unwrap();

    let mut budget = Budget::new(vaco_limits::Limits::permissive());
    let payload = [0xFFu8, 0xF8, 0x69, 0x18, 0x00, 0x00];
    for i in 0..5u32 {
        let mut pkt = Packet::from_slice(&mut budget, &payload).unwrap();
        pkt.stream_index = idx;
        pkt.pts = Timestamp::new(i64::from(i) * 4608);
        pkt.dts = pkt.pts;
        pkt.duration = Duration::from_micros(4608 * 1_000_000 / 44_100);
        pkt.flags = PacketFlags::KEY;
        mux.write_packet(&pkt).unwrap();
    }
    mux.write_trailer().unwrap();

    let written = bytes_handle.snapshot();
    let mut demux = OggDemuxer::open(
        Box::new(MemorySource::new(written)),
        &NoParsers,
        &FormatOptions::default(),
    )
    .expect("the sibling demuxer must accept what this crate wrote");

    assert_eq!(demux.streams().len(), 1);
    let stream = &demux.streams()[0];
    assert_eq!(stream.params.codec_id, Some(CodecId::Flac));
    assert_eq!(
        stream.params.audio.as_ref().map(|a| a.sample_rate),
        Some(44_100)
    );

    let mut count = 0u32;
    while let Ok(p) = demux.read_packet() {
        assert_eq!(p.payload(), payload);
        count += 1;
    }
    assert_eq!(count, 5);
}
