//! Generic Container essence element keys (SMPTE ST 379-1) and the
//! frame-wrapped / clip-wrapped distinction.
//!
//! # What is measured, what is derived
//!
//! The 12-byte [`GC_ESSENCE_PREFIX`] and the **frame-wrapped** item-type byte
//! (`0x15`, "Picture") were measured directly off a real file: `out.mxf`'s
//! only essence element key is `060e2b34.01020101.0d010301.15010500`, present
//! at exactly the three byte offsets `ffprobe -show_packets` reports as
//! `pos` for its three packets. The **track-number matching** rule —
//! `Track.EssenceTrackNumber` (property `0x4804`) equals an essence
//! element's own last 4 key bytes, verbatim — is also measured: the file's
//! one video Track carries `0x4804 = 15 01 05 00`, byte for byte the key
//! above.
//!
//! The **clip-wrapped** item-type bytes (`0x05..=0x08`, one lower per
//! essence family than the frame-wrapped set) are ST 379-1 Table 1,
//! spec-derived and **not exercised by this crate's corpus** — generating a
//! real clip-wrapped or D-10 sample with the installed `ffmpeg 8.1` hit an
//! encoder-side quirk documented in this crate's closing report rather than
//! in code (the muxer refused to write a CBR frame that did not exactly
//! match its computed index-unit size for every quantiser this crate tried).
//! [`ClipWrappedReader`] is therefore real, spec-shaped code that has not
//! been checked against a real file — flagged here rather than silently
//! presented as equally solid.

use vaco_core::{Error, Result};

use crate::index::IndexTableSegment;
use crate::ul::Ul;

/// The 12-byte prefix shared by every Generic Container essence element key
/// this crate has seen or derived from spec, across every essence family.
pub const GC_ESSENCE_PREFIX: [u8; 12] = [
    0x06, 0x0e, 0x2b, 0x34, 0x01, 0x02, 0x01, 0x01, 0x0d, 0x01, 0x03, 0x01,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrapping {
    /// One KLV element per edit unit. Measured (see module docs).
    Frame,
    /// The whole track is one KLV element; edit unit boundaries come from
    /// the index table, never from KLV framing. Spec-derived, unexercised.
    Clip,
    Unknown(u8),
}

impl Ul {
    /// Whether this key is a Generic Container essence element.
    #[must_use]
    pub fn is_essence_element(self) -> bool {
        self.matches_prefix(&GC_ESSENCE_PREFIX)
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
        let count = value_len / unit;
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
