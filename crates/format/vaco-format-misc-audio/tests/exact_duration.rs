#![allow(clippy::unwrap_used, reason = "test code")]

use vaco_core::{Duration, Error, Timestamp};
use vaco_format_core::discovery::NoParsers;
use vaco_format_misc_audio::{sbc, tta, wavpack};
use vaco_io::MemorySource;

fn check(bytes: &[u8], desc: vaco_format_core::DemuxerDesc, pos: usize, size: usize) {
    let mut demux = (desc.open)(Box::new(MemorySource::new(bytes.to_vec())), &NoParsers).unwrap();
    assert_eq!(
        demux.duration().map(Duration::as_ratio),
        Some((256, 11_025)),
        "{}",
        desc.name
    );
    let packet = demux.read_packet().unwrap();
    assert_eq!(packet.pts, Timestamp::ZERO);
    assert_eq!(packet.duration.as_ratio(), (256, 11_025));
    assert_eq!(packet.len, size);
    assert_eq!(packet.payload(), bytes.get(pos..pos + size).unwrap());
    assert!(matches!(demux.read_packet(), Err(Error::Eof)));
}

#[test]
fn wavpack_retains_a_nonintegral_sample_clock() {
    check(
        include_bytes!("fixtures/wavpack-44100-1024.wv"),
        wavpack::DEMUXER,
        0,
        54,
    );
}

#[test]
fn tta_retains_a_nonintegral_sample_clock() {
    check(
        include_bytes!("fixtures/tta-44100-1024.tta"),
        tta::DEMUXER,
        30,
        194,
    );
}

#[test]
fn sbc_packets_retain_every_sample_in_the_native_clock() {
    let bytes = include_bytes!("fixtures/sbc-44100-1024.sbc");
    let mut demux =
        (sbc::DEMUXER.open)(Box::new(MemorySource::new(bytes.to_vec())), &NoParsers).unwrap();
    for index in 0..8 {
        let packet = demux.read_packet().unwrap();
        assert_eq!(packet.pts.ticks(), Some(index * 128));
        assert_eq!(packet.duration.as_ratio(), (32, 11_025));
        assert_eq!(packet.len, 46);
        let start = usize::try_from(index).unwrap() * 46;
        assert_eq!(packet.payload(), bytes.get(start..start + 46).unwrap());
    }
    assert!(matches!(demux.read_packet(), Err(Error::Eof)));
}
