#![allow(
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test code"
)]

use vaco_core::Error;
use vaco_format_core::Demuxer;
use vaco_format_misc_audio::bfstm::BfstmDemuxer;
use vaco_io::MemorySource;
use vaco_packet::PacketSideData;

#[derive(Clone, Copy)]
enum Order {
    Big,
    Little,
}

impl Order {
    fn put16(self, data: &mut [u8], offset: usize, value: u16) {
        let bytes = match self {
            Self::Big => value.to_be_bytes(),
            Self::Little => value.to_le_bytes(),
        };
        data[offset..offset + 2].copy_from_slice(&bytes);
    }

    fn put32(self, data: &mut [u8], offset: usize, value: u32) {
        let bytes = match self {
            Self::Big => value.to_be_bytes(),
            Self::Little => value.to_le_bytes(),
        };
        data[offset..offset + 4].copy_from_slice(&bytes);
    }
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

fn stereo_fixture(order: Order, magic: [u8; 4], final_bytes: usize) -> Fixture {
    stereo_fixture_at_rate(order, magic, final_bytes, 32_000)
}

fn stereo_fixture_at_rate(order: Order, magic: [u8; 4], final_bytes: usize, rate: u32) -> Fixture {
    const INFO: usize = 0x40;
    const INFO_SIZE: usize = 0x100;
    const SEEK: usize = 0x140;
    const SEEK_SIZE: usize = 0x20;
    const DATA: usize = 0x160;
    const DATA_HEADER: usize = 0x20;
    const CHANNELS: usize = 2;
    const BLOCK: usize = 32;
    const BLOCKS: usize = 2;
    const SAMPLES: usize = 56;
    const DATA_SIZE: usize = DATA_HEADER + CHANNELS * BLOCKS * BLOCK;
    const FILE_SIZE: usize = DATA + DATA_SIZE;
    const CHANNEL_TABLE: usize = 0xb0;
    const CHANNEL_0: usize = 0xc4;
    const CHANNEL_1: usize = 0xcc;
    const DSP_0: usize = 0xd4;
    const DSP_1: usize = 0x102;
    let final_samples = final_bytes.saturating_mul(14).div_euclid(8);

    let mut bytes = vec![0; FILE_SIZE];
    bytes[0..4].copy_from_slice(&magic);
    order.put16(&mut bytes, 0x04, 0xfeff);
    order.put16(&mut bytes, 0x06, 0x40);
    order.put32(&mut bytes, 0x08, 0x0003_0000);
    order.put32(&mut bytes, 0x0c, FILE_SIZE as u32);
    order.put16(&mut bytes, 0x10, 3);
    for (base, kind, offset, size) in [
        (0x14, 0x4000, INFO, INFO_SIZE),
        (0x20, 0x4001, SEEK, SEEK_SIZE),
        (0x2c, 0x4002, DATA, DATA_SIZE),
    ] {
        order.put16(&mut bytes, base, kind);
        order.put32(&mut bytes, base + 4, offset as u32);
        order.put32(&mut bytes, base + 8, size as u32);
    }

    bytes[INFO..INFO + 4].copy_from_slice(b"INFO");
    order.put32(&mut bytes, INFO + 4, INFO_SIZE as u32);
    order.put16(&mut bytes, INFO + 8, 0x4100);
    order.put32(&mut bytes, INFO + 0x0c, 0x18);
    order.put32(&mut bytes, INFO + 0x14, u32::MAX);
    order.put16(&mut bytes, INFO + 0x18, 0x0101);
    order.put32(&mut bytes, INFO + 0x1c, (CHANNEL_TABLE - (INFO + 8)) as u32);

    let stream = INFO + 0x20;
    bytes[stream] = 2;
    bytes[stream + 2] = CHANNELS as u8;
    order.put32(&mut bytes, stream + 4, rate);
    order.put32(&mut bytes, stream + 0x0c, (SAMPLES + final_samples) as u32);
    order.put32(&mut bytes, stream + 0x10, BLOCKS as u32);
    order.put32(&mut bytes, stream + 0x14, BLOCK as u32);
    order.put32(&mut bytes, stream + 0x18, SAMPLES as u32);
    order.put32(&mut bytes, stream + 0x1c, final_bytes as u32);
    order.put32(&mut bytes, stream + 0x20, final_samples as u32);
    order.put32(&mut bytes, stream + 0x24, BLOCK as u32);
    order.put32(&mut bytes, stream + 0x28, 4);
    order.put32(&mut bytes, stream + 0x2c, SAMPLES as u32);
    order.put16(&mut bytes, stream + 0x30, 0x1f00);
    order.put32(&mut bytes, stream + 0x34, 0x18);

    order.put32(&mut bytes, CHANNEL_TABLE, CHANNELS as u32);
    order.put16(&mut bytes, CHANNEL_TABLE + 4, 0x4102);
    order.put32(
        &mut bytes,
        CHANNEL_TABLE + 8,
        (CHANNEL_0 - CHANNEL_TABLE) as u32,
    );
    order.put16(&mut bytes, CHANNEL_TABLE + 12, 0x4102);
    order.put32(
        &mut bytes,
        CHANNEL_TABLE + 16,
        (CHANNEL_1 - CHANNEL_TABLE) as u32,
    );
    order.put16(&mut bytes, CHANNEL_0, 0x0300);
    order.put32(&mut bytes, CHANNEL_0 + 4, (DSP_0 - CHANNEL_0) as u32);
    order.put16(&mut bytes, CHANNEL_1, 0x0300);
    order.put32(&mut bytes, CHANNEL_1 + 4, (DSP_1 - CHANNEL_1) as u32);
    for index in 0..16 {
        order.put16(&mut bytes, DSP_0 + index * 2, 0x1000 + index as u16);
        order.put16(&mut bytes, DSP_1 + index * 2, 0x2000 + index as u16);
    }

    bytes[SEEK..SEEK + 4].copy_from_slice(b"SEEK");
    order.put32(&mut bytes, SEEK + 4, SEEK_SIZE as u32);
    for (index, value) in [
        0x1111, 0x2222, 0x3333, 0x4444, 0x5555, 0x6666, 0x7777, 0x8888,
    ]
    .into_iter()
    .enumerate()
    {
        order.put16(&mut bytes, SEEK + 8 + index * 2, value);
    }

    bytes[DATA..DATA + 4].copy_from_slice(b"DATA");
    order.put32(&mut bytes, DATA + 4, DATA_SIZE as u32);

    let coefficients: Vec<u8> = bytes[DSP_0..DSP_0 + 32]
        .iter()
        .chain(&bytes[DSP_1..DSP_1 + 32])
        .copied()
        .collect();
    let mut packets = Vec::new();
    let mut samples = Vec::new();
    let mut value = 7u8;
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
            bytes[block_start..block_start + BLOCK].fill(0xee);
            for index in 0..current_bytes {
                bytes[block_start + index] = value;
                raw.push(value);
                value = value.wrapping_add(13);
            }
        }
        let mut packet = Vec::new();
        let raw_len = u32::try_from(raw.len()).unwrap();
        match order {
            Order::Big => {
                packet.extend_from_slice(&raw_len.to_be_bytes());
                packet.extend_from_slice(&(current_samples as u32).to_be_bytes());
            }
            Order::Little => {
                packet.extend_from_slice(&raw_len.to_le_bytes());
                packet.extend_from_slice(&(current_samples as u32).to_le_bytes());
            }
        }
        packet.extend_from_slice(&coefficients);
        packet.extend_from_slice(&bytes[SEEK + 8 + packet_index * 8..SEEK + 16 + packet_index * 8]);
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
    for order in [Order::Big, Order::Little] {
        let fixture = stereo_fixture_at_rate(order, *b"FSTM", 16, 44_100);
        let mut demux = BfstmDemuxer::open(Box::new(MemorySource::new(fixture.bytes))).unwrap();
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
}

#[test]
fn both_magics_and_byte_orders_match_the_measured_stream() {
    for magic in [*b"FSTM", *b"CSTM"] {
        for order in [Order::Big, Order::Little] {
            let fixture = stereo_fixture(order, magic, 16);
            let demux = BfstmDemuxer::open(Box::new(MemorySource::new(fixture.bytes))).unwrap();
            let stream = demux.streams().first().unwrap();
            let audio = stream.params.audio.as_ref().unwrap();
            assert_eq!(audio.sample_rate, 32_000);
            assert_eq!(audio.layout.as_ref().unwrap().channels, 2);
            assert_eq!(stream.time_base.num, 1);
            assert_eq!(stream.time_base.den, 32_000);
            assert_eq!(stream.duration_ts, Some(84));
            assert_eq!(stream.frame_count, Some(84));
        }
    }
}

#[test]
fn packets_match_ffprobe_timing_wrapper_and_final_padding_omission() {
    let fixture = stereo_fixture(Order::Big, *b"FSTM", 16);
    let mut demux = BfstmDemuxer::open(Box::new(MemorySource::new(fixture.bytes))).unwrap();
    let mut pts = 0;
    for (expected, samples) in fixture.packets.iter().zip(&fixture.samples) {
        let packet = demux.read_packet().unwrap();
        assert_eq!(packet.payload(), expected);
        assert_eq!(packet.pts.ticks(), Some(pts));
        assert_eq!(packet.dts.ticks(), Some(pts));
        assert_eq!(ticks(&packet), Some(*samples));
        pts += samples;
    }
    assert!(matches!(demux.read_packet(), Err(Error::Eof)));
}

#[test]
fn little_endian_packets_serialize_prefix_fields_in_file_order() {
    let fixture = stereo_fixture(Order::Little, *b"CSTM", 16);
    let mut demux = BfstmDemuxer::open(Box::new(MemorySource::new(fixture.bytes))).unwrap();
    for expected in &fixture.packets {
        assert_eq!(demux.read_packet().unwrap().payload(), expected);
    }
}

#[test]
fn full_final_block_has_the_normal_packet_shape() {
    let fixture = stereo_fixture(Order::Big, *b"FSTM", 32);
    let mut demux = BfstmDemuxer::open(Box::new(MemorySource::new(fixture.bytes))).unwrap();
    for expected in &fixture.packets {
        let packet = demux.read_packet().unwrap();
        assert_eq!(packet.payload(), expected);
        assert_eq!(packet.len, 144);
        assert_eq!(ticks(&packet), Some(56));
    }
}

#[test]
fn unmeasured_codec_channels_and_geometry_are_named_refusals() {
    let mut ima = stereo_fixture(Order::Big, *b"FSTM", 16).bytes;
    ima[0x60] = 3;
    assert!(matches!(
        BfstmDemuxer::open(Box::new(MemorySource::new(ima))),
        Err(Error::Unsupported(message)) if message.contains("DSP-ADPCM")
    ));

    let mut mono = stereo_fixture(Order::Big, *b"FSTM", 16).bytes;
    mono[0x62] = 1;
    assert!(matches!(
        BfstmDemuxer::open(Box::new(MemorySource::new(mono))),
        Err(Error::Unsupported(message)) if message.contains("stereo")
    ));

    let mut geometry = stereo_fixture(Order::Big, *b"FSTM", 16).bytes;
    Order::Big.put32(&mut geometry, 0x74, 48);
    assert!(matches!(
        BfstmDemuxer::open(Box::new(MemorySource::new(geometry))),
        Err(Error::Unsupported(message)) if message.contains("geometry")
    ));
}

#[test]
fn section_reference_outside_the_declared_file_is_rejected() {
    let mut fixture = stereo_fixture(Order::Big, *b"FSTM", 16).bytes;
    Order::Big.put32(&mut fixture, 0x30, 0xffff_ff00);
    assert!(matches!(
        BfstmDemuxer::open(Box::new(MemorySource::new(fixture))),
        Err(Error::InvalidData(message)) if message.contains("DATA")
    ));
}
