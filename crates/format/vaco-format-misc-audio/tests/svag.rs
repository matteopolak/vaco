#![allow(clippy::unwrap_used, clippy::panic, reason = "test code")]

use vaco_core::Error;
use vaco_format_core::Demuxer;
use vaco_format_misc_audio::svag::SvagDemuxer;
use vaco_io::MemorySource;
use vaco_packet::{PacketFlags, PacketSideData};

fn build_file(
    data_size: u32,
    sample_rate: u32,
    channels: u32,
    interleave: u32,
    physical_data_len: usize,
) -> Vec<u8> {
    let mut data = b"VAGm".to_vec();
    data.extend_from_slice(&data_size.to_le_bytes());
    data.extend_from_slice(&sample_rate.to_le_bytes());
    data.extend_from_slice(&channels.to_le_bytes());
    data.extend_from_slice(&interleave.to_le_bytes());
    data.extend((0..physical_data_len).map(|i| i as u8));
    data
}

fn duration_ticks(packet: &vaco_packet::Packet) -> Option<i64> {
    packet
        .side_data
        .iter()
        .find_map(|side_data| match side_data {
            PacketSideData::DurationTicks(ticks) => Some(*ticks),
            _ => None,
        })
}

#[test]
fn stereo_header_and_packet_stream_match_the_reference() {
    let data = build_file(320, 44_100, 2, 16, 320);
    let mut demux = SvagDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
    let stream = demux.streams().first().unwrap();
    let audio = stream.params.audio.as_ref().unwrap();

    assert_eq!(audio.sample_rate, 44_100);
    assert_eq!(audio.layout.as_ref().unwrap().channels, 2);
    assert_eq!(stream.duration_ts, Some(280));

    for packet_index in 0..10 {
        let packet = demux.read_packet().unwrap();
        assert_eq!(packet.len, 32);
        assert_eq!(packet.pos, Some(20 + packet_index * 32));
        assert_eq!(
            packet.pts.ticks(),
            Some(i64::try_from(packet_index * 28).unwrap())
        );
        assert_eq!(packet.dts, packet.pts);
        assert_eq!(duration_ticks(&packet), Some(28));
        assert_eq!(packet.flags, PacketFlags::KEY);
    }
    assert!(matches!(demux.read_packet(), Err(Error::Eof)));
}

#[test]
fn interleave_controls_packet_size_and_timestamp_step() {
    let data = build_file(320, 44_100, 2, 32, 320);
    let mut demux = SvagDemuxer::open(Box::new(MemorySource::new(data))).unwrap();

    for packet_index in 0..5 {
        let packet = demux.read_packet().unwrap();
        assert_eq!(packet.len, 64);
        assert_eq!(packet.pos, Some(20 + packet_index * 64));
        assert_eq!(
            packet.pts.ticks(),
            Some(i64::try_from(packet_index * 56).unwrap())
        );
        assert_eq!(duration_ticks(&packet), Some(56));
    }
    assert!(matches!(demux.read_packet(), Err(Error::Eof)));
}

#[test]
fn declared_size_only_controls_duration_and_short_tail_is_corrupt() {
    let data = build_file(64, 44_100, 2, 16, 325);
    let mut demux = SvagDemuxer::open(Box::new(MemorySource::new(data))).unwrap();
    assert_eq!(demux.streams().first().unwrap().duration_ts, Some(56));

    for _ in 0..10 {
        let packet = demux.read_packet().unwrap();
        assert_eq!(packet.len, 32);
    }
    let tail = demux.read_packet().unwrap();
    assert_eq!(tail.len, 5);
    assert_eq!(tail.pos, Some(340));
    assert_eq!(tail.pts.ticks(), None);
    assert_eq!(tail.dts.ticks(), None);
    assert_eq!(duration_ticks(&tail), None);
    assert!(tail.flags.contains(PacketFlags::KEY));
    assert!(tail.flags.contains(PacketFlags::CORRUPT));
    assert!(matches!(demux.read_packet(), Err(Error::Eof)));
}

#[test]
fn malformed_geometry_is_rejected() {
    for (sample_rate, channels, interleave) in [
        (0, 2, 16),
        (44_100, 0, 16),
        (44_100, 2, 0),
        (44_100, 2, 15),
        (44_100, 2, u32::MAX),
    ] {
        let data = build_file(320, sample_rate, channels, interleave, 320);
        assert!(SvagDemuxer::open(Box::new(MemorySource::new(data))).is_err());
    }
}
