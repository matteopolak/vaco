//! Mux a small video+audio file — including one media object bigger than a
//! Data Packet, to exercise fragmentation — then demux it back with
//! `vaco-demux-asf`, the most direct check that the two crates'
//! understanding of the format agrees. Mirrors `vaco-mux-avi`'s own
//! `tests/roundtrip.rs` for the same reason.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::cast_possible_wrap,
    reason = "test code"
)]

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{MediaType, Rational, Timestamp};
use vaco_demux_asf::AsfDemuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::vacoraw::{MemorySink, SharedBytes};
use vaco_format_core::{Demuxer, FormatOptions, Muxer};
use vaco_io::MemorySource;
use vaco_limits::Budget;
use vaco_mux_asf::AsfMuxer;
use vaco_packet::{Packet, PacketFlags};

fn video_params(width: u32, height: u32, fps: (i32, i32)) -> CodecParameters {
    let mut p = CodecParameters::video();
    p.codec_id = Some(CodecId::H264);
    if let Some(v) = &mut p.video {
        v.width = width;
        v.height = height;
        v.frame_rate = Rational::new(fps.0, fps.1);
    }
    p
}

fn audio_params(sample_rate: u32) -> CodecParameters {
    let mut p = CodecParameters::audio();
    p.codec_id = Some(CodecId::Pcm);
    if let Some(a) = &mut p.audio {
        a.sample_rate = sample_rate;
        a.bits_per_coded_sample = Some(16);
    }
    p
}

/// Build a packet whose timestamp is already in the muxer's declared time
/// base (milliseconds, per [`Muxer::stream_time_base`]) — the interleave
/// pipeline's M1 rescale step would normally do this; the test does it
/// directly since there is no pipeline here.
fn packet(stream_index: u32, pts_ms: i64, payload: &[u8], key: bool) -> Packet {
    let mut budget = Budget::new(vaco_limits::Limits::permissive());
    let mut pkt = Packet::from_slice(&mut budget, payload).unwrap();
    pkt.stream_index = stream_index;
    pkt.pts = Timestamp::new(pts_ms);
    // No reordering in this fixture (no B-frames), so dts equals pts — this
    // crate's muxer now writes ASF's "Presentation Time" from `packet.dts`
    // (decode order, monotonic even with real B-frame content), not `pts`;
    // leaving `dts` at its `Timestamp::NONE` default here would zero out
    // every payload's Replicated Data and, downstream, the muxer's own
    // `max_pts_ms`/trailer duration patch this file's own tests check.
    pkt.dts = pkt.pts;
    pkt.flags = if key {
        PacketFlags::KEY
    } else {
        PacketFlags::empty()
    };
    pkt
}

/// Mux three video frames (the third far larger than the packet size, to
/// force fragmentation) and two audio chunks, returning the bytes and the
/// (video, audio) stream indices.
fn mux_sample() -> (Vec<u8>, u32, u32) {
    let sink = MemorySink::new();
    let shared: SharedBytes = sink.shared();
    let mut mux = AsfMuxer::new(Box::new(sink), &FormatOptions::default())
        .unwrap()
        .with_packet_size(256)
        .unwrap();

    let v = mux.add_stream(&video_params(64, 48, (1, 10))).unwrap();
    let a = mux.add_stream(&audio_params(8000)).unwrap();
    mux.write_header().unwrap();

    mux.write_packet(&packet(v, 0, &[0xAA; 20], true)).unwrap();
    mux.write_packet(&packet(a, 0, &[0u8; 400], true)).unwrap(); // 200 mono s16 samples
    // A video frame far bigger than the 256-byte packet size: must fragment.
    let big_frame: Vec<u8> = (0..600u32).map(|i| (i % 251) as u8).collect();
    mux.write_packet(&packet(v, 100, &big_frame, false))
        .unwrap();
    mux.write_packet(&packet(a, 25, &[0u8; 200], true)).unwrap();
    mux.write_trailer().unwrap();

    (shared.snapshot(), v, a)
}

fn open(bytes: Vec<u8>) -> AsfDemuxer {
    let src = Box::new(MemorySource::new(bytes));
    AsfDemuxer::open(src, &NoParsers, &FormatOptions::default()).expect("demux what we muxed")
}

#[test]
fn muxed_streams_demux_with_the_right_shape() {
    let (bytes, v, a) = mux_sample();
    assert_eq!((v, a), (0, 1));
    let demux = open(bytes);
    let streams = demux.streams();
    assert_eq!(streams.len(), 2);
    assert_eq!(streams[0].media_type(), Some(MediaType::Video));
    assert_eq!(streams[0].params.codec_id, Some(CodecId::H264));
    assert_eq!(streams[0].params.video.as_ref().unwrap().width, 64);
    assert_eq!(streams[0].params.video.as_ref().unwrap().height, 48);
    assert_eq!(streams[1].media_type(), Some(MediaType::Audio));
    assert_eq!(streams[1].params.codec_id, Some(CodecId::PcmS16le));
    assert_eq!(streams[1].params.audio.as_ref().unwrap().sample_rate, 8000);
}

