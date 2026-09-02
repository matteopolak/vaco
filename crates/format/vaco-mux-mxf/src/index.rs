//! Writing Index Table Segments (SMPTE ST 377-1 §10): [`build`] for VBE
//! (OP1a/OP-Atom's MPEG-2 long-GOP essence has variable frame size — the
//! same shape `vaco-demux-mxf`'s own corpus needed the
//! `IndexEntryArray`/`StreamOffset` path for), [`build_cbe`] for D-10's
//! constant-bitrate profile (`EditUnitByteCount` arithmetic, no
//! `IndexEntryArray` needed).
//!
//! # Scope
//!
//! `SliceCount = 0`: this file has one essence track in its `BodySID`, so
//! there is nothing to slice — `vaco-demux-mxf`'s own documented scope
//! limit ("one essence track per `BodySID`... an index table that
//! interleaves several tracks via `DeltaEntryArray` is not de-interleaved")
//! is why this crate does not attempt to build a real multi-slice index
//! for a two-track file either: `MxfMuxer::write_trailer` indexes the video
//! track only, and a second essence track (if present) is fully readable
//! by sequential `read_packet` — which never consults the index at all —
//! but is not currently seekable-to. Tag numbers below were measured
//! directly off a real `ffmpeg -f mxf` footer's Index Table Segment this
//! session (`provenance/sources.toml`'s `ffmpeg-mxf-mux-header-probe`);
//! they agree exactly with `vaco-demux-mxf::index`'s own test fixture,
//! which is what confirms they are the real RP210 convention and not an
//! artifact of that test's arbitrary tag choice.

use crate::localset::{push_i64, push_rational, push_u8, push_u32, push_uid16};
use crate::ul::INDEX_TABLE_SEGMENT;

/// One video edit unit's byte position, for `IndexEntryArray`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Entry {
    pub stream_offset: u64,
    pub is_key_frame: bool,
}

/// Build one Index Table Segment's `(key, value)`.
#[must_use]
pub(crate) fn build(
    instance_uid: [u8; 16],
    edit_rate: (i32, i32),
    duration: i64,
    index_sid: u32,
    body_sid: u32,
    entries: &[Entry],
) -> ([u8; 16], Vec<u8>) {
    let mut v = Vec::new();
    push_uid16(&mut v, 0x3c0a, instance_uid);
    push_rational(&mut v, 0x3f0b, edit_rate.0, edit_rate.1);
    push_i64(&mut v, 0x3f0c, 0);
    push_i64(&mut v, 0x3f0d, duration);
    push_u32(&mut v, 0x3f05, 0); // EditUnitByteCount = 0: VBE.
    push_u32(&mut v, 0x3f06, index_sid);
    push_u32(&mut v, 0x3f07, body_sid);
    push_u8(&mut v, 0x3f08, 0); // SliceCount.

    let mut arr = Vec::new();
    arr.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    arr.extend_from_slice(&11u32.to_be_bytes()); // item_len: 1+1+1+8, SliceCount=0.
    for e in entries {
        let flags: u8 = if e.is_key_frame { 0x80 } else { 0x00 };
        arr.push(0); // TemporalOffset.
        arr.push(if e.is_key_frame { 0 } else { -1i8 as u8 }); // KeyFrameOffset.
        arr.push(flags);
        arr.extend_from_slice(&e.stream_offset.to_be_bytes());
    }
    push_bytes(&mut v, 0x3f0a, &arr);

    (INDEX_TABLE_SEGMENT, v)
}

/// Build one **CBE** Index Table Segment's `(key, value)` — D-10's own
/// shape, measured against a real `ffmpeg -f mxf_d10` file's
/// header-embedded Index Table Segment: `EditUnitByteCount` nonzero (every
/// edit unit is exactly that many bytes), `IndexDuration = 0` (computed
/// entirely upfront since D-10 is CBR, no footer deferral needed — the
/// header itself carries this segment, see `mux.rs`'s `MxfVariant::D10`
/// docs), and no `IndexEntryArray` at all: the real file measured did carry
/// a short `DeltaEntryArray`-shaped batch under an unidentified local tag
/// (`0x3f09` in that file's own primer) that this crate does not attempt to
/// reproduce (not measured with confidence — see
/// `docs/format/vaco-mux-mxf.md`), and no `IndexEntryArray` at all, which
/// `vaco-demux-mxf::index::parse` already treats as optional (a CBE
/// segment does not need one: `IndexTableSegment::cbe_offset` computes any
/// edit unit's position from `EditUnitByteCount` alone).
#[must_use]
pub(crate) fn build_cbe(
    instance_uid: [u8; 16],
    edit_rate: (i32, i32),
    edit_unit_byte_count: u32,
    index_sid: u32,
    body_sid: u32,
) -> ([u8; 16], Vec<u8>) {
    let mut v = Vec::new();
    push_uid16(&mut v, 0x3c0a, instance_uid);
    push_rational(&mut v, 0x3f0b, edit_rate.0, edit_rate.1);
    push_i64(&mut v, 0x3f0c, 0);
    push_i64(&mut v, 0x3f0d, 0); // IndexDuration = 0: computed from the container, not restated.
    push_u32(&mut v, 0x3f05, edit_unit_byte_count);
    push_u32(&mut v, 0x3f06, index_sid);
    push_u32(&mut v, 0x3f07, body_sid);
    push_u8(&mut v, 0x3f08, 0); // SliceCount.
    (INDEX_TABLE_SEGMENT, v)
}

fn push_bytes(out: &mut Vec<u8>, tag: u16, value: &[u8]) {
    out.extend_from_slice(&tag.to_be_bytes());
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value);
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "test code"
)]
mod tests {
    use super::*;

    #[test]
    fn entry_item_length_is_eleven_bytes_with_no_slices() {
        let (_key, v) = build(
            [0u8; 16],
            (25, 1),
            2,
            2,
            1,
            &[
                Entry {
                    stream_offset: 0,
                    is_key_frame: true,
                },
                Entry {
                    stream_offset: 26049,
                    is_key_frame: false,
                },
            ],
        );
        // Find the IndexEntryArray item (tag 0x3f0a) and check its header.
        let idx = v
            .windows(2)
            .position(|w| w == [0x3f, 0x0a])
            .expect("IndexEntryArray item present");
        let count = u32::from_be_bytes(v[idx + 4..idx + 8].try_into().unwrap());
        let item_len = u32::from_be_bytes(v[idx + 8..idx + 12].try_into().unwrap());
        assert_eq!(count, 2);
        assert_eq!(item_len, 11);
    }
}
