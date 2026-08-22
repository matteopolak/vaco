//! Property tests: the invariants that hold for *every* input, not just the
//! ones somebody thought to write down.
//!
//! Three properties, chosen because each covers a bug class the unit tests
//! cannot: VINT round-tripping (the encoding is the whole format), termination
//! on arbitrary bytes (the denial-of-service surface), and stability under
//! single-byte corruption (what a damaged download actually looks like).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]

use proptest::prelude::*;
use vaco_demux_matroska::MatroskaDemuxer;
use vaco_demux_matroska::ebml::{self, schema as el};
use vaco_demux_matroska::synth::{self, SegmentSize};
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_io::{MediaSource, MemorySource};

/// Read to the end, capped so a non-terminating parse fails rather than hangs.
fn drain_capped(bytes: Vec<u8>) -> usize {
    let src: Box<dyn MediaSource> = Box::new(MemorySource::new(bytes));
    let Ok(mut d) = MatroskaDemuxer::open(src, &NoParsers, &FormatOptions::default()) else {
        return 0;
    };
    for n in 0..50_000usize {
        if d.read_packet().is_err() {
            return n;
        }
    }
    panic!("read_packet did not terminate");
}

fn valid_file() -> Vec<u8> {
    let mut audio = synth::float(el::SAMPLINGFREQUENCY, 48000.0);
    audio.extend_from_slice(&synth::uint(el::CHANNELS, 2));
    let mut body = synth::uint(el::TRACKNUMBER, 1);
    body.extend_from_slice(&synth::uint(el::TRACKUID, 1));
    body.extend_from_slice(&synth::uint(el::TRACKTYPE, 2));
    body.extend_from_slice(&synth::string(el::CODECID, "A_PCM/INT/LIT"));
    body.extend_from_slice(&synth::uint(el::FLAGLACING, 1));
    body.extend_from_slice(&synth::uint(el::DEFAULTDURATION, 20_000_000));
    body.extend_from_slice(&synth::element(el::AUDIO, &audio));
    let track = synth::element(el::TRACKENTRY, &body);
    let clusters: Vec<_> = (0..3u64)
        .map(|i| {
            synth::cluster(
                i * 100,
                &[synth::element(
                    el::SIMPLEBLOCK,
                    &synth::block_body(1, 0, 0x80, &[i as u8; 48]),
                )],
                SegmentSize::Known,
            )
        })
        .collect();
    synth::file(
        "matroska",
        &synth::uint(el::TIMESTAMPSCALE, 1_000_000),
        &track,
        &clusters,
        SegmentSize::Known,
    )
}

proptest! {
    /// Every value that fits an eight-octet VINT survives a round trip, and the
    /// shortest encoding never accidentally lands on the unknown-size marker.
    #[test]
    fn data_sizes_round_trip(v in 0u64..(1u64 << 56) - 2) {
        let bytes = synth::vint_min(v);
        let (size, used) = ebml::read_size(&bytes, 8).unwrap();
        prop_assert_eq!(size, ebml::Size::Known(v));
        prop_assert_eq!(used, bytes.len());
    }

    /// RFC 9559 section 10.3.3's signed lace VINT, over its whole range.
    #[test]
    fn lace_size_deltas_round_trip(v in -(1i64 << 34)..(1i64 << 34)) {
        let bytes = synth::signed_vint(v);
        let (got, used) = ebml::read_signed_vint(&bytes).unwrap();
        prop_assert_eq!(got, v);
        prop_assert_eq!(used, bytes.len());
    }

    /// Arbitrary bytes behind a real EBML header: the parser must always stop,
    /// never panic, and never allocate its way out of the budget.
    #[test]
    fn arbitrary_bytes_after_a_header_terminate(tail in prop::collection::vec(any::<u8>(), 0..2048)) {
        let mut bytes = synth::ebml_header("matroska");
        bytes.extend_from_slice(&tail);
        let _ = drain_capped(bytes);
    }

    /// Wholly arbitrary bytes, most of which are rejected at the magic.
    #[test]
    fn arbitrary_bytes_terminate(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
        let _ = drain_capped(bytes);
    }

    /// One corrupted octet anywhere in a valid file: still terminates, still
    /// never panics. This is what a damaged download looks like.
    #[test]
    fn single_byte_corruption_never_panics(at in 0usize..600, to in any::<u8>()) {
        let mut bytes = valid_file();
        let at = at % bytes.len();
        bytes[at] = to;
        let _ = drain_capped(bytes);
    }

    /// Every truncation of a valid file is answerable.
    #[test]
    fn truncation_never_panics(keep in 0usize..600) {
        let bytes = valid_file();
        let keep = keep.min(bytes.len());
        let _ = drain_capped(bytes[..keep].to_vec());
    }

    /// A lace header may claim up to 256 frames; whatever it claims, the frames
    /// that come back lie inside the block and do not overlap.
    #[test]
    fn laced_frames_stay_inside_the_block(
        flags in prop::sample::select(vec![0x00u8, 0x02, 0x04, 0x06]),
        payload in prop::collection::vec(any::<u8>(), 0..512),
    ) {
        let data = synth::block_body(1, 0, 0x80 | flags, &payload);
        let Ok(header) = vaco_demux_matroska::block::parse_header(&data, true) else {
            return Ok(());
        };
        let Ok(frames) = vaco_demux_matroska::block::frames(&data, &header) else {
            return Ok(());
        };
        let mut prev_end = header.header_len;
        for f in &frames {
            prop_assert!(f.offset >= prev_end, "frames overlap or go backwards");
            let end = f.offset.checked_add(f.len).unwrap();
            prop_assert!(end <= data.len(), "frame runs past the block");
            prev_end = end;
        }
    }
}
