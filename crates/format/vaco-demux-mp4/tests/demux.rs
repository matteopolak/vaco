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
    clippy::expect_used,
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

    /// An unrecognized `hdlr` handler used to drop the whole track outright.
    /// Measured against the reference on a corrupted `moov` whose `stsd`
    /// sample entry (`avc1`) was still intact: it recovers a real video
    /// stream from the codec, not merely a `data` placeholder — matching
    /// `codec_parameters`'s own codec-id fallback, which an eagerly-resolved
    /// `media_type` used to shadow before this fix let it run.
    #[test]
    fn an_unrecognized_handler_falls_back_to_the_stsd_entrys_own_codec() {
        let mut track = simple_track(1, 4, 4, 512);
        track.handler = *b"zzzz";
        let demux = open(fixture(1000, 0, &[track], &[0; 64]));
        assert_eq!(demux.streams().len(), 1);
        assert_eq!(demux.streams()[0].media_type(), Some(MediaType::Video));
    }

    /// When *neither* the handler nor the `stsd` sample entry says what a
    /// track is (no sample description at all here), it is salvaged as
    /// `Data` rather than dropped — the same fallback the handler table
    /// already uses for a *recognized* non-AV handler (`meta`, `tmcd`).
    #[test]
    fn a_track_with_no_handler_and_no_stsd_entry_is_salvaged_as_data_not_dropped() {
        let mut track = simple_track(1, 4, 4, 512);
        track.handler = *b"zzzz";
        track.stbl.stsd_box = None;
        let demux = open(fixture(1000, 0, &[track], &[0; 64]));
        assert_eq!(demux.streams().len(), 1);
        assert_eq!(demux.streams()[0].media_type(), Some(MediaType::Data));
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
                stsd_box: Some(common::avc1_stsd()),
                ..StblSpec::default()
            },
            ..TrackSpec::default()
        };
        let demux = open(fixture(1000, 0, &[track], &[]));
        assert_eq!(demux.streams().len(), 1);
        assert_eq!(demux.streams()[0].frame_count, None);
    }

    /// A `tref \u{2023} chap` track carrying Apple's simple `text` samples
    /// becomes chapters when there is no Nero `chpl` to take precedence.
    #[test]
    fn a_quicktime_chapter_track_becomes_chapters() {
        use vaco_format_isom::FourCc;
        use vaco_format_isom::writer;

        // A bare `text` sample entry: no handler-specific body, so
        // `SampleEntry::parse` treats everything after the eight-byte header
        // as extensions, and none are needed for a chapter title to be read.
        let mut text_entry_body = vec![0u8; 6]; // reserved
        text_entry_body.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
        let mut stsd_body = 1u32.to_be_bytes().to_vec();
        stsd_body.extend_from_slice(&vaco_format_isom::build::bx(b"text", &text_entry_body));
        // `StblSpec::stsd_box` takes a **complete** box, so build one.
        //
        // It used to take the fullbox content and wrap it, and the two
        // conventions coexisted: `common::avc1_stsd` handed it a whole box,
        // which got wrapped again into `stsd` inside `stsd` and parsed as
        // nothing — silently, because the tests using it asserted timing and
        // offsets rather than sample entries. This test was the first to need
        // real `parse_stsd` output and had to compensate by hand.
        let text_stsd = vaco_format_isom::build::fullbx(b"stsd", 0, 0, &stsd_body);

        let mut video = simple_track(1, 1, 4, 1024);
        video.tref = writer::tref(&writer::tref_entry(FourCc::new(b"chap"), &[2]));

        let sample1 = {
            let mut s = 2u16.to_be_bytes().to_vec();
            s.extend_from_slice(b"AB");
            s
        };
        let sample2 = {
            let mut s = 2u16.to_be_bytes().to_vec();
            s.extend_from_slice(b"CD");
            s
        };
        let text_track = TrackSpec {
            track_id: 2,
            handler: *b"text",
            timescale: 1000,
            media_duration: 1000,
            stbl: StblSpec {
                stsd_box: Some(text_stsd),
                stts: vec![(1, 500), (1, 500)],
                stsc: vec![(1, 2, 1)],
                stsz: vec![sample1.len() as u32, sample2.len() as u32],
                stco: vec![MDAT_PAYLOAD as u32 + 4],
                has_stss: false,
                ..StblSpec::default()
            },
            ..TrackSpec::default()
        };

        let mut media = vec![0u8; 4]; // the video sample
        media.extend_from_slice(&sample1);
        media.extend_from_slice(&sample2);

        let demux = open(fixture(1000, 0, &[video, text_track], &media));
        let chapters = demux.chapters();
        assert_eq!(chapters.len(), 2, "one chapter per text sample");
        assert_eq!(
            chapters[0].metadata[0],
            ("title".to_owned(), "AB".to_owned())
        );
        assert_eq!(chapters[0].start.ticks(), Some(0));
        // 500 ticks at 1/1000 is 0.5 s, which is 5 000 000 in the chapter time
        // base's 100 ns units.
        assert_eq!(chapters[0].end.ticks(), Some(5_000_000));
        assert_eq!(
            chapters[1].metadata[0],
            ("title".to_owned(), "CD".to_owned())
        );
        assert_eq!(chapters[1].start.ticks(), Some(5_000_000));
        assert!(
            chapters[1].end.is_none(),
            "the last chapter has no stated end"
        );
    }

    /// Nero `chpl` wins over a `tref \u{2023} chap` track when both are
    /// present — an assumption, not a measurement; see the crate's doc file.
    #[test]
    fn chpl_chapters_take_precedence_over_a_quicktime_chapter_track() {
        use vaco_format_isom::FourCc;
        use vaco_format_isom::writer;

        let mut text_entry_body = vec![0u8; 6];
        text_entry_body.extend_from_slice(&1u16.to_be_bytes());
        let mut stsd_body = 1u32.to_be_bytes().to_vec();
        stsd_body.extend_from_slice(&vaco_format_isom::build::bx(b"text", &text_entry_body));
        // `StblSpec::stsd_box` takes a **complete** box, so build one.
        //
        // It used to take the fullbox content and wrap it, and the two
        // conventions coexisted: `common::avc1_stsd` handed it a whole box,
        // which got wrapped again into `stsd` inside `stsd` and parsed as
        // nothing — silently, because the tests using it asserted timing and
        // offsets rather than sample entries. This test was the first to need
        // real `parse_stsd` output and had to compensate by hand.
        let text_stsd = vaco_format_isom::build::fullbx(b"stsd", 0, 0, &stsd_body);

        let mut video = simple_track(1, 1, 4, 1024);
        video.tref = writer::tref(&writer::tref_entry(FourCc::new(b"chap"), &[2]));

        let sample1 = {
            let mut s = 2u16.to_be_bytes().to_vec();
            s.extend_from_slice(b"AB");
            s
        };
        let text_track = TrackSpec {
            track_id: 2,
            handler: *b"text",
            timescale: 1000,
            media_duration: 500,
            stbl: StblSpec {
                stsd_box: Some(text_stsd),
                stts: vec![(1, 500)],
                stsc: vec![(1, 1, 1)],
                stsz: vec![sample1.len() as u32],
                stco: vec![MDAT_PAYLOAD as u32 + 4],
                has_stss: false,
                ..StblSpec::default()
            },
            ..TrackSpec::default()
        };

        let mut media = vec![0u8; 4];
        media.extend_from_slice(&sample1);

        // `fixture` has no `udta`/`chpl` support of its own; build the file by
        // hand instead, appending a Nero chapter list after the tracks.
        let mut moov_tracks = Vec::new();
        for t in [&video, &text_track] {
            moov_tracks.extend_from_slice(&vaco_format_isom::build::trak(t));
        }
        let entries = vec![vaco_format_isom::writer::chpl_entry(0, "From chpl")];
        let udta = vaco_format_isom::build::bx(b"udta", &vaco_format_isom::writer::chpl(&entries));

        let mut mvhd = 0u32.to_be_bytes().to_vec();
        mvhd.extend_from_slice(&0u32.to_be_bytes());
        mvhd.extend_from_slice(&1000u32.to_be_bytes());
        mvhd.extend_from_slice(&0u32.to_be_bytes());
        mvhd.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        mvhd.extend_from_slice(&0x0100u16.to_be_bytes());
        mvhd.extend_from_slice(&[0u8; 10]);
        for v in vaco_format_isom::fixed::IDENTITY_MATRIX {
            mvhd.extend_from_slice(&v.to_be_bytes());
        }
        mvhd.extend_from_slice(&[0u8; 24]);
        mvhd.extend_from_slice(&2u32.to_be_bytes());
        let mut moov = vaco_format_isom::build::fullbx(b"mvhd", 0, 0, &mvhd);
        moov.extend_from_slice(&moov_tracks);
        moov.extend_from_slice(&udta);

        let mut out = vaco_format_isom::build::bx(b"ftyp", b"isom\x00\x00\x02\x00isom");
        assert_eq!(out.len() as u64, MDAT_PAYLOAD - 8);
        out.extend_from_slice(&vaco_format_isom::build::bx(b"mdat", &media));
        out.extend_from_slice(&vaco_format_isom::build::bx(b"moov", &moov));

        let demux = open(out);
        let chapters = demux.chapters();
        assert_eq!(chapters.len(), 1);
        assert_eq!(
            chapters[0].metadata[0],
            ("title".to_owned(), "From chpl".to_owned())
        );
    }

    /// The `avc1` fixture actually parses into a sample entry.
    ///
    /// This test exists because for a long time it would have failed. Every
    /// fixture built with `common::avc1_stsd` handed `StblSpec` a complete
    /// `stsd` box while the builder expected the fullbox *content*, so each one
    /// became an `stsd` nested inside an `stsd`: the parser read the inner
    /// box's size field as the outer's version and flags, and its `FourCc` as the
    /// entry count. `parse_stsd` returned nothing useful and every codec
    /// parameter stayed `None`.
    ///
    /// Nothing caught it because nothing asked. The tests using those fixtures
    /// asserted timing, chunk offsets and packet boundaries — all of which the
    /// corruption left alone. So the fix comes with the assertion that would
    /// have failed before it, which is the only way to know a fix did anything.
    #[test]
    fn the_avc1_fixture_yields_real_codec_parameters() {
        let track = simple_track(1, 1, 4, 1024);
        let demux = open(fixture(1000, 0, &[track], &[0u8; 4]));
        let s = &demux.streams()[0];
        assert_eq!(
            s.params.codec_id,
            Some(vaco_codec_core::CodecId::H264),
            "the sample entry did not parse; is `stsd` double-wrapped again?"
        );
        let v = s.params.video.as_ref().expect("a video sample entry");
        assert_eq!((v.width, v.height), (160, 120));
    }

    /// Common Encryption is reported, not decoded: `encryption_scheme` and
    /// `encryption_key_id` land on the stream, and reading a packet from it
    /// fails with a message naming the reason rather than handing back the
    /// still-encrypted bytes.
    #[test]
    fn cenc_is_reported_and_reading_it_is_refused() {
        let kid = {
            let mut k = [0u8; 16];
            k[15] = 1;
            k
        };
        let mut track = simple_track(1, 2, 4, 1024);
        track.stbl.stsd_box = Some(common::encv_stsd(kid));
        let mut demux = open(fixture(1000, 0, &[track], &[0u8; 8]));
        assert_eq!(demux.streams().len(), 1);
        assert_eq!(
            demux.streams()[0]
                .metadata
                .iter()
                .find(|(k, _)| k == "encryption_scheme")
                .map(|(_, v)| v.as_str()),
            Some("cenc")
        );
        assert_eq!(
            demux.streams()[0]
                .metadata
                .iter()
                .find(|(k, _)| k == "encryption_key_id")
                .map(|(_, v)| v.as_str()),
            Some("00000000000000000000000000000001")
        );
        let err = demux.read_packet().unwrap_err();
        let text = err.to_string();
        assert!(
            text.to_lowercase().contains("encrypt")
                || text.to_lowercase().contains("cenc")
                || text.to_lowercase().contains("decrypt"),
            "{text}"
        );
    }

    /// A `colr ▸ nclx` extension box, byte-for-byte the shape read back from a
    /// real `ffmpeg 8.1 -movflags write_colr -colorspace bt709` file (see
    /// `vaco_format_isom::stsd`'s own `colr_matches_a_real_ffmpeg_nclx_atom`),
    /// maps onto `VideoParameters::color`.
    #[test]
    fn a_colr_box_sets_the_video_color_info() {
        use vaco_color::{ColorPrimaries, ColorRange, MatrixCoefficients, TransferCharacteristic};

        let colr =
            vaco_format_isom::build::bx(b"colr", &[b'n', b'c', b'l', b'x', 0, 1, 0, 1, 0, 1, 0x80]);
        let mut track = simple_track(1, 1, 4, 1024);
        track.stbl.stsd_box = Some(common::avc1_stsd_with_extension(&colr));
        let demux = open(fixture(1000, 0, &[track], &[0u8; 4]));
        let color = demux.streams()[0].params.video.as_ref().unwrap().color;
        assert_eq!(color.primaries, ColorPrimaries::Bt709);
        assert_eq!(color.transfer, TransferCharacteristic::Bt709);
        assert_eq!(color.matrix, MatrixCoefficients::Bt709);
        assert_eq!(color.range, ColorRange::Full);
    }

    /// A `tmcd` track's single sample becomes a `timecode` tag on its own
    /// stream *and* on the video track whose `tref ▸ tmcd` names it —
    /// measured against a real `ffmpeg -timecode 01:00:00:00` `.mov`, where
    /// `ffprobe` prints the same `TAG:timecode` on both streams.
    #[test]
    fn a_tmcd_track_tags_itself_and_its_referencing_track() {
        use vaco_format_isom::FourCc;
        use vaco_format_isom::writer;

        let mut tmcd_entry = vec![0u8; 6]; // reserved
        tmcd_entry.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
        tmcd_entry.extend_from_slice(&0u32.to_be_bytes()); // reserved
        tmcd_entry.extend_from_slice(&0u32.to_be_bytes()); // flags: non-drop
        tmcd_entry.extend_from_slice(&2_500u32.to_be_bytes()); // time_scale
        tmcd_entry.extend_from_slice(&100u32.to_be_bytes()); // frame_duration
        tmcd_entry.push(25); // number_of_frames
        tmcd_entry.push(0); // reserved
        let mut stsd_body = 1u32.to_be_bytes().to_vec();
        stsd_body.extend_from_slice(&vaco_format_isom::build::bx(b"tmcd", &tmcd_entry));
        let tmcd_stsd = vaco_format_isom::build::fullbx(b"stsd", 0, 0, &stsd_body);

        let mut video = simple_track(1, 1, 4, 1024);
        video.tref = writer::tref(&writer::tref_entry(FourCc::new(b"tmcd"), &[2]));

        // One sample: a big-endian frame count of 90 000 at 25 fps = one hour.
        let sample = 90_000u32.to_be_bytes().to_vec();
        let tmcd_track = TrackSpec {
            track_id: 2,
            handler: *b"tmcd",
            timescale: 2_500,
            media_duration: 100,
            stbl: StblSpec {
                stsd_box: Some(tmcd_stsd),
                stts: vec![(1, 100)],
                stsc: vec![(1, 1, 1)],
                stsz: vec![sample.len() as u32],
                stco: vec![MDAT_PAYLOAD as u32 + 4],
                has_stss: false,
                ..StblSpec::default()
            },
            ..TrackSpec::default()
        };

        let mut media = vec![0u8; 4]; // the video sample
        media.extend_from_slice(&sample);

        let demux = open(fixture(1000, 0, &[video, tmcd_track], &media));
        assert_eq!(demux.streams().len(), 2);
        for s in demux.streams() {
            assert_eq!(
                s.metadata
                    .iter()
                    .find(|(k, _)| k == "timecode")
                    .map(|(_, v)| v.as_str()),
                Some("01:00:00:00"),
                "stream {} (id {:?}) missing the timecode tag",
                s.index,
                s.id
            );
        }
    }

    /// Real bytes measured from `ffmpeg -f lavfi -i "sine=frequency=440:
    /// duration=0.2" -ac 1 -c:a alac tiny.m4a`'s own `stsd` box: one audio
    /// sample entry (fourcc `alac`) wrapping a real `alac` full box --
    /// 4 bytes of version+flags, then the 24-byte `ALACSpecificConfig`
    /// itself (`frame_length = 4096` at that record's own first 4 bytes).
    ///
    /// Regression for a real, measured bug: `codec_parameters` used to hand
    /// over the `alac` box's full, un-stripped 28 bytes as extradata --
    /// version+flags included, since `CodecConfig::data` keeps them for a
    /// full box "because they are part of the record" (true in general, but
    /// `vaco-codec-alac`'s own `AlacCookie::parse` expects the bare
    /// record). That shifted `frame_length` onto the version+flags' `0`,
    /// and every packet whose `partialFrame` bit relied on the cookie's
    /// `frame_length` (every full-length packet in a real file) decoded as
    /// a valid-looking, silently empty frame -- `vaco` decoded about 2.5%
    /// of a real 22-frame `ffmpeg`-produced `.m4a` and exited 0.
    #[test]
    fn alac_stsd_extradata_is_the_bare_config_not_the_full_box_header() {
        let real_alac_stsd = {
            fn from_hex(s: &str) -> Vec<u8> {
                (0..s.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
                    .collect()
            }
            // The bytes captured this way are the `stsd` box's own payload
            // (version+flags, entry_count, then the entries) -- everything
            // after its 8-byte box header, which `StblSpec::stsd_box`
            // expects to be present, so it is added back here.
            let payload = from_hex(
                "000000000000000100000048616c6163000000000000000100000000000000000001001000000000ac44000000000024616c616300000000000010000010280a0e01000000002004000ac4400000ac44",
            );
            let mut boxed = u32::try_from(payload.len() + 8)
                .unwrap()
                .to_be_bytes()
                .to_vec();
            boxed.extend_from_slice(b"stsd");
            boxed.extend_from_slice(&payload);
            boxed
        };
        let track = TrackSpec {
            handler: *b"soun",
            timescale: 44100,
            media_duration: 4096,
            stbl: StblSpec {
                stsd_box: Some(real_alac_stsd),
                stts: vec![(1, 4096)],
                stsc: vec![(1, 1, 1)],
                stsz: vec![100],
                stco: vec![u32::try_from(MDAT_PAYLOAD).unwrap()],
                has_stss: false,
                ..StblSpec::default()
            },
            ..TrackSpec::default()
        };
        let data = fixture(44100, 4096, &[track], &[0u8; 100]);
        let demux = open(data);
        let stream = demux.streams().first().expect("one stream");
        let extradata = stream
            .params
            .extradata
            .as_ref()
            .expect("alac extradata must be present");
        assert_eq!(
            extradata.len(),
            24,
            "must be the bare 24-byte ALACSpecificConfig, not the 28-byte full-box-prefixed record: {extradata:02x?}"
        );
        assert_eq!(
            &extradata[0..4],
            &4096u32.to_be_bytes(),
            "frame_length must read as 4096, not corrupted to 0 by an un-stripped version+flags prefix: {extradata:02x?}"
        );
    }

    /// A real `stsd` box captured from `ffmpeg -c:a flac -f mp4` (one
    /// `fLaC` sample entry: `channelcount=2`, `samplesize=16`,
    /// `samplerate=48000`, a `dfLa` full box whose `STREAMINFO` states
    /// `sample_rate=48000`, `channels=2`, `bits_per_sample=16`, plus a
    /// `btrt`). Regression for a real, measured bug: `codec_parameters`
    /// used to hand `vaco-parse-audio-misc::flac::FlacParser` `dfLa`'s own
    /// un-converted 42-byte full-box payload, which that parser's naive
    /// fixed-offset read misreads as `channels=1`, `bits_per_raw_sample=1`.
    #[test]
    fn flac_stsd_extradata_is_flac_prefixed_not_the_full_box_header() {
        fn from_hex(s: &str) -> Vec<u8> {
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
                .collect()
        }
        let real_flac_entry = from_hex(
            "0000006a664c6143000000000000000100000000000000000002001000000000bb8000000000003264664c6100000000800000221000100000034f00048e0bb802f00000bb809e36f85f3d9494c75a2ac524efaf9ebd0000001462747274000000000001f4000001a840",
        );
        let payload = {
            let mut p = vec![0u8, 0, 0, 0]; // stsd version+flags
            p.extend_from_slice(&1u32.to_be_bytes()); // entry_count
            p.extend_from_slice(&real_flac_entry);
            p
        };
        let mut real_flac_stsd = u32::try_from(payload.len() + 8)
            .unwrap()
            .to_be_bytes()
            .to_vec();
        real_flac_stsd.extend_from_slice(b"stsd");
        real_flac_stsd.extend_from_slice(&payload);

        let track = TrackSpec {
            handler: *b"soun",
            timescale: 48000,
            media_duration: 48000,
            stbl: StblSpec {
                stsd_box: Some(real_flac_stsd),
                stts: vec![(1, 48000)],
                stsc: vec![(1, 1, 1)],
                stsz: vec![100],
                stco: vec![u32::try_from(MDAT_PAYLOAD).unwrap()],
                has_stss: false,
                ..StblSpec::default()
            },
            ..TrackSpec::default()
        };
        let data = fixture(48000, 48000, &[track], &[0u8; 100]);
        let demux = open(data);
        let stream = demux.streams().first().expect("one stream");
        let extradata = stream
            .params
            .extradata
            .as_ref()
            .expect("flac extradata must be present");
        assert!(
            extradata.starts_with(b"fLaC"),
            "must carry this project's canonical fLaC-prefixed shape, not dfLa's own full-box header: {extradata:02x?}"
        );
        // `vaco-parse-audio-misc::flac::tests::a_flac_prefixed_metadata_block_
        // describes_the_stream_correctly` covers that this exact shape --
        // "fLaC" then a metadata-block header then STREAMINFO -- is read
        // back as channels=2/bits_per_raw_sample=16, not the channels=1/
        // bits_per_raw_sample=1 a real `ParserProvider` used to report over
        // dfLa's un-converted full-box payload. Not re-asserted here:
        // `vaco-demux-mp4` cannot depend on a `vaco-parse-*` crate (D14.1).
    }

    /// A trailing zero-size sample -- the standard "clear the subtitle"
    /// entry many `mov_text` writers (including real ffmpeg's own) append
    /// after the last real cue, carrying no payload and no reason to be
    /// handed to a caller as a packet -- is not one.
    ///
    /// Regression for a real, measured bug: real ffmpeg 9.0.1's own MP4
    /// demuxer never surfaces a sample this shaped at all (`ffprobe
    /// -show_packets` on a real `-c:s mov_text` file reports only the two
    /// real cues), but this crate's own `next_packet` handed every `stsz`
    /// entry straight to the caller regardless of size, so `vaco-probe`
    /// reported a third, phantom zero-duration/zero-length packet a real
    /// player never sees.
    #[test]
    fn a_zero_size_trailing_sample_is_never_handed_to_the_caller() {
        let sample1 = {
            let mut s = 11u16.to_be_bytes().to_vec();
            s.extend_from_slice(b"Hello world");
            s
        };
        let sample2 = {
            let mut s = 11u16.to_be_bytes().to_vec();
            s.extend_from_slice(b"Second line");
            s
        };
        // The trailing sample is *not* zero-size: `00 00` is mov_text's
        // own big-endian `u16` zero-length-string encoding, a real 2-byte
        // sample -- the same shape measured on a real ffmpeg-produced file.
        // A fix keyed on size instead of duration would miss this entirely.
        let trailing = [0x00u8, 0x00];
        let mut media = sample1.clone();
        media.extend_from_slice(&sample2);
        media.extend_from_slice(&trailing);

        let track = TrackSpec {
            handler: *b"text",
            timescale: 1000,
            media_duration: 2000,
            stbl: StblSpec {
                stts: vec![(1, 1000), (1, 1000), (1, 0)],
                stsc: vec![(1, 3, 1)],
                stsz: vec![
                    sample1.len() as u32,
                    sample2.len() as u32,
                    trailing.len() as u32,
                ],
                stco: vec![u32::try_from(MDAT_PAYLOAD).unwrap()],
                has_stss: false,
                ..StblSpec::default()
            },
            ..TrackSpec::default()
        };
        let data = fixture(1000, 2000, &[track], &media);
        let mut demux = open(data);
        let packets = drain(&mut demux);
        assert_eq!(
            packets.len(),
            2,
            "the trailing zero-size sample must not become a third packet: {packets:?}"
        );
        assert_eq!(packets[0].4, sample1.len());
        assert_eq!(packets[1].4, sample2.len());
    }

    /// The other half of the rule above: the skip is for `mov_text`'s
    /// trailing cue-clear sample, **not** for any sample whose `stts` delta
    /// happens to be zero.
    ///
    /// Measured on a 20-sample video track whose final `stts` run is
    /// `(1, 0)` — the shape this repository's own MP4 muxer wrote for the
    /// last sample of every progressive file until the same commit as this
    /// test: `ffprobe -count_packets` on the reference reports all 20, and a
    /// duration-only skip reported 19, silently deleting the last frame of
    /// every such file. Twelve `vaco-mux-mp4` round-trip tests and one
    /// `vaco-mux-dash` one had never passed because of it.
    #[test]
    fn a_trailing_zero_duration_video_sample_is_still_a_packet() {
        const SAMPLE_SIZE: u32 = 4;
        const COUNT: u32 = 6;
        let media = vec![0xAB; (COUNT * SAMPLE_SIZE) as usize];
        let track = TrackSpec {
            handler: *b"vide",
            timescale: 30,
            media_duration: u64::from((COUNT - 1) * 100),
            stbl: StblSpec {
                stts: vec![(COUNT - 1, 100), (1, 0)],
                stsc: vec![(1, COUNT, 1)],
                stsz: vec![SAMPLE_SIZE; COUNT as usize],
                stco: vec![u32::try_from(MDAT_PAYLOAD).unwrap()],
                has_stss: false,
                ..StblSpec::default()
            },
            ..TrackSpec::default()
        };
        let data = fixture(1000, (COUNT - 1) * 100, &[track], &media);
        let mut demux = open(data);
        let packets = drain(&mut demux);
        assert_eq!(
            packets.len(),
            COUNT as usize,
            "a zero final stts delta must not delete the last frame: {packets:?}"
        );
        assert_eq!(packets[COUNT as usize - 1].2, i64::from((COUNT - 1) * 100));
    }
}
