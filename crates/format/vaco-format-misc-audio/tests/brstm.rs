#![allow(
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test code"
)]

use vaco_core::Error;
use vaco_format_core::Demuxer;
use vaco_format_misc_audio::brstm::BrstmDemuxer;
use vaco_io::MemorySource;
use vaco_packet::PacketSideData;

fn be16(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}

fn be32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

fn ticks(packet: &vaco_packet::Packet) -> Option<i64> {
    packet
        .side_data
        .iter()
        .find_map(|side_data| match side_data {
            PacketSideData::DurationTicks(value) => Some(*value),
            _ => None,
        })
}

struct Fixture {
    bytes: Vec<u8>,
    packets: Vec<Vec<u8>>,
    samples: Vec<i64>,
}

/// A patterned, source-derived stereo DSP-ADPCM BRSTM. It makes both the
/// per-channel coefficient prefix and each ADPC history-table entry visible.
fn stereo_fixture(final_bytes: usize) -> Fixture {
    stereo_fixture_at_rate(final_bytes, 32_000)
}

fn stereo_fixture_at_rate(final_bytes: usize, rate: u16) -> Fixture {
    const HEAD: usize = 0x40;
    const HEAD_SIZE: usize = 0x100;
    const ADPC: usize = HEAD + HEAD_SIZE;
    const ADPC_SIZE: usize = 0x20;
    const DATA: usize = ADPC + ADPC_SIZE;
    const DATA_HEADER: usize = 0x20;
    const CHANNELS: usize = 2;
    const BLOCK: usize = 32;
    const BLOCKS: usize = 3;
    const SAMPLES: usize = 56;
    const DATA_SIZE: usize = DATA_HEADER + CHANNELS * ((BLOCKS - 1) * BLOCK + BLOCK);
    const FILE_SIZE: usize = DATA + DATA_SIZE;
    let final_samples = final_bytes.saturating_mul(14).div_euclid(8);

    let mut bytes = vec![0; FILE_SIZE];
    bytes[0..4].copy_from_slice(b"RSTM");
    bytes[4..6].copy_from_slice(&[0xfe, 0xff]);
    bytes[6] = 1;
    be32(&mut bytes, 0x08, FILE_SIZE as u32);
    be16(&mut bytes, 0x0c, 0x40);
    be16(&mut bytes, 0x0e, 3);
    be32(&mut bytes, 0x10, HEAD as u32);
    be32(&mut bytes, 0x14, HEAD_SIZE as u32);
    be32(&mut bytes, 0x18, ADPC as u32);
    be32(&mut bytes, 0x1c, ADPC_SIZE as u32);
    be32(&mut bytes, 0x20, DATA as u32);
    be32(&mut bytes, 0x24, DATA_SIZE as u32);

    bytes[HEAD..HEAD + 4].copy_from_slice(b"HEAD");
    be32(&mut bytes, HEAD + 4, HEAD_SIZE as u32);
    be32(&mut bytes, HEAD + 8, 0x0100_0000);
    be32(&mut bytes, HEAD + 0x0c, 0x18);
    be32(&mut bytes, HEAD + 0x10, 0x0100_0000);
    be32(&mut bytes, HEAD + 0x14, 0x4c);
    be32(&mut bytes, HEAD + 0x18, 0x0100_0000);
    be32(&mut bytes, HEAD + 0x1c, 0x5c);
    let part1 = HEAD + 0x20;
    bytes[part1] = 2;
    bytes[part1 + 2] = CHANNELS as u8;
    be16(&mut bytes, part1 + 4, rate);
    be32(
        &mut bytes,
        part1 + 0x0c,
        (2 * SAMPLES + final_samples) as u32,
    );
    be32(&mut bytes, part1 + 0x10, (DATA + DATA_HEADER) as u32);
    be32(&mut bytes, part1 + 0x14, BLOCKS as u32);
    be32(&mut bytes, part1 + 0x18, BLOCK as u32);
    be32(&mut bytes, part1 + 0x1c, SAMPLES as u32);
    be32(&mut bytes, part1 + 0x20, final_bytes as u32);
    be32(&mut bytes, part1 + 0x24, final_samples as u32);
    be32(&mut bytes, part1 + 0x28, BLOCK as u32);
    be32(&mut bytes, part1 + 0x2c, SAMPLES as u32);
    be32(&mut bytes, part1 + 0x30, 8);
    let part2 = HEAD + 0x54;
    bytes[part2] = 1;
    bytes[part2 + 4] = 1;
    be32(&mut bytes, part2 + 8, 0x58);
    bytes[HEAD + 0x60..HEAD + 0x63].copy_from_slice(&[2, 0, 1]);
    let part3 = HEAD + 0x64;
    bytes[part3] = CHANNELS as u8;
    let channel_info = part3 + 4 + CHANNELS * 8;
    for channel in 0..CHANNELS {
        let table = part3 + 4 + channel * 8;
        let info = channel_info + channel * 0x38;
        be32(&mut bytes, table, 0x0100_0000);
        be32(&mut bytes, table + 4, (info - (HEAD + 8)) as u32);
        be32(&mut bytes, info, 0x0100_0000);
        be32(&mut bytes, info + 4, (info + 8 - (HEAD + 8)) as u32);
        for index in 0..32 {
            bytes[info + 8 + index] = 0x10 * (channel + 1) as u8 + index as u8;
        }
    }
    bytes[ADPC..ADPC + 4].copy_from_slice(b"ADPC");
    be32(&mut bytes, ADPC + 4, 32);
    for index in 0..24 {
        bytes[ADPC + 8 + index] = 0xa0 + index as u8;
    }
    bytes[DATA..DATA + 4].copy_from_slice(b"DATA");
    be32(&mut bytes, DATA + 4, DATA_SIZE as u32);
    be32(&mut bytes, DATA + 8, 0x18);

    let coefficients: Vec<u8> = (0x10..0x30).chain(0x20..0x40).collect();
    let mut packets = Vec::new();
    let mut samples = Vec::new();
    let mut value = 1u8;
    for packet_index in 0..BLOCKS {
        let current_bytes = if packet_index + 1 == BLOCKS {
            final_bytes
        } else {
            BLOCK
        };
        let current_samples = current_bytes.saturating_mul(14).div_euclid(8);
        let mut raw = Vec::new();
        for channel in 0..CHANNELS {
            let block_start = DATA + DATA_HEADER + (packet_index * CHANNELS + channel) * BLOCK;
            for index in 0..current_bytes {
                bytes[block_start + index] = value;
                raw.push(value);
                value = value.wrapping_add(1);
            }
        }
        let mut packet = Vec::new();
        packet.extend_from_slice(&(raw.len() as u32).to_be_bytes());
        packet.extend_from_slice(&(current_samples as u32).to_be_bytes());
        packet.extend_from_slice(&coefficients);
        packet.extend_from_slice(&bytes[ADPC + 8 + packet_index * 8..ADPC + 16 + packet_index * 8]);
        packet.extend_from_slice(&raw);
        packets.push(packet);
        samples.push(i64::try_from(current_samples).unwrap());
    }
    Fixture {
        bytes,
        packets,
        samples,
    }
}

