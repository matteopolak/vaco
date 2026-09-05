//! End-to-end demuxing of a hand-built FLV file: `onMetaData`, an AVC
//! sequence header plus two coded frames (one with a non-zero
//! `CompositionTime`), and an AAC sequence header plus one raw frame.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]

use vaco_codec_core::CodecId;
use vaco_core::MediaType;
use vaco_demux_flv::FlvDemuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::MemorySource;

fn tag(tag_type: u8, timestamp_ms: u32, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(tag_type);
    let size = u32::try_from(body.len()).unwrap();
    out.extend_from_slice(&size.to_be_bytes()[1..]); // 24-bit DataSize
    out.extend_from_slice(&timestamp_ms.to_be_bytes()[1..]); // 24-bit low
    out.push((timestamp_ms >> 24) as u8); // extended byte
    out.extend_from_slice(&[0, 0, 0]); // StreamID
    out.extend_from_slice(body);
    let prev_size = u32::try_from(out.len()).unwrap();
    let mut framed = prev_size.to_be_bytes().to_vec();
    framed.extend_from_slice(&out);
    framed
}

fn amf_metadata() -> Vec<u8> {
    let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::permissive());
    let _ = &mut budget;
    let mut out = Vec::new();
    vaco_demux_flv::AmfValue::String("onMetaData".to_owned()).encode(&mut out);
    let meta = vaco_demux_flv::AmfValue::EcmaArray(vec![
        ("width".to_owned(), vaco_demux_flv::AmfValue::Number(64.0)),
        ("height".to_owned(), vaco_demux_flv::AmfValue::Number(48.0)),
        ("duration".to_owned(), vaco_demux_flv::AmfValue::Number(1.5)),
    ]);
    meta.encode(&mut out);
    out
}

/// Build a minimal FLV file: header, `onMetaData`, one AVC sequence header,
/// two AVC coded frames, one AAC sequence header, one AAC raw frame.
fn build_flv() -> Vec<u8> {
    let mut file = b"FLV".to_vec();
    file.push(1); // version
    file.push(0x05); // has video + has audio
    file.extend_from_slice(&9u32.to_be_bytes());

    // `tag()` already embeds the leading `PreviousTagSize` field for every
    // tag it builds (including this first one), so no separate
    // `PreviousTagSize0` is written here.
    file.extend_from_slice(&tag(18, 0, &amf_metadata()));

    // AVC sequence header: FrameType=1(key)<<4 | CodecID=7, AVCPacketType=0,
    // CompositionTime=0, then a fake AVCDecoderConfigurationRecord.
    let mut avc_seq = vec![0x17, 0x00, 0x00, 0x00, 0x00];
    avc_seq.extend_from_slice(&[0x01, 0x64, 0x00, 0x0a]); // pretend config bytes
    file.extend_from_slice(&tag(9, 0, &avc_seq));

    // AVC keyframe, CompositionTime = 0.
    let mut avc_key = vec![0x17, 0x01, 0x00, 0x00, 0x00];
    avc_key.extend_from_slice(&[0xAA; 20]);
    file.extend_from_slice(&tag(9, 0, &avc_key));

    // AVC inter frame, CompositionTime = 40ms.
    let mut avc_inter = vec![0x27, 0x01, 0x00, 0x00, 0x28];
    avc_inter.extend_from_slice(&[0xBB; 12]);
    file.extend_from_slice(&tag(9, 100, &avc_inter));

    // AAC sequence header (AudioSpecificConfig).
    let mut aac_seq = vec![0xAF, 0x00];
    aac_seq.extend_from_slice(&[0x12, 0x10]);
    file.extend_from_slice(&tag(8, 0, &aac_seq));

    // AAC raw frame.
    let mut aac_frame = vec![0xAF, 0x01];
    aac_frame.extend_from_slice(&[0xCC; 16]);
    file.extend_from_slice(&tag(8, 23, &aac_frame));

    file
}

fn open(bytes: Vec<u8>) -> FlvDemuxer {
    let src = Box::new(MemorySource::new(bytes));
    FlvDemuxer::open(src, &NoParsers, &FormatOptions::default()).expect("open")
}

