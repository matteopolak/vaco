//! Generic Container essence element keys (SMPTE ST 379-1) and the
//! frame-wrapped / clip-wrapped distinction.
//!
//! # What is measured, what is derived
//!
//! The 12-byte [`GC_ESSENCE_PREFIX`] and the **frame-wrapped** item-type byte
//! (`0x15`, "Picture") were measured directly off a real `OP1a` file: `out.mxf`'s
//! only essence element key is `060e2b34.01020101.0d010301.15010500`, present
//! at exactly the three byte offsets `ffprobe -show_packets` reports as
//! `pos` for its three packets. The **track-number matching** rule —
//! `Track.EssenceTrackNumber` (property `0x4804`) equals an essence
//! element's own last 4 key bytes, verbatim — is also measured: the file's
//! one video Track carries `0x4804 = 15 01 05 00`, byte for byte the key
//! above.
//!
//! # `0x05..=0x08` is not reliably "clip-wrapped" — corrected against a real D-10 file
//!
//! An earlier version of this module read ST 379-1 Table 1 as: item-type
//! byte `0x05..=0x08` means clip-wrapped (one KLV for the whole essence
//! track), one lower per essence family than the frame-wrapped `0x15..=0x18`
//! range, and shipped that as [`Wrapping`] without a real file to check it
//! against.
//!
//! A real `ffmpeg -f mxf_d10` file (D-10/SMPTE 386M, byte-exact against
//! `ffprobe` — see `demux.rs`'s D-10 tests) contradicts it: its Picture
//! essence element key ends `05 01 01 00` — squarely in the "clip-wrapped"
//! range — yet the file carries **twenty-five separate KLVs with that exact
//! key**, one per frame, each holding exactly one edit unit's CBR bytes
//! (confirmed both by hex-dumping the file directly and by `ffprobe
//! -show_packets` reporting twenty-five packets at those same offsets).
//! That is frame-wrapped by any operational definition, at a key byte this
//! crate's own code had classified as clip-wrapped.
//!
//! [`Wrapping`] is kept as an essence-*family* identifier only now — it is
//! not used to decide how a real file's packets are framed. Every essence
//! element this crate demuxes, D-10 included, is read as "one KLV, one
//! packet" (`demux::MxfDemuxer::read_packet`), which is what every file in
//! this crate's corpus — `OP1a`, OP-Atom and D-10 alike — has actually turned
//! out to do. Whether ST 379-1 genuinely ties wrapping to this byte at all,
//! or whether D-10's own mapping (SMPTE 386M, older and more specific than
//! the generic Generic Container table) simply reuses a numeric range the
//! generic table assigns a different meaning to, was not resolved — it did
//! not need to be, once the operational behaviour was measured directly.
//!
//! [`clip_wrapped_spans`] and its CBE/VBE span math are still real,
//! spec-shaped code, still not wired into `demux::MxfDemuxer::read_packet`,
//! and now additionally not backed by a reliable way to *detect* a
//! genuinely clip-wrapped essence element up front (see above) — wiring it
//! in would need both a real clip-wrapped sample (still not producible with
//! the installed `ffmpeg 8.1`, which has no clip-wrap option for its `mxf`/
//! `mxf_d10` muxers either) and a non-byte-range way to decide when to call
//! it, such as comparing a KLV's declared length against the index table's
//! own known edit-unit size instead of trusting the item-type byte.

use vaco_core::{Error, Result};

use crate::index::IndexTableSegment;
use crate::ul::Ul;

/// The 12-byte prefix shared by every Generic Container essence element key
/// this crate has seen or derived from spec, across every essence family.
pub const GC_ESSENCE_PREFIX: [u8; 12] = [
    0x06, 0x0e, 0x2b, 0x34, 0x01, 0x02, 0x01, 0x01, 0x0d, 0x01, 0x03, 0x01,
];

