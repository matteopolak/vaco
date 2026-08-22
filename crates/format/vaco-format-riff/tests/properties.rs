//! Property tests for the invariants the unit tests can only sample.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]

use proptest::prelude::*;
use vaco_format_riff::bitmapinfo::BitmapInfoHeader;
use vaco_format_riff::chunk::{Chunk, ChunkId, ChunkIter};
use vaco_format_riff::wave::WaveFormatEx;
use vaco_limits::{Budget, Limits};

/// Serialize one chunk the way a well-behaved writer would: header, payload,
/// pad byte on an odd length.
fn write_chunk(id: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&id);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    if payload.len() % 2 == 1 {
        out.push(0);
    }
    out
}

fn chunk_id() -> impl Strategy<Value = [u8; 4]> {
    // Printable ASCII, the way every real chunk id is, so the id round-trips
    // through `ChunkId`'s own `Display`.
    prop::collection::vec(b'A'..=b'Z', 4..=4).prop_map(|v| [v[0], v[1], v[2], v[3]])
}

fn chunk_payload() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..64)
}

proptest! {
    /// A sequence of well-formed chunks, concatenated, reads back as exactly
    /// that sequence: same ids, same payloads, in order, none marked
    /// truncated.
    #[test]
    fn well_formed_chunks_round_trip(
        chunks in prop::collection::vec((chunk_id(), chunk_payload()), 0..8)
    ) {
        let mut file = Vec::new();
        for (id, payload) in &chunks {
            file.extend_from_slice(&write_chunk(*id, payload));
        }
        let got: Vec<Chunk<'_>> = ChunkIter::new(&file, 0)
            .collect::<Result<Vec<_>, _>>()
            .expect("well-formed input never errors");
        prop_assert_eq!(got.len(), chunks.len());
        for (parsed, (id, payload)) in got.iter().zip(chunks.iter()) {
            prop_assert_eq!(parsed.id, ChunkId::new(id));
            prop_assert_eq!(parsed.payload, payload.as_slice());
            prop_assert!(!parsed.truncated);
        }
    }

    /// `ChunkIter` never panics over arbitrary bytes, and always terminates
    /// (the iterator is bounded by construction, but this is the property
    /// that actually matters to a caller: it will not hang).
    #[test]
    fn chunk_iter_never_panics_on_arbitrary_bytes(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        let mut count = 0u32;
        for item in ChunkIter::new(&bytes, 0) {
            let _ = item;
            count += 1;
            // A malformed chain yields at most one Err and then stops; a
            // well-formed chain is bounded by HEADER_LEN per chunk. Either
            // way this loop cannot run away on a 256-byte input.
            prop_assert!(count <= 256);
        }
    }

    /// A `WAVEFORMATEX` with an 18-byte-or-longer encoding round-trips every
    /// field, including `extra`, through parse.
    #[test]
    fn wave_format_ex_round_trips(
        format_tag in any::<u16>(),
        channels in any::<u16>(),
        samples_per_sec in any::<u32>(),
        avg_bytes_per_sec in any::<u32>(),
        block_align in any::<u16>(),
        bits_per_sample in any::<u16>(),
        extra in prop::collection::vec(any::<u8>(), 0..48),
    ) {
        let mut data = Vec::new();
        data.extend_from_slice(&format_tag.to_le_bytes());
        data.extend_from_slice(&channels.to_le_bytes());
        data.extend_from_slice(&samples_per_sec.to_le_bytes());
        data.extend_from_slice(&avg_bytes_per_sec.to_le_bytes());
        data.extend_from_slice(&block_align.to_le_bytes());
        data.extend_from_slice(&bits_per_sample.to_le_bytes());
        data.extend_from_slice(&(extra.len() as u16).to_le_bytes());
        data.extend_from_slice(&extra);

        let mut budget = Budget::new(Limits::permissive());
        let fmt = WaveFormatEx::parse(&data, &mut budget).expect("well-formed input never errors");
        prop_assert_eq!(fmt.format_tag, format_tag);
        prop_assert_eq!(fmt.channels, channels);
        prop_assert_eq!(fmt.samples_per_sec, samples_per_sec);
        prop_assert_eq!(fmt.avg_bytes_per_sec, avg_bytes_per_sec);
        prop_assert_eq!(fmt.block_align, block_align);
        prop_assert_eq!(fmt.bits_per_sample, bits_per_sample);
        prop_assert_eq!(fmt.extra, extra);
    }

    /// `WaveFormatEx::parse` never panics on arbitrary bytes of any length,
    /// with a budget small enough that some inputs must be rejected.
    #[test]
    fn wave_format_ex_never_panics_on_arbitrary_bytes(bytes in prop::collection::vec(any::<u8>(), 0..128)) {
        let mut budget = Budget::new(Limits::strict());
        let _ = WaveFormatEx::parse(&bytes, &mut budget);
    }

    /// `BitmapInfoHeader` round-trips every field through parse.
    #[test]
    fn bitmap_info_header_round_trips(
        size in any::<u32>(),
        width in any::<i32>(),
        height in any::<i32>(),
        planes in any::<u16>(),
        bit_count in any::<u16>(),
        compression_raw in any::<u32>(),
        size_image in any::<u32>(),
        x_pels in any::<i32>(),
        y_pels in any::<i32>(),
        clr_used in any::<u32>(),
        clr_important in any::<u32>(),
    ) {
        let mut data = Vec::new();
        data.extend_from_slice(&size.to_le_bytes());
        data.extend_from_slice(&width.to_le_bytes());
        data.extend_from_slice(&height.to_le_bytes());
        data.extend_from_slice(&planes.to_le_bytes());
        data.extend_from_slice(&bit_count.to_le_bytes());
        data.extend_from_slice(&compression_raw.to_le_bytes());
        data.extend_from_slice(&size_image.to_le_bytes());
        data.extend_from_slice(&x_pels.to_le_bytes());
        data.extend_from_slice(&y_pels.to_le_bytes());
        data.extend_from_slice(&clr_used.to_le_bytes());
        data.extend_from_slice(&clr_important.to_le_bytes());

        let h = BitmapInfoHeader::parse(&data).expect("exactly forty bytes never errors");
        prop_assert_eq!(h.size, size);
        prop_assert_eq!(h.width, width);
        prop_assert_eq!(h.height, height);
        prop_assert_eq!(h.planes, planes);
        prop_assert_eq!(h.bit_count, bit_count);
        prop_assert_eq!(h.compression_raw, compression_raw);
        prop_assert_eq!(h.size_image, size_image);
        prop_assert_eq!(h.x_pels_per_meter, x_pels);
        prop_assert_eq!(h.y_pels_per_meter, y_pels);
        prop_assert_eq!(h.clr_used, clr_used);
        prop_assert_eq!(h.clr_important, clr_important);
    }

    /// `BitmapInfoHeader::parse` never panics on arbitrary bytes.
    #[test]
    fn bitmap_info_header_never_panics_on_arbitrary_bytes(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
        let _ = BitmapInfoHeader::parse(&bytes);
    }
}
