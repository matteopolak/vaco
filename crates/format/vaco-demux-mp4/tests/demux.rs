//! End-to-end demuxing, on fixtures built box by box.
//!
//! The conformance measurements against `ffprobe` live in the crate's doc file
//! and in `examples/mp4dump.rs`; these are the cases that pin a rule down
//! without needing the reference binary on the machine.

mod common;

use common::{ChunkSource, MDAT_PAYLOAD, fixture, simple_track};
use vaco_core::{Error, MediaType, Rational, Timestamp};
use vaco_demux_mp4::{Mp4Demuxer, Mp4Options};
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::{Demuxer, FormatOptions, SeekFlags, SeekTarget};
use vaco_format_isom::build::{StblSpec, TrackSpec};
use vaco_io::{MediaSource, MemorySource};

#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::integer_division,
    clippy::useless_vec,
    reason = "test code"
)]
mod tests {
    use super::*;

    fn open(data: Vec<u8>) -> Mp4Demuxer {
        let src: Box<dyn MediaSource> = Box::new(MemorySource::new(data));
        Mp4Demuxer::open(
            src,
            &NoParsers,
            &FormatOptions::default(),
            Mp4Options::default(),
        )
        .unwrap()
    }

    fn open_with(data: Vec<u8>, mp4: Mp4Options) -> Mp4Demuxer {
        let src: Box<dyn MediaSource> = Box::new(MemorySource::new(data));
        Mp4Demuxer::open(src, &NoParsers, &FormatOptions::default(), mp4).unwrap()
    }

    fn drain(demux: &mut Mp4Demuxer) -> Vec<(u32, i64, i64, u64, usize)> {
        let mut out = Vec::new();
        loop {
            match demux.read_packet() {
                Ok(p) => out.push((
                    p.stream_index,
                    p.pts.ticks().unwrap_or(i64::MIN),
                    p.dts.ticks().unwrap_or(i64::MIN),
                    p.pos.unwrap_or(0),
                    p.payload().len(),
                )),
                Err(Error::Eof) => break,
                Err(e) => panic!("read failed: {e}"),
            }
        }
        out
    }

    fn one_track(n: u32, size: u32) -> Vec<u8> {
        let media = vec![0xAB; (n * size) as usize];
        fixture(1000, 0, &[simple_track(1, n, size, 512)], &media)
    }

    #[test]
    fn a_single_track_reports_its_sample_table() {
        let demux = open(one_track(10, 4));
        assert_eq!(demux.streams().len(), 1);
        let s = &demux.streams()[0];
        assert_eq!(s.id, Some(1));
        assert_eq!(s.time_base, Rational::new(1, 12800));
        assert_eq!(s.media_type(), Some(MediaType::Video));
        assert_eq!(s.frame_count, Some(10));
        // 10 samples of 4 bytes over 10 * 512 ticks at 12800/s.
        assert_eq!(s.params.bit_rate, Some(40 * 8 * 12800 / 5120));
        assert_eq!(s.duration_ts, Some(5120));
    }

    #[test]
    fn packets_carry_offsets_sizes_and_times() {
        let mut demux = open(one_track(4, 6));
        let packets = drain(&mut demux);
        assert_eq!(
            packets,
            vec![
                (0, 0, 0, MDAT_PAYLOAD, 6),
                (0, 512, 512, MDAT_PAYLOAD + 6, 6),
                (0, 1024, 1024, MDAT_PAYLOAD + 12, 6),
                (0, 1536, 1536, MDAT_PAYLOAD + 18, 6),
            ]
        );
    }

    #[test]
    fn eof_is_sticky() {
        let mut demux = open(one_track(2, 4));
        drain(&mut demux);
        assert!(matches!(demux.read_packet(), Err(Error::Eof)));
        assert!(matches!(demux.read_packet(), Err(Error::Eof)));
    }

    #[test]
    fn every_chunk_size_produces_the_same_packets() {
        let data = one_track(64, 5);
        let whole = drain(&mut open(data.clone()));
        for chunk in [1usize, 3, 4096, 1 << 20] {
            let src: Box<dyn MediaSource> = Box::new(ChunkSource::new(data.clone(), chunk, true));
            let mut demux = Mp4Demuxer::open(
                src,
                &NoParsers,
                &FormatOptions::default(),
                Mp4Options::default(),
            )
            .unwrap();
            assert_eq!(drain(&mut demux), whole, "chunk size {chunk}");
        }
    }

