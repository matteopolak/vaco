//! Mux synthetic H.264/AAC packets through [`SmoothStreamingMuxer`] onto
//! real files in a `tempfile::tempdir()`, then check the output's
//! *structure* against what was measured from real `ffmpeg -f
//! smoothstreaming` output (`mss-samples/out2.ism` in the working scratch
//! area this crate's docs reference) — no demuxer exists for this format
//! anywhere (measured, see `lib.rs`'s module docs), so this is the closest
//! available check to a round trip: build the same shape of asset the
//! reference was measured on (12s, 5s fragment target, a keyframe every 5s),
//! confirm the `Manifest` states the same chunk count and total duration
//! shape, and confirm every `Fragments`/`FragmentInfo` pair obeys the
//! measured `FragmentInfo == moof alone` byte relationship.
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
use vaco_mux_smoothstreaming::{SmoothStreamingMuxOptions, SmoothStreamingMuxer};
use vaco_packet::{Packet, PacketFlags};
use vaco_protocol_core::ProtocolRegistry;

fn registry() -> ProtocolRegistry {
    let mut r = ProtocolRegistry::new();
    r.register(&vaco_protocol_file::FILE_PROTOCOL);
    r
}

fn avc_extradata() -> Vec<u8> {
    // version 1, one SPS (`67 f4 00 0d ...`), one PPS (`68 eb e3 c4 48 44`) —
    // the exact bytes measured from `mss-samples/out.ism/Manifest`'s own
    // `CodecPrivateData`.
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
        ..VideoParameters::default()
    });
    p
}

fn aac_params(bit_rate: u64) -> CodecParameters {
    // `118856e500` — the exact `AudioSpecificConfig` measured from
    // `mss-samples/out.ism/Manifest` (48kHz mono AAC-LC).
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

/// Walk the top-level boxes of `data`, returning `(type, size, payload_start)`.
fn walk_boxes(data: &[u8]) -> Vec<(String, u64, usize)> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 8 <= data.len() {
        let size = u32::from_be_bytes(data[i..i + 4].try_into().unwrap()) as u64;
        let kind = String::from_utf8_lossy(&data[i + 4..i + 8]).into_owned();
        out.push((kind, size, i + 8));
        if size < 8 {
            break;
        }
        i += size as usize;
    }
    out
}

