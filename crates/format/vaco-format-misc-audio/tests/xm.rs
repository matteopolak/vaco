#![allow(
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test code"
)]

use vaco_core::Error;
use vaco_format_core::Demuxer;
use vaco_format_misc_audio::xm::XmDemuxer;
use vaco_io::MemorySource;

fn fixture() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"Extended Module: ");
    bytes.extend_from_slice(b"vaco xm fixture\0\0\0\0\0");
    bytes.push(0x1a);
    bytes.extend_from_slice(b"vaco test writer\0\0\0\0");
    put16_extend(&mut bytes, 0x0104);
    put32_extend(&mut bytes, 276);
    put16_extend(&mut bytes, 1); // song length
    put16_extend(&mut bytes, 0); // restart position
    put16_extend(&mut bytes, 2); // channels
    put16_extend(&mut bytes, 1); // patterns
    put16_extend(&mut bytes, 1); // instruments
    put16_extend(&mut bytes, 1); // linear frequency table
    put16_extend(&mut bytes, 6); // tempo
    put16_extend(&mut bytes, 125); // BPM
    bytes.extend_from_slice(&[0; 256]);

    put32_extend(&mut bytes, 9); // pattern header length
    bytes.push(0); // packing type
    put16_extend(&mut bytes, 1); // rows
    put16_extend(&mut bytes, 0); // no packed events

    put32_extend(&mut bytes, 263); // instrument header size
    let mut instrument = vec![0; 259];
    instrument[0..22].copy_from_slice(b"fixture instrument\0\0\0\0");
    instrument[23..25].copy_from_slice(&1u16.to_le_bytes()); // one sample
    instrument[25..29].copy_from_slice(&40u32.to_le_bytes());
    bytes.extend_from_slice(&instrument);

    put32_extend(&mut bytes, 4); // sample length in points
    put32_extend(&mut bytes, 0); // loop start
    put32_extend(&mut bytes, 0); // loop length
    bytes.extend_from_slice(&[64, 0, 0, 128, 0, 0]); // volume/tune/type/pan/rel/reserved
    bytes.extend_from_slice(b"fixture sample\0\0\0\0\0\0\0\0");
    bytes.extend_from_slice(&[1, 2, 3, 4]); // delta-coded sample payload
    bytes
}

fn put16_extend(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn put32_extend(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

#[test]
fn parses_header_pattern_and_sample_payload() {
    let bytes = fixture();
    assert_eq!(bytes.len(), 652);
    let mut demux = XmDemuxer::open(Box::new(MemorySource::new(bytes))).unwrap();
    assert_eq!(demux.streams().len(), 1);
    assert_eq!(demux.streams()[0].duration_ts, Some(4));
    assert_eq!(demux.streams()[0].frame_count, Some(4));
    assert_eq!(demux.streams()[0].metadata[0].1, "fixture sample");

    let packet = demux.read_packet().unwrap();
    assert_eq!(packet.payload(), &[1, 2, 3, 4]);
    assert_eq!(packet.stream_index, 0);
    assert_eq!(packet.pos, Some(648));
    assert!(matches!(demux.read_packet(), Err(Error::Eof)));

    let mut sixteen_bit = fixture();
    sixteen_bit[622] = 0x10;
    sixteen_bit.extend_from_slice(&[5, 6, 7, 8]);
    let mut demux = XmDemuxer::open(Box::new(MemorySource::new(sixteen_bit))).unwrap();
    assert_eq!(
        demux.read_packet().unwrap().payload(),
        &[1, 2, 3, 4, 5, 6, 7, 8]
    );
}

#[test]
fn refuses_wrong_version_and_malformed_pattern() {
    let mut version = fixture();
    version[58..60].copy_from_slice(&0x0103u16.to_le_bytes());
    assert!(matches!(
        XmDemuxer::open(Box::new(MemorySource::new(version))),
        Err(Error::Unsupported(_))
    ));

    let mut pattern = fixture();
    pattern[340] = 1;
    assert!(matches!(
        XmDemuxer::open(Box::new(MemorySource::new(pattern))),
        Err(Error::InvalidData(_))
    ));

    let mut order = fixture();
    order[80] = 1;
    assert!(matches!(
        XmDemuxer::open(Box::new(MemorySource::new(order))),
        Err(Error::InvalidData(_))
    ));
}

#[test]
fn refuses_reserved_sample_flags_and_truncated_payload() {
    let mut flags = fixture();
    flags[622] = 0x08;
    assert!(matches!(
        XmDemuxer::open(Box::new(MemorySource::new(flags))),
        Err(Error::Unsupported(_))
    ));

    let mut truncated = fixture();
    truncated.pop();
    assert!(matches!(
        XmDemuxer::open(Box::new(MemorySource::new(truncated))),
        Err(Error::InvalidData(_))
    ));

    assert!(matches!(
        XmDemuxer::open(Box::new(MemorySource::forward_only(fixture()))),
        Err(Error::NotSeekable)
    ));
}

#[test]
fn descriptor_only_accepts_xm_signature() {
    use vaco_format_core::probe::{ProbeData, ProbeScore};
    assert_eq!(
        vaco_format_misc_audio::xm::probe(&ProbeData::new(b"Extended Module: ")),
        ProbeScore::MAGIC_CHECKED
    );
    assert_eq!(
        vaco_format_misc_audio::xm::probe(&ProbeData::new(b"Extended Module")),
        ProbeScore::NONE
    );
}
