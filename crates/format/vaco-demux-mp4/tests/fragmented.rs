//! `sidx`/`mfra` wiring: the trailer is read, the boxes are exposed, and
//! seeking uses `mfra` as a fast path with the same landing rule the
//! fragment-scan fallback already guaranteed.
//!
//! Fixtures are built box by box in [`common`] rather than shelled out to
//! `ffmpeg`, matching the rest of this crate's tests: a committed test must
//! not depend on a binary that may not be on the machine that runs it.

mod common;

use common::{frag_file, frag_moov, frag_unit, mfra, sidx};
use vaco_core::{Error, Timestamp};
use vaco_demux_mp4::{Mp4Demuxer, Mp4Options};
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::{Demuxer, FormatOptions, SeekFlags, SeekTarget};
use vaco_io::{MediaSource, MemorySource};

#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]
mod tests {
    use super::*;

    const TRACK_ID: u32 = 1;
    const HANDLER: [u8; 4] = *b"vide";

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

    /// `n` fragments of `samples_per_fragment` fixed-size samples each, one
    /// second apart (the track timescale `frag_moov` gives every track is
    /// 1000, and each fragment's `tfdt` is `i * samples_per_fragment * 1000`).
    /// Returns the file bytes and, for building `mfra` by hand in a test that
    /// wants to check it against a wrong one, the `(tfdt, moof_offset)` of
    /// every fragment.
    fn build(n: u32, samples_per_fragment: u32, with_mfra: bool) -> (Vec<u8>, Vec<(u64, u64)>) {
        let moov = frag_moov(1000, &[(TRACK_ID, HANDLER)]);
        let mut units = Vec::new();
        let mut offsets = Vec::new();
        // `ftyp` + `moov`, exactly as `frag_file` will place them, so offsets
        // computed here match where each unit actually lands.
        let header_len = {
            let probe = frag_file(&moov, &[], None);
            probe.len() as u64
        };
        let mut pos = header_len;
        let sizes = vec![100u32; samples_per_fragment as usize];
        for i in 0..n {
            let tfdt = u64::from(i) * u64::from(samples_per_fragment) * 1000;
            offsets.push((tfdt, pos));
            let unit = frag_unit(i + 1, TRACK_ID, tfdt, &sizes);
            pos += unit.len() as u64;
            units.push(unit);
        }
        let trailer = with_mfra.then(|| {
            let rows: Vec<(u64, u64, u32, u32, u32)> =
                offsets.iter().map(|&(t, o)| (t, o, 1, 1, 1)).collect();
            mfra(&[(TRACK_ID, &rows)])
        });
        (frag_file(&moov, &units, trailer), offsets)
    }

    #[test]
    fn a_fragmented_file_without_mfra_opens_and_reports_no_fast_path() {
        let (data, _) = build(3, 2, false);
        let demux = open(data);
        assert!(demux.is_fragmented());
        assert!(demux.fragment_random_access().is_empty());
    }

