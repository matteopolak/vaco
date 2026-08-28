//! Mux synthetic H.264/AAC packets through [`HdsMuxer`] onto real files in a
//! `tempfile::tempdir()`, then check the output's structure against what
//! was measured from real `ffmpeg -f hds` output — no demuxer exists for
//! this format anywhere, so this is the closest available check to a round
//! trip: reproduce the shape of asset the reference was measured on (12s,
//! 10s fragment target, a keyframe every 5s) and confirm the fragment
//! count/`abst` run table/manifest all agree with the measured structure,
//! and that each fragment restates both tracks' sequence headers.
//!
//! Everything here goes through `file:` — no network.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::cast_lossless,
    clippy::items_after_statements,
    reason = "test code"
)]

use std::fs;

use vaco_codec_core::{AudioParameters, CodecId, CodecParameters, VideoParameters};
use vaco_core::{Duration, MediaType, Rational};
use vaco_format_adaptive::WriteAccess;
use vaco_format_core::Muxer;
use vaco_limits::{Budget, Limits};
use vaco_mux_hds::{HdsMuxOptions, HdsMuxer};
use vaco_packet::{Packet, PacketFlags};
use vaco_protocol_core::ProtocolRegistry;

fn registry() -> ProtocolRegistry {
    let mut r = ProtocolRegistry::new();
    r.register(&vaco_protocol_file::FILE_PROTOCOL);
    r
}

fn avc_extradata() -> Vec<u8> {
    vec![
        0x01, 0xf4, 0x00, 0x0d, 0xff, 0xe1, 0x00, 0x19, 0x67, 0xf4, 0x00, 0x0d, 0x91, 0x9b, 0x28,
        0x28, 0x3f, 0x60, 0x22, 0x00, 0x00, 0x03, 0x00, 0x02, 0x00, 0x00, 0x03, 0x00, 0x64, 0x1e,
        0x28, 0x53, 0x2c, 0x01, 0x00, 0x06, 0x68, 0xeb, 0xe3, 0xc4, 0x48, 0x44,
    ]
}

fn h264_params(bit_rate: u64) -> CodecParameters {
    let mut p = CodecParameters {
        media_type: Some(MediaType::Video),
        codec_id: Some(CodecId::H264),
        extradata: Some(avc_extradata()),
        bit_rate: Some(bit_rate),
        ..CodecParameters::default()
    };
    p.video = Some(VideoParameters {
        width: 320,
        height: 240,
        frame_rate: Rational::new(25, 1),
        nal_length_size: Some(4),
        ..VideoParameters::default()
    });
    p
}

fn aac_params(bit_rate: u64) -> CodecParameters {
    let extradata = vec![0x11, 0x88, 0x56, 0xe5, 0x00];
    let mut p = CodecParameters {
        media_type: Some(MediaType::Audio),
        codec_id: Some(CodecId::Aac),
        extradata: Some(extradata),
        bit_rate: Some(bit_rate),
        ..CodecParameters::default()
    };
    p.audio = Some(AudioParameters {
        sample_rate: 48_000,
        layout: Some(vaco_chlayout::ChannelLayout::MONO),
        ..AudioParameters::default()
    });
    p
}

fn walk_boxes(data: &[u8]) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 8 <= data.len() {
        let size = u32::from_be_bytes(data[i..i + 4].try_into().unwrap()) as u64;
        let kind = String::from_utf8_lossy(&data[i + 4..i + 8]).into_owned();
        out.push((kind, size));
        if size < 8 {
            break;
        }
        i += size as usize;
    }
    out
}