/// The Generic Container System Item's own 12-byte prefix, measured off a
/// real single-partition D-10 file (`ffmpeg -f mxf_d10`): present once per
/// edit unit, sharing the essence element's Generic Container designator
/// (`0d.01.03.01`) but a different registry-category prefix (`02.05.01.01`,
/// the same one partition packs and the primer share, versus essence
/// elements' `01.02.01.01`). This crate does not interpret the System
/// Item's content, only recognises the key so `metadata::scan_region` knows
/// to stop there — see that function's doc comment.
pub const GC_SYSTEM_ITEM_PREFIX: [u8; 12] = [
    0x06, 0x0e, 0x2b, 0x34, 0x02, 0x05, 0x01, 0x01, 0x0d, 0x01, 0x03, 0x01,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrapping {
    /// The item-type byte ST 379-1 Table 1 assigns to frame-wrapped
    /// essence (`0x15..=0x18`). Confirmed operationally frame-wrapped (one
    /// KLV per edit unit) against a real `OP1a` file — see module docs.
    Frame,
    /// The item-type byte ST 379-1 Table 1 assigns to clip-wrapped essence
    /// (`0x05..=0x08`) — **not confirmed to mean "one KLV for the whole
    /// track"**: a real D-10 file uses a key in this exact range and is
    /// measured, operationally, to be frame-wrapped (one KLV per edit
    /// unit, twenty-five of them). See the module docs for the full
    /// account. This variant is kept for the byte classification itself,
    /// not as a "this needs `clip_wrapped_spans`" signal — nothing in this
    /// crate currently makes that decision from the byte alone.
    Clip,
    Unknown(u8),
}

impl Ul {
    /// Whether this key is a Generic Container essence element.
    #[must_use]
    pub fn is_essence_element(self) -> bool {
        self.matches_prefix(&GC_ESSENCE_PREFIX)
    }

    /// Whether this key is the Generic Container System Item. See
    /// [`GC_SYSTEM_ITEM_PREFIX`].
    #[must_use]
    pub fn is_generic_container_system_item(self) -> bool {
        self.matches_prefix(&GC_SYSTEM_ITEM_PREFIX)
    }

    /// The essence element's wrapping, from its item-type byte (index 12).
    #[must_use]
    pub const fn wrapping(self) -> Wrapping {
        match self.0[12] {
            0x05..=0x08 => Wrapping::Clip,
            0x15..=0x18 => Wrapping::Frame,
            other => Wrapping::Unknown(other),
        }
    }

    /// The last 4 key bytes, as the big-endian "track number" a Track's
    /// `EssenceTrackNumber` property is matched against verbatim.
    #[must_use]
    pub const fn track_number(self) -> u32 {
        u32::from_be_bytes([self.0[12], self.0[13], self.0[14], self.0[15]])
    }
}

/// Same order of magnitude as `demux::MAX_CBE_INDEX_ENTRIES` and
/// `index::MAX_INDEX_ENTRIES`: this crate's other two "how many edit units
/// can one CBE segment claim" caps. Not yet reachable from a real demux
/// (this function is unwired — see the module docs), but a value this
/// large divided by a tiny `EditUnitByteCount` is exactly the "attacker
/// picks a huge declared essence-element length, this crate allocates a
/// span per implied edit unit" shape those two caps exist to close off, so
/// it is capped here too rather than left as the one CBE-count loop in
/// this crate without one.
const MAX_CBE_SPANS: u64 = 16 * 1024 * 1024;

/// Slice a clip-wrapped essence element's absolute byte range into one span
/// per edit unit, using its Index Table Segment.
///
/// Spec-derived (ST 377-1 §10.2.3), **not exercised against a real file** —
/// see the module docs. Returned spans are `(offset, length)` absolute file
/// positions, ready to seek to and read.
///
/// # Errors
/// [`Error::Unsupported`] if the segment has neither a nonzero
/// `EditUnitByteCount` (CBE) nor any `IndexEntryArray` entries (VBE): there
/// is no way to find edit-unit boundaries in a clip-wrapped element without
/// one of the two.
pub fn clip_wrapped_spans(
    value_offset: u64,
    value_len: u64,
    index: &IndexTableSegment,
) -> Result<Vec<(u64, u64)>> {
    if index.is_cbe() {
        let unit = u64::from(index.edit_unit_byte_count);
        if unit == 0 {
            return Ok(Vec::new());
        }
        // `unit` is checked non-zero immediately above.
        #[allow(clippy::integer_division, reason = "unit is checked non-zero above")]
        let count = (value_len / unit).min(MAX_CBE_SPANS);
        let mut spans = Vec::new();
        for n in 0..count {
            let Some(off) = index.cbe_offset(n) else {
                break;
            };
            spans.push((value_offset.saturating_add(off), unit));
        }
        return Ok(spans);
    }
    if index.entries.is_empty() {
        return Err(Error::Unsupported(
            "mxf: clip-wrapped essence has neither a CBE edit unit size nor index entries",
        ));
    }
    let mut spans = Vec::new();
    for (i, entry) in index.entries.iter().enumerate() {
        let start = value_offset.saturating_add(entry.stream_offset);
        let end = index
            .entries
            .get(i + 1)
            .map_or(value_offset.saturating_add(value_len), |next| {
                value_offset.saturating_add(next.stream_offset)
            });
        spans.push((start, end.saturating_sub(start)));
    }
    Ok(spans)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use crate::index::IndexTableEntry;

    const VIDEO_KEY: Ul = Ul::new([
        0x06, 0x0e, 0x2b, 0x34, 0x01, 0x02, 0x01, 0x01, 0x0d, 0x01, 0x03, 0x01, 0x15, 0x01, 0x05,
        0x00,
    ]);

    #[test]
    fn recognises_the_measured_frame_wrapped_key() {
        assert!(VIDEO_KEY.is_essence_element());
        assert_eq!(VIDEO_KEY.wrapping(), Wrapping::Frame);
        assert_eq!(VIDEO_KEY.track_number(), 0x1501_0500);
    }

    #[test]
    fn a_real_d10_key_classifies_as_clip_but_is_operationally_frame_wrapped() {
        // Measured against a real `ffmpeg -f mxf_d10` file (see the module
        // docs, and `demux.rs`'s D-10 tests): this key's item-type byte
        // (0x05) falls in ST 379-1's "clip-wrapped" range, but the file
        // carries twenty-five separate KLVs sharing this exact key, one
        // per edit unit -- not one KLV for the whole track. `Wrapping`
        // still reports the byte-range classification (it is real, it is
        // just not reliable evidence of framing), which is why this test
        // exists: to keep that gap visible rather than "fixing" `wrapping()`
        // to lie about which ST 379-1 range the byte is actually in.
        const D10_VIDEO_KEY: Ul = Ul::new([
            0x06, 0x0e, 0x2b, 0x34, 0x01, 0x02, 0x01, 0x01, 0x0d, 0x01, 0x03, 0x01, 0x05, 0x01,
            0x01, 0x00,
        ]);
        assert!(D10_VIDEO_KEY.is_essence_element());
        assert_eq!(D10_VIDEO_KEY.wrapping(), Wrapping::Clip);
        // `demux::tests::d10_fixture_demuxes_three_packets_matching_measured_positions_and_sizes`
        // is the operational proof: three separate packets, not one.
    }

    #[test]
    fn track_number_matches_the_measured_track_property_bytes() {
        // Track.EssenceTrackNumber was measured as raw bytes `15 01 05 00`.
        let raw: u32 = u32::from_be_bytes([0x15, 0x01, 0x05, 0x00]);
        assert_eq!(VIDEO_KEY.track_number(), raw);
    }

    #[test]
    fn cbe_spans_are_evenly_sized() {
        let seg = IndexTableSegment {
            edit_unit_byte_count: 100,
            ..Default::default()
        };
        let spans = clip_wrapped_spans(1000, 350, &seg).unwrap();
        assert_eq!(spans, vec![(1000, 100), (1100, 100), (1200, 100)]);
    }

    #[test]
    fn vbe_spans_come_from_consecutive_entry_offsets() {
        let seg = IndexTableSegment {
            entries: vec![
                IndexTableEntry {
                    stream_offset: 0,
                    ..Default::default()
                },
                IndexTableEntry {
                    stream_offset: 26049,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let spans = clip_wrapped_spans(6144, 40000, &seg).unwrap();
        assert_eq!(spans, vec![(6144, 26049), (32193, 13951)]);
    }

    #[test]
    fn no_cbe_and_no_entries_is_unsupported_not_a_guess() {
        let seg = IndexTableSegment::default();
        assert!(clip_wrapped_spans(0, 100, &seg).is_err());
    }
}