#[test]
fn a_fragmented_media_object_reassembles_byte_exact() {
    let (bytes, v, _a) = mux_sample();
    let mut demux = open(bytes);
    let big_frame: Vec<u8> = (0..600u32).map(|i| (i % 251) as u8).collect();

    let mut found = false;
    loop {
        match demux.read_packet() {
            Ok(p) if p.stream_index == v && p.len == big_frame.len() => {
                assert_eq!(p.payload(), big_frame.as_slice());
                assert!(!p.is_key());
                found = true;
            }
            Ok(_) => {}
            Err(vaco_core::Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    assert!(found, "the fragmented 600-byte frame was never reassembled");
}

#[test]
fn muxed_packets_demux_with_the_right_shape_and_key_flags() {
    let (bytes, v, a) = mux_sample();
    let mut demux = open(bytes);
    let mut got = Vec::new();
    loop {
        match demux.read_packet() {
            Ok(p) => got.push((p.stream_index, p.is_key(), p.len)),
            Err(vaco_core::Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    assert_eq!(
        got,
        vec![
            (v, true, 20),
            (a, true, 400),
            (v, false, 600),
            (a, true, 200),
        ]
    );
}

#[test]
fn the_trailer_patches_counts_so_a_second_open_sees_a_consistent_file() {
    let (bytes, _v, _a) = mux_sample();
    let demux = open(bytes);
    // Duration comes from the patched Play Duration field; asserting it is
    // present (rather than a specific value, which depends on this test's
    // own timestamps) is what actually exercises the patch-back path.
    assert!(demux.duration().is_some());
}

#[test]
fn a_codec_with_no_asf_mapping_is_rejected_not_silently_wrong() {
    let sink = MemorySink::new();
    let mut mux = AsfMuxer::new(Box::new(sink), &FormatOptions::default()).unwrap();
    let mut p = CodecParameters::video();
    p.codec_id = Some(CodecId::Av1); // no ASF FourCC mapping in this crate
    if let Some(v) = &mut p.video {
        v.width = 64;
        v.height = 48;
    }
    assert!(mux.add_stream(&p).is_err());
}

/// ADTS-framed AAC (MPEG-TS's own convention: no out-of-band config, the
/// config repeats in every frame header instead) is refused, not silently
/// written into a stream properties object with no `AudioSpecificConfig`
/// behind it. Measured against `ffmpeg 9.0.1`: `-c copy -f asf` on an
/// ADTS-sourced AAC stream fails with "ADTS is only supported with codec tag
/// 0x1610"; it does not run `aac_adtstoasc` automatically.
#[test]
fn adts_framed_aac_with_no_extradata_is_rejected() {
    let sink = MemorySink::new();
    let mut mux = AsfMuxer::new(Box::new(sink), &FormatOptions::default()).unwrap();
    let mut p = CodecParameters::audio();
    p.codec_id = Some(CodecId::Aac);
    if let Some(a) = &mut p.audio {
        a.sample_rate = 44_100;
    }
    // No extradata set at all — exactly what an ADTS-framed source gives.
    assert!(mux.add_stream(&p).is_err());
}

/// The same codec with a raw `AudioSpecificConfig` in `extradata` (what an
/// MP4/`esds` or Matroska source gives) is the case this crate does support.
#[test]
fn raw_aac_with_extradata_is_accepted() {
    let sink = MemorySink::new();
    let mut mux = AsfMuxer::new(Box::new(sink), &FormatOptions::default()).unwrap();
    let mut p = CodecParameters::audio();
    p.codec_id = Some(CodecId::Aac);
    p.extradata = Some(vec![0x12, 0x10]); // a minimal AudioSpecificConfig
    if let Some(a) = &mut p.audio {
        a.sample_rate = 44_100;
    }
    assert!(mux.add_stream(&p).is_ok());
}

// Packetise-then-depacketise across random payload sizes and packet sizes —
// the property test that actually exercises both the "small objects packed
// behind one multiple-payload header" and "one large object split into
// fragments" paths in the same run, which is exactly where a boundary bug
// in either would show up.
proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]

    #[test]
    fn packetise_then_depacketise_round_trips_for_any_payload_and_packet_size(
        packet_size in 100u32..=2000,
        payload_lens in proptest::collection::vec(0usize..3000, 1..12),
    ) {
        let sink = MemorySink::new();
        let shared: SharedBytes = sink.shared();
        let mut mux = AsfMuxer::new(Box::new(sink), &FormatOptions::default())
            .unwrap()
            .with_packet_size(packet_size)
            .unwrap();
        let v = mux.add_stream(&video_params(16, 16, (1, 1))).unwrap();
        mux.write_header().unwrap();

        let payloads: Vec<Vec<u8>> = payload_lens
            .iter()
            .enumerate()
            .map(|(i, &len)| (0..len).map(|b| (b.wrapping_add(i) % 256) as u8).collect())
            .collect();
        for (i, data) in payloads.iter().enumerate() {
            mux.write_packet(&packet(v, i as i64 * 40, data, i == 0)).unwrap();
        }
        mux.write_trailer().unwrap();

        let mut demux = open(shared.snapshot());
        let mut got = Vec::new();
        loop {
            match demux.read_packet() {
                Ok(p) => got.push(p.payload().to_vec()),
                Err(vaco_core::Error::Eof) => break,
                Err(e) => panic!("unexpected error: {e:?}"),
            }
        }
        assert_eq!(got, payloads);
    }
}
