#![allow(clippy::unwrap_used, reason = "test code")]

mod common;

use vaco_core::{Duration, Error};
use vaco_demux_mp4::{Mp4Demuxer, Mp4Options};
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::MemorySource;

#[test]
fn aggregate_and_packet_durations_retain_the_track_clock() {
    for (rate, ticks) in [(44_100, 1024), (90_000, 1), (30_000, 1001)] {
        let mut track = common::simple_track(1, 1, 4, ticks);
        track.timescale = rate;
        let bytes = common::fixture(1000, 0, &[track], &[1, 2, 3, 4]);
        let mut demux = Mp4Demuxer::open(
            Box::new(MemorySource::new(bytes)),
            &NoParsers,
            &FormatOptions::default(),
            Mp4Options::default(),
        )
        .unwrap();
        let expected = Duration::from_ticks(
            i64::from(ticks),
            vaco_core::Rational::new(1, i32::try_from(rate).unwrap()),
        );
        assert_eq!(demux.duration(), expected);
        let packet = demux.read_packet().unwrap();
        assert_eq!(Some(packet.duration), expected);
        assert_eq!(packet.payload(), &[1, 2, 3, 4]);
        assert!(matches!(demux.read_packet(), Err(Error::Eof)));
    }
}