    #[test]
    fn a_faststart_file_demuxes_on_a_pipe() {
        let n = 8u32;
        let size = 4u32;
        let data = common::fixture_faststart(1000, 0, &vec![0x5A; (n * size) as usize], |base| {
            let mut t = simple_track(1, n, size, 512);
            t.stbl.stco = (0..n)
                .map(|i| u32::try_from(base).unwrap_or(0) + i * size)
                .collect();
            vec![t]
        });
        for chunk in [1usize, 4096] {
            let src: Box<dyn MediaSource> = Box::new(ChunkSource::new(data.clone(), chunk, false));
            let mut demux = Mp4Demuxer::open(
                src,
                &NoParsers,
                &FormatOptions::default(),
                Mp4Options::default(),
            )
            .unwrap();
            assert_eq!(demux.streams().len(), 1);
            assert_eq!(drain(&mut demux).len(), n as usize, "chunk {chunk}");
        }
    }

    #[test]
    fn a_moov_after_mdat_on_a_pipe_names_the_fix() {
        let data = one_track(4, 4);
        let src: Box<dyn MediaSource> = Box::new(ChunkSource::new(data, 64, false));
        let Err(err) = Mp4Demuxer::open(
            src,
            &NoParsers,
            &FormatOptions::default(),
            Mp4Options::default(),
        ) else {
            panic!("moov after mdat is unreadable without seeking");
        };
        let text = err.to_string();
        assert!(text.contains("faststart"), "{text}");
    }

    #[test]
    fn a_zero_media_timescale_drops_the_track() {
        let mut good = simple_track(1, 4, 4, 512);
        let mut bad = simple_track(2, 4, 4, 512);
        bad.timescale = 0;
        good.timescale = 12_800;
        let demux = open(fixture(1000, 0, &[good, bad], &[0; 64]));
        assert_eq!(demux.streams().len(), 1);
        assert_eq!(demux.streams()[0].id, Some(1));
    }

    #[test]
    fn a_uniform_stsz_claiming_four_billion_samples_terminates() {
        // The gap `vaco-format-isom` left to its caller: twelve bytes can
        // declare `sample_count = 0xFFFF_FFFF` with no payload to clamp it.
        let mut track = simple_track(1, 1, 4, 512);
        track.stbl.stsz = Vec::new();
        track.stbl.stsz_uniform = Some((4, u32::MAX));
        track.stbl.stsc = vec![(1, u32::MAX, 1)];
        track.stbl.stco = vec![u32::try_from(MDAT_PAYLOAD).unwrap()];
        let data = fixture(1000, 0, &[track], &[0; 32]);
        let mut demux = open(data);
        assert_eq!(demux.streams()[0].frame_count, Some(u64::from(u32::MAX)));
        // Reading terminates, and quickly: the source is a few hundred bytes,
        // so at most a few hundred samples can lie inside it.
        let packets = drain(&mut demux);
        assert!(packets.len() < 1024, "{} packets", packets.len());
    }

    #[test]
    fn an_edit_list_shifts_both_timestamps_and_trims_the_head() {
        let mut track = simple_track(1, 4, 4, 512);
        track.timescale = 12_800;
        // One non-empty edit starting 1024 ticks into the media.
        track.elst = vec![(2000, 1024, 1)];
        let data = fixture(1000, 2000, &[track], &[0; 64]);
        let mut demux = open(data);
        let packets = drain(&mut demux);
        assert_eq!(packets[0].1, -1024, "first pts is shifted by -media_time");
        assert_eq!(packets[0].2, -1024);
        assert_eq!(demux.streams()[0].start_time, Timestamp::ZERO);
    }

    #[test]
    fn ignore_editlist_reports_raw_media_time() {
        let mut track = simple_track(1, 4, 4, 512);
        track.elst = vec![(2000, 1024, 1)];
        let data = fixture(1000, 2000, &[track], &[0; 64]);
        let mut demux = open_with(
            data,
            Mp4Options {
                ignore_editlist: true,
                ..Mp4Options::default()
            },
        );
        assert_eq!(drain(&mut demux)[0].1, 0);
    }

    #[test]
    fn an_empty_edit_delays_the_track() {
        let mut track = simple_track(1, 4, 4, 512);
        track.elst = vec![(520, -1, 1), (2000, 0, 1)];
        let data = fixture(1000, 2520, &[track], &[0; 64]);
        let mut demux = open(data);
        // 520 movie ticks at 12800/1000 is 6656 media ticks.
        assert_eq!(demux.streams()[0].start_time, Timestamp::new(6656));
        assert_eq!(drain(&mut demux)[0].1, 6656);
    }