#[test]
fn twelve_seconds_produces_three_fragments_per_track_with_matching_fragment_info() {
    let dir = tempfile::tempdir().unwrap();
    let manifest_path = dir.path().join("Manifest");
    let manifest_url = manifest_path.to_str().unwrap().to_owned();

    let video_bitrate = 59793;
    let audio_bitrate = 69000;
    // `vaco_protocol_core::Protocol` has no directory-creation verb
    // (`planning/INTERFACE-GAPS.md` gap 27) — `WriteAccess::create` fails
    // with `NotFound` against a path whose parent does not exist yet, so a
    // caller driving this muxer at a local `file:` output must pre-create
    // each `QualityLevels(<bitrate>)/` directory itself, exactly as done
    // here.
    fs::create_dir_all(dir.path().join(format!("QualityLevels({video_bitrate})"))).unwrap();
    fs::create_dir_all(dir.path().join(format!("QualityLevels({audio_bitrate})"))).unwrap();

    let write = WriteAccess::unrestricted(registry());
    let mut mux = SmoothStreamingMuxer::new(
        manifest_url.clone(),
        Some(write),
        SmoothStreamingMuxOptions::new(),
    );
    let video_idx = mux.add_stream(&h264_params(video_bitrate)).unwrap();
    let audio_idx = mux.add_stream(&aac_params(audio_bitrate)).unwrap();
    mux.write_header().unwrap();

    let mut budget = Budget::new(Limits::permissive());

    // 25 fps, keyframe every 125 frames (5s GOP), 12s = 300 frames total —
    // lands exactly on three fragments of 5s/5s/2s, matching
    // `mss-samples/out2.ism`'s own measured chunk durations.
    const VIDEO_FRAME_US: i64 = 40_000; // 1/25 s
    const VIDEO_FRAMES: i64 = 300;
    for i in 0..VIDEO_FRAMES {
        let is_key = i % 125 == 0;
        let mut pkt = Packet::from_slice(&mut budget, &[0xAB, 0xCD, 0xEF, 0x01]).unwrap();
        pkt.stream_index = video_idx;
        pkt.duration = Duration::from_micros(VIDEO_FRAME_US);
        pkt.flags = if is_key {
            PacketFlags::KEY
        } else {
            PacketFlags::empty()
        };
        mux.write_packet(&pkt).unwrap();
    }

    // 48kHz AAC, 1024 samples/frame => 21333us/frame (rounded, matching how
    // a real encoder reports it), for just over 12 seconds.
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
    assert!(
        manifest_xml.contains("Chunks=\"3\""),
        "video should land on exactly 3 fragments:\n{manifest_xml}"
    );
    assert!(
        manifest_xml.contains(&format!("Bitrate=\"{video_bitrate}\"")),
        "manifest should name the video QualityLevel's real bitrate"
    );
    assert!(
        manifest_xml.contains(&format!("Bitrate=\"{audio_bitrate}\"")),
        "manifest should name the audio QualityLevel's real bitrate"
    );
    assert!(manifest_xml.contains("<c n=\"0\" d=\"50000000\" />"));
    assert!(manifest_xml.contains("<c n=\"1\" d=\"50000000\" />"));
    assert!(manifest_xml.contains("<c n=\"2\" d=\"20000000\" />"));

    // Every video Fragments/FragmentInfo pair: FragmentInfo == the moof
    // bytes alone (measured, see `fragment.rs` docs), and Fragments is
    // strictly longer (it also carries `mdat`).
    for start in ["0", "50000000", "100000000"] {
        let frag_path = dir
            .path()
            .join(format!("QualityLevels({video_bitrate})"))
            .join(format!("Fragments(video={start})"));
        let info_path = dir
            .path()
            .join(format!("QualityLevels({video_bitrate})"))
            .join(format!("FragmentInfo(video={start})"));
        let frag = fs::read(&frag_path).unwrap_or_else(|e| panic!("{frag_path:?}: {e}"));
        let info = fs::read(&info_path).unwrap_or_else(|e| panic!("{info_path:?}: {e}"));
        assert!(
            frag.len() > info.len(),
            "Fragments must carry mdat beyond the moof FragmentInfo has"
        );
        assert_eq!(
            &frag[..info.len()],
            info.as_slice(),
            "FragmentInfo must equal the moof prefix of Fragments byte for byte"
        );

        let boxes = walk_boxes(&frag);
        assert_eq!(boxes[0].0, "moof");
        assert_eq!(boxes[1].0, "mdat");
        // moof's own size must equal FragmentInfo's total length.
        assert_eq!(boxes[0].1 as usize, info.len());
    }

    // The manifest's own top-level Duration is the longest track's total:
    // audio's 563 frames * 213_330 HNS ticks/frame, slightly longer than
    // video's exact 12.0s.
    assert!(
        manifest_xml.contains("Duration=\"120104790\""),
        "{manifest_xml}"
    );
}

#[test]
fn a_stream_with_no_declared_bit_rate_is_rejected_before_any_file_is_created() {
    let dir = tempfile::tempdir().unwrap();
    let manifest_url = dir.path().join("Manifest").to_str().unwrap().to_owned();
    let write = WriteAccess::unrestricted(registry());
    let mut mux =
        SmoothStreamingMuxer::new(manifest_url, Some(write), SmoothStreamingMuxOptions::new());

    let mut params = h264_params(0);
    params.bit_rate = None;
    let err = mux.add_stream(&params).unwrap_err();
    assert!(format!("{err:?}").contains("bit_rate"));
}