    #[test]
    fn mfra_is_read_from_the_trailer() {
        let (data, offsets) = build(4, 3, true);
        let demux = open(data);
        assert!(demux.is_fragmented());
        let tables = demux.fragment_random_access();
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].track_id, TRACK_ID);
        assert_eq!(tables[0].entries.len(), offsets.len());
        for (entry, &(tfdt, offset)) in tables[0].entries.iter().zip(&offsets) {
            assert_eq!(entry.time, tfdt);
            assert_eq!(entry.moof_offset, offset);
        }
    }

    #[test]
    fn a_truncated_trailer_yields_no_fast_path_not_an_error() {
        let (mut data, _) = build(2, 2, true);
        // Cut the file off mid-`mfra`: `mfro` no longer sits in the last 16
        // bytes, so the trailer reader must decline cleanly.
        data.truncate(data.len() - 20);
        let demux = open(data);
        assert!(demux.is_fragmented());
        assert!(demux.fragment_random_access().is_empty());
        // ... and reading still works — the fast path is optional.
        let s = demux.streams();
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn sidx_boxes_between_moov_and_the_first_moof_are_collected() {
        let moov = frag_moov(1000, &[(TRACK_ID, HANDLER)]);
        let one_sidx = sidx(TRACK_ID, 1000, 0, 0, &[(false, 500, 3000, true, 1, 0)]);
        let unit = frag_unit(1, TRACK_ID, 0, &[100, 100, 100]);
        let mut units = vec![one_sidx];
        units.push(unit);
        let data = frag_file(&moov, &units, None);
        let demux = open(data);
        let boxes = demux.segment_index();
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].reference_id, TRACK_ID);
        assert_eq!(boxes[0].timescale, 1000);
        assert_eq!(boxes[0].references.len(), 1);
        assert_eq!(boxes[0].references[0].referenced_size, 500);
        assert_eq!(boxes[0].references[0].subsegment_duration, 3000);
    }

    /// A version-1 top-level `pssh` beside `moof` — ISO/IEC 23001-7 §8.1's
    /// fragmented-file location — reports both its DRM system and every KID
    /// in declaration order. The same metadata shape is used for a
    /// progressive file's `moov`-level copy.
    #[test]
    fn a_top_level_pssh_beside_moof_reports_its_system_and_kids() {
        let moov = frag_moov(1000, &[(TRACK_ID, HANDLER)]);
        let mut pssh_body = Vec::new();
        pssh_body.extend_from_slice(&[0x42; 16]); // system_id
        pssh_body.extend_from_slice(&2u32.to_be_bytes()); // KID_count
        pssh_body.extend_from_slice(&[0xAA; 16]);
        pssh_body.extend_from_slice(&[0xBB; 16]);
        pssh_body.extend_from_slice(&0u32.to_be_bytes()); // data_size = 0
        let pssh = vaco_format_isom::build::fullbx(b"pssh", 1, 0, &pssh_body);
        let unit = frag_unit(1, TRACK_ID, 0, &[100, 100, 100]);
        let data = frag_file(&moov, &[pssh, unit], None);
        let demux = open(data);
        let encryption: Vec<_> = demux
            .metadata()
            .iter()
            .filter(|(key, _)| key.starts_with("encryption_"))
            .cloned()
            .collect();
        assert_eq!(
            encryption,
            vec![
                (
                    "encryption_system_id".to_owned(),
                    "42424242424242424242424242424242".to_owned(),
                ),
                (
                    "encryption_key_id".to_owned(),
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                ),
                (
                    "encryption_key_id".to_owned(),
                    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
                ),
            ]
        );
    }

    #[test]
    fn seeking_a_fragmented_file_lands_on_the_right_fragment_with_mfra_present() {
        // 5 fragments of 4 samples each, 1000 ticks/sample: fragment `i`
        // covers media time `[i*4000, (i+1)*4000)`.
        let (data, _) = build(5, 4, true);
        let mut demux = open(data);
        assert_eq!(demux.fragment_random_access()[0].entries.len(), 5);

        // A backward seek into the middle of fragment 3 must land on that
        // fragment's first (and only sync) sample, at 3*4000.
        demux
            .seek(
                SeekTarget::Timestamp {
                    stream_index: 0,
                    ts: Timestamp::new(3 * 4000 + 1500),
                },
                SeekFlags::BACKWARD,
            )
            .unwrap();
        let first = demux.read_packet().unwrap();
        assert_eq!(first.dts.ticks(), Some(3 * 4000));

        // Seeking to the very first sample of the file must still work.
        demux
            .seek(
                SeekTarget::Timestamp {
                    stream_index: 0,
                    ts: Timestamp::ZERO,
                },
                SeekFlags::BACKWARD,
            )
            .unwrap();
        let first = demux.read_packet().unwrap();
        assert_eq!(first.dts.ticks(), Some(0));

        // Seeking past the end lands on the last fragment's sync sample.
        demux
            .seek(
                SeekTarget::Timestamp {
                    stream_index: 0,
                    ts: Timestamp::new(1_000_000),
                },
                SeekFlags::BACKWARD,
            )
            .unwrap();
        let first = demux.read_packet().unwrap();
        assert_eq!(first.dts.ticks(), Some(4 * 4000));
    }

    #[test]
    fn seeking_agrees_whether_or_not_mfra_is_present() {
        // The fast path and the fragment-scan fallback must agree on every
        // landing — `mfra` changes which code path answers, never the
        // answer. Built as two demuxers from the same fragment layout, one
        // with a trailer and one without, seeking every one to a grid of
        // targets spanning the whole file.
        let (with_trailer, offsets) = build(6, 3, true);
        let (without_trailer, offsets_check) = build(6, 3, false);
        assert_eq!(offsets, offsets_check, "same fragment layout either way");
        let mut a = open(with_trailer);
        let mut b = open(without_trailer);
        assert!(!a.fragment_random_access().is_empty());
        assert!(b.fragment_random_access().is_empty());

        for target in [0i64, 500, 1499, 3000, 3001, 8999, 9000, 17999, 100_000] {
            for demux in [&mut a, &mut b] {
                demux
                    .seek(
                        SeekTarget::Timestamp {
                            stream_index: 0,
                            ts: Timestamp::new(target),
                        },
                        SeekFlags::BACKWARD,
                    )
                    .unwrap();
            }
            let landed_a = a.read_packet().unwrap().dts.ticks();
            let landed_b = b.read_packet().unwrap().dts.ticks();
            assert_eq!(landed_a, landed_b, "target {target}");
        }
    }

    #[test]
    fn a_non_seekable_source_gets_no_fast_path_but_still_demuxes() {
        let (data, _) = build(2, 2, true);
        let src: Box<dyn MediaSource> = Box::new(common::ChunkSource::new(data, 4096, false));
        let mut demux = Mp4Demuxer::open(
            src,
            &NoParsers,
            &FormatOptions::default(),
            Mp4Options::default(),
        )
        .unwrap();
        assert!(demux.fragment_random_access().is_empty());
        let mut count = 0;
        loop {
            match demux.read_packet() {
                Ok(_) => count += 1,
                Err(Error::Eof) => break,
                Err(e) => panic!("read failed: {e}"),
            }
        }
        assert_eq!(count, 4);
    }

    proptest::proptest! {
        /// The property `seeking_agrees_whether_or_not_mfra_is_present` checks
        /// by hand, generalised: for *any* fragment layout and *any* seek
        /// target, a demuxer with `mfra` and one without must land on the
        /// same sample. `mfra` is only ever allowed to change how fast the
        /// answer arrives.
        #[test]
        fn fast_path_and_fallback_always_agree(
            n in 1u32..10,
            samples_per_fragment in 1u32..6,
            target in -2_000i64..40_000,
        ) {
            let (with_trailer, offsets) = build(n, samples_per_fragment, true);
            let (without_trailer, offsets_check) = build(n, samples_per_fragment, false);
            proptest::prop_assert_eq!(&offsets, &offsets_check);
            let mut a = open(with_trailer);
            let mut b = open(without_trailer);
            proptest::prop_assert!(!a.fragment_random_access().is_empty());
            proptest::prop_assert!(b.fragment_random_access().is_empty());
            for demux in [&mut a, &mut b] {
                demux
                    .seek(
                        SeekTarget::Timestamp { stream_index: 0, ts: Timestamp::new(target) },
                        SeekFlags::BACKWARD,
                    )
                    .unwrap();
            }
            let landed_a = a.read_packet().ok().and_then(|p| p.dts.ticks());
            let landed_b = b.read_packet().ok().and_then(|p| p.dts.ticks());
            proptest::prop_assert_eq!(landed_a, landed_b);
        }
    }
}