    #[test]
    fn a_truncated_file_still_reports_its_streams() {
        let n = 16u32;
        let full = common::fixture_faststart(1000, 0, &vec![0; (n * 8) as usize], |base| {
            let mut t = simple_track(1, n, 8, 512);
            t.stbl.stco = (0..n)
                .map(|i| u32::try_from(base).unwrap_or(0) + i * 8)
                .collect();
            vec![t]
        });
        let mut cut = full.clone();
        cut.truncate(full.len() - 40);
        let mut demux = open(cut);
        assert_eq!(demux.streams().len(), 1);
        // `nb_frames` still reports the table's count (plan 18 §3.1.10,
        // VERIFY-M7); the samples past the end are simply not readable.
        assert_eq!(demux.streams()[0].frame_count, Some(u64::from(n)));
        let packets = drain(&mut demux);
        assert_eq!(packets.len(), (n - 5) as usize);
    }

    #[test]
    fn seeking_lands_on_the_sync_sample_at_or_before_the_target() {
        let n = 20u32;
        let mut track = simple_track(1, n, 4, 512);
        track.stbl.has_stss = true;
        track.stbl.stss = vec![1, 6, 11, 16];
        let data = fixture(1000, 0, &[track], &[0; 128]);
        let mut demux = open(data);
        demux
            .seek(
                SeekTarget::Timestamp {
                    stream_index: 0,
                    ts: Timestamp::new(512 * 8),
                },
                SeekFlags::BACKWARD,
            )
            .unwrap();
        let packets = drain(&mut demux);
        // Sample 5 (one-based 6) is the sync sample at or before sample 8.
        assert_eq!(packets[0].2, 512 * 5);
        assert_eq!(packets.len(), (n - 5) as usize);
    }

    #[test]
    fn seeking_backwards_to_zero_replays_the_whole_track() {
        let mut demux = open(one_track(12, 4));
        let all = drain(&mut demux);
        demux
            .seek(
                SeekTarget::Timestamp {
                    stream_index: 0,
                    ts: Timestamp::ZERO,
                },
                SeekFlags::BACKWARD,
            )
            .unwrap();
        assert_eq!(drain(&mut demux), all);
    }

    #[test]
    fn two_tracks_interleave_by_file_position_inside_the_window() {
        // Rule MP4-O1 as measured: within one second of each other, file order
        // decides. Both tracks' samples alternate in the `mdat`.
        let mut a = simple_track(1, 4, 4, 512);
        let mut b = simple_track(2, 4, 4, 512);
        let base = u32::try_from(MDAT_PAYLOAD).unwrap();
        a.stbl.stco = (0..4).map(|i| base + i * 8).collect();
        b.stbl.stco = (0..4).map(|i| base + 4 + i * 8).collect();
        b.handler = *b"soun";
        let data = fixture(1000, 0, &[a, b], &[0; 64]);
        let mut demux = open(data);
        let order: Vec<u32> = drain(&mut demux).iter().map(|p| p.0).collect();
        assert_eq!(order, vec![0, 1, 0, 1, 0, 1, 0, 1]);
    }

    #[test]
    fn a_stsc_that_does_not_start_at_chunk_one_drops_the_track() {
        let mut track = simple_track(1, 4, 4, 512);
        track.stbl.stsc = vec![(2, 1, 1)];
        let data = fixture(1000, 0, &[track], &[0; 64]);
        let demux = open(data);
        assert!(demux.streams().is_empty());
    }

    #[test]
    fn a_file_with_no_moov_is_refused() {
        let data = vaco_format_isom::build::bx(b"ftyp", b"isom\x00\x00\x02\x00isom");
        let src: Box<dyn MediaSource> = Box::new(MemorySource::new(data));
        assert!(
            Mp4Demuxer::open(
                src,
                &NoParsers,
                &FormatOptions::default(),
                Mp4Options::default()
            )
            .is_err()
        );
    }

    #[test]
    fn an_empty_track_reports_no_frame_count() {
        let track = TrackSpec {
            track_id: 1,
            stbl: StblSpec {
                stsd: Some(common::avc1_stsd()),
                ..StblSpec::default()
            },
            ..TrackSpec::default()
        };
        let demux = open(fixture(1000, 0, &[track], &[]));
        assert_eq!(demux.streams().len(), 1);
        assert_eq!(demux.streams()[0].frame_count, None);
    }
}
