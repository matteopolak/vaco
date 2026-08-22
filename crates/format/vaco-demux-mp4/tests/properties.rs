//! Properties that must hold for every sample table, not just the ones a
//! muxer happens to write.
//!
//! The invariants are the ones a consumer leans on: packets are inside the
//! file, decode times do not go backwards within a stream, a chunk-fed source
//! is indistinguishable from a whole-file one, and a backward seek never lands
//! after its target.

mod common;

use common::{ChunkSource, MDAT_PAYLOAD, fixture, simple_track};
use proptest::prelude::*;
use vaco_core::{Error, Timestamp};
use vaco_demux_mp4::{Mp4Demuxer, Mp4Options};
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::{Demuxer, FormatOptions, SeekFlags, SeekTarget};
use vaco_format_isom::build::TrackSpec;
use vaco_io::{MediaSource, MemorySource};

#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::integer_division,
    clippy::cast_possible_truncation,
    reason = "test code"
)]
mod props {
    use super::*;

    #[derive(Debug, Clone)]
    struct Shape {
        sizes: Vec<u32>,
        stts: Vec<(u32, u32)>,
        per_chunk: u32,
        stss: Vec<u32>,
    }

    fn shape() -> impl Strategy<Value = Shape> {
        (
            prop::collection::vec(1u32..64, 1..24),
            prop::collection::vec((1u32..8, 1u32..1000), 1..4),
            1u32..5,
            prop::collection::vec(1u32..24, 0..6),
        )
            .prop_map(|(sizes, stts, per_chunk, mut stss)| {
                stss.sort_unstable();
                stss.dedup();
                Shape {
                    sizes,
                    stts,
                    per_chunk,
                    stss,
                }
            })
    }

    /// Lay the shape out as a real file, with chunks that actually exist.
    fn build(shape: &Shape) -> Vec<u8> {
        let n = shape.sizes.len() as u32;
        let media_len: u32 = shape.sizes.iter().copied().sum();
        let mut track = simple_track(1, n, 1, 512);
        track.stbl.stsz.clone_from(&shape.sizes);
        track.stbl.stts.clone_from(&shape.stts);
        track.stbl.stsc = vec![(1, shape.per_chunk, 1)];
        track.stbl.has_stss = !shape.stss.is_empty();
        track.stbl.stss = shape.stss.iter().copied().filter(|k| *k <= n).collect();
        // One chunk per `per_chunk` samples, laid end to end in the `mdat`.
        let mut offsets = Vec::new();
        let mut at = u32::try_from(MDAT_PAYLOAD).unwrap();
        for (i, size) in shape.sizes.iter().enumerate() {
            if (i as u32).is_multiple_of(shape.per_chunk) {
                offsets.push(at);
            }
            at += size;
        }
        track.stbl.stco = offsets;
        track.media_duration = shape
            .stts
            .iter()
            .map(|(c, d)| u64::from(*c) * u64::from(*d))
            .sum();
        fixture(1000, 0, &[track], &vec![0x11; media_len as usize])
    }

    fn open(data: Vec<u8>) -> Option<Mp4Demuxer> {
        let src: Box<dyn MediaSource> = Box::new(MemorySource::new(data));
        Mp4Demuxer::open(
            src,
            &NoParsers,
            &FormatOptions::default(),
            Mp4Options::default(),
        )
        .ok()
    }

    type Row = (u32, i64, u64, usize);

    fn drain(demux: &mut Mp4Demuxer) -> Vec<Row> {
        let mut out = Vec::new();
        for _ in 0..4096 {
            match demux.read_packet() {
                Ok(p) => out.push((
                    p.stream_index,
                    p.dts.ticks().unwrap_or(i64::MIN),
                    p.pos.unwrap_or(0),
                    p.payload().len(),
                )),
                Err(_) => break,
            }
        }
        out
    }

    proptest! {
        #[test]
        fn packets_lie_inside_the_file_and_never_go_backwards(shape in shape()) {
            let data = build(&shape);
            let total = data.len() as u64;
            let Some(mut demux) = open(data) else { return Ok(()) };
            let declared = demux.streams()[0].frame_count.unwrap_or(0);
            let packets = drain(&mut demux);
            prop_assert!(packets.len() as u64 <= declared);
            let mut last = i64::MIN;
            for (_, dts, pos, len) in &packets {
                prop_assert!(pos + *len as u64 <= total, "packet past the end");
                prop_assert!(*dts >= last, "dts went backwards");
                last = *dts;
            }
        }

        #[test]
        fn a_chunk_fed_source_is_indistinguishable(shape in shape(), chunk in 1usize..64) {
            let data = build(&shape);
            let Some(mut whole) = open(data.clone()) else { return Ok(()) };
            let expected = drain(&mut whole);
            let src: Box<dyn MediaSource> = Box::new(ChunkSource::new(data, chunk, true));
            let mut fed = Mp4Demuxer::open(
                src,
                &NoParsers,
                &FormatOptions::default(),
                Mp4Options::default(),
            )
            .unwrap();
            prop_assert_eq!(drain(&mut fed), expected);
        }

        #[test]
        fn a_backward_seek_never_lands_after_its_target(shape in shape(), at in 0i64..8000) {
            let data = build(&shape);
            let Some(mut demux) = open(data) else { return Ok(()) };
            if demux.streams().is_empty() {
                return Ok(());
            }
            demux
                .seek(
                    SeekTarget::Timestamp { stream_index: 0, ts: Timestamp::new(at) },
                    SeekFlags::BACKWARD,
                )
                .unwrap();
            match demux.read_packet() {
                Ok(p) => {
                    let dts = p.dts.ticks().unwrap_or(i64::MIN);
                    // The one exception is a track with no sync sample at all
                    // before the target, where the first sample is the best a
                    // backward seek can do.
                    prop_assert!(dts <= at || dts == demux.streams()[0].start_time.ticks().unwrap_or(0));
                }
                Err(Error::Eof) => {}
                Err(e) => prop_assert!(false, "{e}"),
            }
        }

        #[test]
        fn seeking_back_to_the_start_replays_the_file(shape in shape()) {
            let data = build(&shape);
            let Some(mut demux) = open(data) else { return Ok(()) };
            let first = drain(&mut demux);
            demux
                .seek(
                    SeekTarget::Timestamp { stream_index: 0, ts: Timestamp::new(i64::MIN / 2) },
                    SeekFlags::BACKWARD,
                )
                .unwrap();
            prop_assert_eq!(drain(&mut demux), first);
        }

        #[test]
        fn an_arbitrary_prefix_of_a_real_file_never_panics(cut in 0usize..900) {
            let data = build(&Shape {
                sizes: vec![4; 12],
                stts: vec![(12, 512)],
                per_chunk: 3,
                stss: vec![1, 7],
            });
            let mut short = data;
            short.truncate(cut);
            if let Some(mut demux) = open(short) {
                let _ = drain(&mut demux);
            }
        }

        #[test]
        fn a_track_with_no_samples_opens_and_ends(delta in 1u32..1000) {
            let track = TrackSpec {
                track_id: 1,
                stbl: vaco_format_isom::build::StblSpec {
                    stsd: Some(common::avc1_stsd()),
                    stts: vec![(0, delta)],
                    ..vaco_format_isom::build::StblSpec::default()
                },
                ..TrackSpec::default()
            };
            let Some(mut demux) = open(fixture(1000, 0, &[track], &[])) else { return Ok(()) };
            prop_assert!(drain(&mut demux).is_empty());
        }
    }
}