#[test]
fn streams_are_discovered_progressively_with_metadata_applied() {
    let mut demux = open(build_flv());
    // Draining once is enough to have seen both the video and audio
    // sequence headers, which is when each stream is created.
    loop {
        match demux.read_packet() {
            Ok(_) => {}
            Err(vaco_core::Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    let streams = demux.streams();
    assert_eq!(streams.len(), 2);
    assert_eq!(streams[0].media_type(), Some(MediaType::Video));
    assert_eq!(streams[0].params.codec_id, Some(CodecId::H264));
    assert_eq!(streams[0].params.video.as_ref().unwrap().width, 64);
    assert_eq!(streams[0].params.video.as_ref().unwrap().height, 48);
    assert_eq!(streams[1].media_type(), Some(MediaType::Audio));
    assert_eq!(streams[1].params.codec_id, Some(CodecId::Aac));
    assert_eq!(demux.duration().unwrap().as_micros(), 1_500_000);
}

#[test]
fn amf_duration_retains_the_double_value_as_an_exact_decimal_seconds_ratio() {
    let mut bytes = build_flv();
    let old = 1.5_f64.to_be_bytes();
    let new = 1.000_000_007_f64.to_be_bytes();
    let at = bytes
        .windows(old.len())
        .position(|window| window == old)
        .expect("metadata fixture duration");
    bytes[at..at + new.len()].copy_from_slice(&new);
    let demux = open(bytes);
    assert_eq!(
        demux.duration().map(vaco_core::Duration::as_ratio),
        Some((1_000_000_007, 1_000_000_000))
    );
}

#[test]
fn amf_duration_keeps_a_scientific_submicrosecond_double() {
    let mut bytes = build_flv();
    let old = 1.5_f64.to_be_bytes();
    let new = 1e-7_f64.to_be_bytes();
    let at = bytes
        .windows(old.len())
        .position(|window| window == old)
        .expect("metadata fixture duration");
    bytes[at..at + new.len()].copy_from_slice(&new);
    let demux = open(bytes);
    assert_eq!(
        demux.duration().map(vaco_core::Duration::as_ratio),
        Some((1, 10_000_000))
    );
}

#[test]
fn sequence_headers_become_extradata_not_packets() {
    let mut demux = open(build_flv());
    let mut packets = Vec::new();
    loop {
        match demux.read_packet() {
            Ok(p) => packets.push((p.stream_index, p.pts.ticks(), p.dts.ticks(), p.is_key())),
            Err(vaco_core::Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    // Exactly the real frames: one AVC key, one AVC inter, one AAC raw frame.
    // No packet for either sequence header or the script tag.
    assert_eq!(packets.len(), 3);
    assert_eq!(packets[0], (0, Some(0), Some(0), true));
    // The inter frame's CompositionTime (40) offsets pts from dts.
    assert_eq!(packets[1], (0, Some(140), Some(100), false));
    assert_eq!(packets[2], (1, Some(23), Some(23), true));

    let video_extradata = demux.streams()[0].params.extradata.clone();
    assert_eq!(video_extradata, Some(vec![0x01, 0x64, 0x00, 0x0a]));
    let audio_extradata = demux.streams()[1].params.extradata.clone();
    assert_eq!(audio_extradata, Some(vec![0x12, 0x10]));
}

#[test]
fn eof_is_sticky() {
    let mut demux = open(build_flv());
    loop {
        match demux.read_packet() {
            Ok(_) => {}
            Err(vaco_core::Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    assert!(matches!(demux.read_packet(), Err(vaco_core::Error::Eof)));
    assert!(matches!(demux.read_packet(), Err(vaco_core::Error::Eof)));
}

#[test]
fn seeking_to_the_start_lands_on_a_keyframe() {
    let mut demux = open(build_flv());
    while demux.read_packet().is_ok() {}
    demux
        .seek(
            SeekTarget::Timestamp {
                stream_index: 0,
                ts: vaco_core::Timestamp::new(0),
            },
            SeekFlags::empty(),
        )
        .expect("seek");
    let first = demux.read_packet().expect("packet after seek");
    assert!(first.is_key());
}

#[test]
fn byte_seek_to_an_exact_tag_boundary_reads_that_tag() {
    let mut demux = open(build_flv());
    // Byte 9 is where the very first tag's back-pointer begins (a 9-byte
    // file header, no separate `PreviousTagSize0` — see `build_flv`'s
    // comment). That first tag is the `onMetaData` script tag, which yields
    // no packet, so the first one actually read is the AVC sequence
    // header's successor: the first real video frame.
    demux
        .seek(SeekTarget::Byte(9), SeekFlags::empty())
        .expect("byte seek");
    let pkt = demux.read_packet().expect("packet after byte seek");
    assert_eq!(pkt.stream_index, 0);
    assert!(pkt.is_key());
}

#[test]
fn byte_seek_before_the_first_tag_never_panics() {
    // Position 0 is inside the file header, not any tag — resync's forward
    // scan for a plausible header is heuristic on binary content this small
    // (a real file has far more tags to disambiguate against), so this only
    // asserts what must always hold: no panic, and a `Result` either way.
    let mut demux = open(build_flv());
    if demux.seek(SeekTarget::Byte(0), SeekFlags::empty()).is_ok() {
        let _ = demux.read_packet();
    }
}

/// The muxer's own `Lavf...` signature — measured directly on a real
/// `ffmpeg -f flv` file (`fuzz/seeds/diff/flv/h264-video-only.flv`, offset
/// `0xa1`): `onMetaData` carries a plain AMF0 string keyed `encoder`, and
/// before this test's fix `handle_script_tag`'s title/artist/creationdate
/// loop had no entry for it, so it was silently dropped no matter what a
/// real file stated.
#[test]
fn on_meta_data_encoder_field_reaches_demuxer_metadata() {
    let mut file = b"FLV".to_vec();
    file.push(1);
    file.push(0x01); // has video only
    file.extend_from_slice(&9u32.to_be_bytes());

    let mut script = Vec::new();
    vaco_demux_flv::AmfValue::String("onMetaData".to_owned()).encode(&mut script);
    vaco_demux_flv::AmfValue::EcmaArray(vec![(
        "encoder".to_owned(),
        vaco_demux_flv::AmfValue::String("Lavf62.12.100".to_owned()),
    )])
    .encode(&mut script);
    file.extend_from_slice(&tag(18, 0, &script));

    let demux = open(file);
    assert_eq!(
        demux.metadata(),
        &[("encoder".to_owned(), "Lavf62.12.100".to_owned())]
    );
}
