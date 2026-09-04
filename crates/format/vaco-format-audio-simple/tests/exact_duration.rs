#![allow(clippy::indexing_slicing, clippy::unwrap_used, reason = "test code")]

use vaco_core::{Duration, Error, ExactDuration, Timestamp};
use vaco_format_audio_simple::aiff::AiffDemuxer;
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::MemorySource;

#[test]
fn aiff_keeps_the_declared_sample_count_at_the_native_rate() {
    let bytes = include_bytes!("fixtures/aiff_44100_one_frame.aiff");
    let mut demux = AiffDemuxer::open(
        Box::new(MemorySource::new(bytes.to_vec())),
        &FormatOptions::default(),
    )
    .unwrap();

    // ffprobe 9.0.1 reports time_base=1/44100, duration_ts=1024,
    // duration=0.023220, nb_frames=1024, and nb_read_packets=1.
    assert_eq!(demux.streams()[0].duration_ts, Some(1_024));
    assert_eq!(demux.duration(), Some(Duration::from_micros(23_219)));
    assert_eq!(
        demux.duration_exact().map(ExactDuration::as_ratio),
        Some((256, 11_025))
    );

    let packet = demux.read_packet().unwrap();
    assert_eq!(packet.pts, Timestamp::ZERO);
    assert_eq!(packet.len, 2_048);
    assert!(matches!(demux.read_packet(), Err(Error::Eof)));
}
