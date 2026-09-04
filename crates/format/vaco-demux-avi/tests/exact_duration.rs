#![allow(clippy::indexing_slicing, clippy::unwrap_used, reason = "test code")]

use vaco_core::{Duration, Error, ExactDuration, Rational, Timestamp};
use vaco_demux_avi::AviDemuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::MemorySource;

#[test]
fn avi_keeps_the_video_length_at_the_native_rate() {
    let bytes = include_bytes!("fixtures/avi_ntsc_one_frame.avi");
    let mut demux = AviDemuxer::open(
        Box::new(MemorySource::new(bytes.to_vec())),
        &NoParsers,
        &FormatOptions::default(),
    )
    .unwrap();

    // ffprobe 9.0.1 reports time_base=1001/30000, duration_ts=1,
    // duration=0.033367, nb_frames=1, and nb_read_packets=1.
    assert_eq!(demux.streams().len(), 1);
    assert_eq!(demux.streams()[0].time_base, Rational::new(1_001, 30_000));
    assert_eq!(demux.streams()[0].duration_ts, Some(1));
    assert_eq!(demux.duration(), Some(Duration::from_micros(33_366)));
    assert_eq!(
        demux.duration_exact().map(ExactDuration::as_ratio),
        Some((1_001, 30_000))
    );

    let packet = demux.read_packet().unwrap();
    assert_eq!(packet.pts.ticks(), None);
    assert_eq!(packet.dts, Timestamp::ZERO);
    assert_eq!(packet.len, 2_137);
    assert!(matches!(demux.read_packet(), Err(Error::Eof)));
}
