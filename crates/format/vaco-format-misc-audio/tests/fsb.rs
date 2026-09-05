#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

use vaco_codec_core::CodecId;
use vaco_format_core::Demuxer;
use vaco_format_misc_audio::fsb::{FsbDemuxer, FsbVersion};
use vaco_io::MemorySource;
use vaco_sampfmt::SampleFmt;

fn fsb5_fixture(mode: u32) -> Vec<u8> {
    let payload = [0x10_u8, 0x20, 0x30, 0x40];
    let mut out = Vec::new();
    out.extend_from_slice(b"FSB5");
    out.extend_from_slice(&1_u32.to_le_bytes());
    out.extend_from_slice(&1_u32.to_le_bytes());
    out.extend_from_slice(&8_u32.to_le_bytes());
    out.extend_from_slice(&0_u32.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&mode.to_le_bytes());
    out.extend_from_slice(&[0; 8 + 16 + 8]);
    // 44.1 kHz code, stereo, offset 0, 4 samples, no metadata chain.
    let packed = (8_u64 << 1) | (1_u64 << 5) | (4_u64 << 34);
    out.extend_from_slice(&packed.to_le_bytes());
    out.extend_from_slice(&payload);
    out
}

#[test]
fn fsb5_pcm_fixture_reports_metadata_and_payload() {
    let mut demuxer = FsbDemuxer::open(Box::new(MemorySource::new(
        include_bytes!("fixtures/fsb5-pcm16.fsb").to_vec(),
    )))
    .unwrap();
    assert_eq!(demuxer.version(), FsbVersion::Five);
    assert_eq!(demuxer.streams().len(), 1);
    let stream = demuxer.streams().first().unwrap();
    assert_eq!(stream.params.audio.as_ref().unwrap().sample_rate, 44_100);
    assert_eq!(
        stream
            .params
            .audio
            .as_ref()
            .unwrap()
            .layout
            .as_ref()
            .unwrap()
            .channels,
        2
    );
    assert_eq!(stream.duration_ts, Some(4));
    assert_eq!(stream.params.codec_id, Some(CodecId::PcmS16le));
    assert_eq!(
        stream.params.audio.as_ref().unwrap().format,
        Some(SampleFmt::S16)
    );
    let packet = demuxer.read_packet().unwrap();
    assert_eq!(packet.pos, Some(68));
    assert_eq!(&packet.payload()[..packet.len], &[0x10, 0x20, 0x30, 0x40]);
    assert!(matches!(demuxer.read_packet(), Err(vaco_core::Error::Eof)));
}

#[test]
fn fsb4_thp_fixture_reports_reference_packet_geometry() {
    let mut demuxer = FsbDemuxer::open(Box::new(MemorySource::new(
        include_bytes!("fixtures/fsb4-thp.fsb").to_vec(),
    )))
    .unwrap();
    assert_eq!(demuxer.version(), FsbVersion::Four);
    assert_eq!(demuxer.streams().len(), 1);
    assert_eq!(demuxer.streams()[0].duration_ts, Some(14));
    let packet = demuxer.read_packet().unwrap();
    assert_eq!(packet.pos, Some(128));
    assert_eq!(packet.len, 16);
    assert_eq!(&packet.payload()[..packet.len], &[0x40; 16]);
    assert_eq!(
        packet.duration.to_ticks(demuxer.streams()[0].time_base),
        Some(14)
    );
    assert!(matches!(demuxer.read_packet(), Err(vaco_core::Error::Eof)));
}

#[test]
fn fsb4_unknown_mode_is_named_refusal() {
    let mut bytes = include_bytes!("fixtures/fsb4-thp.fsb").to_vec();
    bytes[96..100].copy_from_slice(&2_u32.to_be_bytes());
    let error = FsbDemuxer::open(Box::new(MemorySource::new(bytes))).unwrap_err();
    assert!(
        matches!(error, vaco_core::Error::Unsupported(message) if message.contains("Nintendo THP"))
    );
}

#[test]
fn fsb5_unknown_mode_is_named_refusal() {
    let error = FsbDemuxer::open(Box::new(MemorySource::new(fsb5_fixture(6)))).unwrap_err();
    assert!(
        matches!(error, vaco_core::Error::Unsupported(message) if message.contains("sound format 6"))
    );
}
