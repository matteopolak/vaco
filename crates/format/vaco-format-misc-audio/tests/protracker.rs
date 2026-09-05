#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]

use vaco_format_core::Demuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::probe::{ProbeData, ProbeScore};
use vaco_format_misc_audio::protracker;
use vaco_io::{MediaSource, MemorySource};

/// A minimal, spec-shaped four-channel M.K. module: one pattern and one
/// two-word (four-byte) signed sample. Keeping the fixture in source makes its
/// offsets auditable and avoids committing an opaque binary blob.
fn fixture() -> Vec<u8> {
    let mut bytes = vec![0u8; 2_112];
    bytes[..16].copy_from_slice(b"Minimal Vaco MOD");
    for sample in 0..31 {
        let offset = 20 + sample * 30;
        let name = format!("sample{}", sample + 1);
        bytes[offset..offset + name.len()].copy_from_slice(name.as_bytes());
        if sample == 0 {
            bytes[offset + 22..offset + 24].copy_from_slice(&2u16.to_be_bytes());
            bytes[offset + 25] = 64;
            bytes[offset + 28..offset + 30].copy_from_slice(&1u16.to_be_bytes());
        }
    }
    bytes[950] = 1; // song length
    bytes[952] = 0; // order 0 uses pattern 0
    bytes[1080..1084].copy_from_slice(b"M.K.");
    // Pattern 0, row 0, channel 0: period 428, sample 1, no effect.
    bytes[1084..1088].copy_from_slice(&[0x11, 0xac, 0x10, 0]);
    bytes[2_108..].copy_from_slice(&[0x10, 0x20, 0xf0, 0x80]);
    bytes
}

fn open(bytes: Vec<u8>) -> Box<dyn vaco_format_core::Demuxer> {
    let source: Box<dyn MediaSource> = Box::new(MemorySource::new(bytes));
    (protracker::DEMUXER.open)(source, &NoParsers).unwrap()
}

#[test]
fn probe_requires_the_protracker_signature() {
    let bytes = fixture();
    assert_eq!(
        protracker::probe(&ProbeData::new(&bytes)),
        ProbeScore::MAGIC_CHECKED
    );
    assert_eq!(
        protracker::probe(&ProbeData::new(b"Minimal Vaco MOD")),
        ProbeScore::NONE
    );
}

#[test]
fn opens_fixture_and_emits_the_sample_payload() {
    let mut demux = open(fixture());
    assert_eq!(demux.streams().len(), 31);
    assert_eq!(
        demux.streams()[0]
            .params
            .audio
            .as_ref()
            .unwrap()
            .layout
            .as_ref()
            .unwrap()
            .channels,
        1
    );
    assert_eq!(
        demux.streams()[0].metadata,
        [("title".to_string(), "sample1".to_string())]
    );

    let packet = demux.read_packet().unwrap();
    assert_eq!(packet.stream_index, 0);
    assert_eq!(packet.pos, Some(2_108));
    assert_eq!(packet.payload(), &[0x10, 0x20, 0xf0, 0x80]);
    assert_eq!(packet.len, 4);
    assert!(matches!(demux.read_packet(), Err(vaco_core::Error::Eof)));
}

#[test]
fn malformed_signature_and_truncated_sample_are_refused() {
    let mut wrong_signature = fixture();
    wrong_signature[1080..1084].copy_from_slice(b"NOPE");
    assert!(matches!(
        (protracker::DEMUXER.open)(Box::new(MemorySource::new(wrong_signature)), &NoParsers),
        Err(vaco_core::Error::InvalidData(_))
    ));

    let truncated = fixture()[..2_111].to_vec();
    assert!(matches!(
        (protracker::DEMUXER.open)(Box::new(MemorySource::new(truncated)), &NoParsers),
        Err(vaco_core::Error::InvalidData(_))
    ));
}
