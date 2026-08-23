//! Fixture-based invariant (CONFORMANCE-FINDINGS 15): every Level-1 element
//! in a real, `ffmpeg`-written Matroska file opens with a `CRC-32` element
//! that validates.
//!
//! `tests/fixtures/ffmpeg_reference.mkv` is `ffmpeg 8.1`'s own output —
//! `ffmpeg -bitexact -f lavfi -i testsrc=size=64x64:rate=25:d=1 -pix_fmt
//! yuv420p -c:v libx264 m.mkv` — not anything this crate produced. That
//! matters: checking this crate's CRC against this crate's own writer would
//! only prove the encode and decode ends of one bug agree with each other
//! (`planning/AGENT-CONSTRAINTS.md`'s note on an oracle sharing your own
//! misreading). Checking it against an independently produced file, across
//! every Level-1 element the file has rather than just the one this crate
//! was developed against, is what actually falsifies a wrong polynomial,
//! seed or byte order.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code"
)]

use vaco_format_ebml::{Caps, MAX_ID_LEN, MAX_SIZE_LEN, Slice, read_id, read_size};

const FIXTURE: &[u8] = include_bytes!("fixtures/ffmpeg_reference.mkv");
const CRC32_ID: u32 = 0xBF;
const SEGMENT_ID: u32 = 0x1853_8067;

/// Every Level-1 element that opens with a `CRC-32` (all but `Void`, which
/// carries none) validates it against [`vaco_hash::crc32`] computed over the
/// remainder of that element's own body.
#[test]
fn every_level1_crc32_validates_against_a_reference_written_file() {
    let caps = Caps::default();
    let top: Vec<_> = Slice::new(FIXTURE, caps).children().collect();
    let segment = top
        .iter()
        .find(|c| c.id == SEGMENT_ID)
        .expect("fixture has a Segment element");

    let mut checked = Vec::new();
    for child in Slice::new(segment.data, caps).children() {
        let Ok((first_id, idl)) = read_id(child.data, MAX_ID_LEN) else {
            continue;
        };
        if first_id != CRC32_ID {
            continue; // Void, which carries no CRC-32 child.
        }
        let Ok((size, szl)) = read_size(&child.data[idl..], MAX_SIZE_LEN) else {
            continue;
        };
        let Some(crc_len) = size.known() else {
            continue;
        };
        let crc_start = idl + szl;
        let crc_end = crc_start + crc_len as usize;
        let Some(crc_bytes) = child.data.get(crc_start..crc_end) else {
            continue;
        };
        let mut declared_le = [0u8; 4];
        let n = crc_bytes.len().min(4);
        declared_le[..n].copy_from_slice(&crc_bytes[..n]);
        let declared = u32::from_le_bytes(declared_le);

        let rest = &child.data[crc_end..];
        let computed = vaco_hash::crc32(rest);
        assert_eq!(
            declared, computed,
            "CRC-32 mismatch on Level-1 element id=0x{:X}",
            child.id
        );
        checked.push(child.id);
    }

    // A test that only ever finds one CRC-32-bearing element could not
    // distinguish "the algorithm is right" from "it happens to work for the
    // one element already used to derive it" — this fixture carries CRC-32
    // on six (SeekHead, Info, Tracks, Tags, Cluster, Cues), confirmed by
    // direct byte inspection when the fixture was captured.
    assert_eq!(
        checked.len(),
        6,
        "expected six CRC-32-bearing Level-1 elements, saw {checked:?}"
    );
}