#[test]
fn nonintegral_clock_preserves_packet_sample_counts() {
    let fixture = stereo_fixture_at_rate(16, 44_100);
    let mut demux = BrstmDemuxer::open(Box::new(MemorySource::new(fixture.bytes))).unwrap();
    for (expected, samples) in fixture.packets.iter().zip(&fixture.samples) {
        let packet = demux.read_packet().unwrap();
        assert_eq!(packet.payload(), expected);
        assert_eq!(
            Some(packet.duration),
            vaco_core::Duration::from_ticks(*samples, vaco_core::Rational::new(1, 44_100))
        );
    }
    assert!(matches!(demux.read_packet(), Err(Error::Eof)));
}

#[test]
fn mono_is_refused_until_a_reference_accepted_fixture_exists() {
    let mut fixture = stereo_fixture(16).bytes;
    fixture[0x62] = 1;
    assert!(BrstmDemuxer::open(Box::new(MemorySource::new(fixture))).is_err());
}

#[test]
fn stereo_packets_match_ffprobe_bytes_md5_timing_and_final_padding_omission() {
    let fixture = stereo_fixture(16);
    let mut demux = BrstmDemuxer::open(Box::new(MemorySource::new(fixture.bytes))).unwrap();
    let stream = demux.streams().first().unwrap();
    assert_eq!(stream.params.audio.as_ref().unwrap().sample_rate, 32_000);
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
    assert_eq!(stream.duration_ts, Some(140));
    let hashes = [
        "a0afef4e565b2f6c8343e270d0713396",
        "32b50d01bc9976593653862e44b3e441",
        "8a887c4ec1278e9a2dabb6cf91597bb5",
    ];
    let mut pts = 0;
    for ((expected, samples), hash) in fixture.packets.iter().zip(&fixture.samples).zip(hashes) {
        let packet = demux.read_packet().unwrap();
        assert_eq!(packet.payload(), expected, "ffprobe MD5: {hash}");
        assert_eq!(packet.len, expected.len());
        assert_eq!(packet.pts.ticks(), Some(pts));
        assert_eq!(packet.dts.ticks(), Some(pts));
        assert_eq!(ticks(&packet), Some(*samples));
        pts += samples;
    }
    assert!(matches!(demux.read_packet(), Err(Error::Eof)));
}

#[test]
fn full_final_block_has_the_same_packet_shape_as_a_normal_block() {
    let fixture = stereo_fixture(32);
    let mut demux = BrstmDemuxer::open(Box::new(MemorySource::new(fixture.bytes))).unwrap();
    for expected in &fixture.packets {
        let packet = demux.read_packet().unwrap();
        assert_eq!(packet.payload(), expected);
        assert_eq!(packet.len, 144);
        assert_eq!(ticks(&packet), Some(56));
    }
}