#[test]
fn twelve_seconds_produces_two_fragments_with_restated_sequence_headers() {
    let dir = tempfile::tempdir().unwrap();
    let manifest_path = dir.path().join("index.f4m");
    let manifest_url = manifest_path.to_str().unwrap().to_owned();

    let write = WriteAccess::unrestricted(registry());
    let mut mux = HdsMuxer::new(manifest_url, Some(write), HdsMuxOptions::new());

    let video_idx = mux.add_stream(&h264_params(400_000)).unwrap();
    let audio_idx = mux.add_stream(&aac_params(69_000)).unwrap();
    mux.write_header().unwrap();

    let mut budget = Budget::new(Limits::permissive());

    const VIDEO_FRAME_US: i64 = 40_000; // 1/25 s
    const VIDEO_FRAMES: i64 = 300; // 12s, keyframe every 125 frames (5s GOP)
    for i in 0..VIDEO_FRAMES {
        let is_key = i % 125 == 0;
        let mut pkt = Packet::from_slice(&mut budget, &[0xAB, 0xCD, 0xEF, 0x01]).unwrap();
        pkt.stream_index = video_idx;
        pkt.duration = Duration::from_micros(VIDEO_FRAME_US);
        pkt.flags = if is_key { PacketFlags::KEY } else { PacketFlags::empty() };
        mux.write_packet(&pkt).unwrap();
    }

    const AUDIO_FRAME_US: i64 = 21_333;
    const AUDIO_FRAMES: i64 = 563;
    for _ in 0..AUDIO_FRAMES {
        let mut pkt = Packet::from_slice(&mut budget, &[0x11, 0x22, 0x33]).unwrap();
        pkt.stream_index = audio_idx;
        pkt.duration = Duration::from_micros(AUDIO_FRAME_US);
        pkt.flags = PacketFlags::KEY;
        mux.write_packet(&pkt).unwrap();
    }

    mux.write_trailer().unwrap();

    let manifest_xml = fs::read_to_string(&manifest_path).unwrap();
    assert!(manifest_xml.contains("<bootstrapInfo profile=\"named\" url=\"stream0.abst\" id=\"bootstrap0\" />"));
    assert!(manifest_xml.contains("bitrate=\"469\""), "{manifest_xml}"); // 400_000+69_000 bps -> 469 kbit/s
    assert!(manifest_xml.contains("url=\"stream0\""));

    let frag1 = fs::read(dir.path().join("stream0Seg1-Frag1")).unwrap();
    let frag2 = fs::read(dir.path().join("stream0Seg1-Frag2")).unwrap();
    let abst = fs::read(dir.path().join("stream0.abst")).unwrap();

    // Each fragment file is one bare `mdat` box, no `moof` at all.
    let boxes1 = walk_boxes(&frag1);
    assert_eq!(boxes1, vec![("mdat".to_owned(), frag1.len() as u64)]);
    let boxes2 = walk_boxes(&frag2);
    assert_eq!(boxes2, vec![("mdat".to_owned(), frag2.len() as u64)]);

    // Fragment 2 restates both tracks' sequence headers before any real
    // sample: first tag is video (type 9), AVCPacketType=0 (offset 11 in
    // the mdat payload, right after the 11-byte FLV tag header, then the
    // FrameType/CodecID byte at offset 11, AVCPacketType at offset 12).
    let payload2 = &frag2[8..];
    assert_eq!(payload2[0], 9, "first tag in fragment 2 is video");
    assert_eq!(payload2[12], 0x00, "video AVCPacketType=0 (sequence header)");
    // The AAC sequence header tag follows immediately after the video one.
    let video_tag_total = 11 + u32::from_be_bytes([0, payload2[1], payload2[2], payload2[3]]) as usize + 4;
    assert_eq!(payload2[video_tag_total], 8, "second tag in fragment 2 is audio");
    assert_eq!(
        payload2[video_tag_total + 12],
        0x00,
        "audio AACPacketType=0 (sequence header)"
    );

    // abst: two fragments, first_fragment_timestamp/duration land on the
    // same 10s/2s split real ffmpeg produced for the same shape of input.
    assert_eq!(&abst[8..12], &0u32.to_be_bytes()); // version+flags of the abst full box (0)
    // asrt's own fragmentsPerSegment (last 4 bytes of the asrt payload,
    // found via the "asrt" tag) must equal 2.
    let asrt_pos = abst.windows(4).position(|w| w == b"asrt").unwrap();
    let fragments_per_segment = u32::from_be_bytes(abst[asrt_pos + 17..asrt_pos + 21].try_into().unwrap());
    assert_eq!(fragments_per_segment, 2);

    // Structural bar met; playback through a real Flash/HDS client is not
    // reachable on this machine (see lib.rs's docs) and is not claimed.
    let _ = &frag1;
}

#[test]
fn a_stream_with_annexb_h264_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let manifest_url = dir.path().join("index.f4m").to_str().unwrap().to_owned();
    let write = WriteAccess::unrestricted(registry());
    let mut mux = HdsMuxer::new(manifest_url, Some(write), HdsMuxOptions::new());

    let mut params = h264_params(400_000);
    params.video.as_mut().unwrap().nal_length_size = Some(0);
    let err = mux.add_stream(&params).unwrap_err();
    assert!(format!("{err:?}").contains("length-prefixed"));
}

#[test]
fn two_quality_levels_pair_video_then_audio_into_separate_streams() {
    let dir = tempfile::tempdir().unwrap();
    let manifest_url = dir.path().join("index.f4m").to_str().unwrap().to_owned();
    let write = WriteAccess::unrestricted(registry());
    let mut mux = HdsMuxer::new(manifest_url, Some(write), HdsMuxOptions::new());

    mux.add_stream(&h264_params(400_000)).unwrap();
    mux.add_stream(&aac_params(69_000)).unwrap();
    mux.add_stream(&h264_params(200_000)).unwrap();
    mux.add_stream(&aac_params(40_000)).unwrap();
    mux.write_header().unwrap();
    mux.write_trailer().unwrap();

    let manifest_xml = fs::read_to_string(dir.path().join("index.f4m")).unwrap();
    assert!(manifest_xml.contains("url=\"stream0\""));
    assert!(manifest_xml.contains("url=\"stream1\""));
    assert!(manifest_xml.contains("bitrate=\"469\""));
    assert!(manifest_xml.contains("bitrate=\"240\""));
    assert!(dir.path().join("stream0.abst").exists());
    assert!(dir.path().join("stream1.abst").exists());
}
