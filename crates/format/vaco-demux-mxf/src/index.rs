//! The Index Table Segment (SMPTE ST 377-1 §10) and seeking through it.
//!
//! # CBE vs VBE
//!
//! `EditUnitByteCount` is the whole distinction: nonzero means every edit
//! unit is exactly that many bytes (**CBE** — constant bytes per edit unit,
//! the shape D-10 and other fixed-bitrate mappings use), so a seek target's
//! byte offset is `BodyOffset + edit_unit * EditUnitByteCount`, computed, no
//! table lookup needed. Zero means **VBE** (variable), and the actual
//! per-entry byte offsets live in `IndexEntryArray` — this crate's own
//! corpus file is VBE (long-GOP MPEG-2 has no fixed frame size), which is
//! why `IndexEntryArray`, not the CBE arithmetic, is what got verified
//! against a real file (module docs in `ul`).
//!
//! # What is measured vs derived
//!
//! Every tag and the `IndexEntryArray` item shape (`TemporalOffset(i8)
//! KeyFrameOffset(i8) Flags(u8) StreamOffset(u64 BE)`, then `SliceCount`
//! extra `u32` slice offsets) was decoded from a real footer partition's
//! Index Table Segment: `SliceCount=1` and `IndexEntryArray`'s item length
//! is `15` there, `11 + 4*1`, confirming the trailing slice-offset shape.
//! `PosTableCount` (an extra `8 * PosTableCount` bytes per entry, for
//! B-frame temporal reordering tables) was **not** exercised by that file
//! (it is absent/zero) — support for a nonzero `PosTableCount` is
//! spec-derived, not measured, and is called out again on
//! [`IndexTableSegment::index_entry_len`].

use std::collections::HashMap;

use vaco_core::{Error, Rational, Result};
use vaco_limits::Budget;

use crate::localset;
use crate::properties::{PropertyId, Resolver};
use crate::ul::Ul;

#[derive(Debug, Clone, Copy, Default)]
pub struct IndexTableEntry {
    pub temporal_offset: i8,
    pub key_frame_offset: i8,
    pub flags: u8,
    /// Byte offset from the start of this index table's essence container,
    /// i.e. relative to the owning partition's `BodyOffset`.
    pub stream_offset: u64,
}

impl IndexTableEntry {
    /// Bit 7 of `Flags` (ST 377-1 Table 30): whether decoding may start here.
    #[must_use]
    pub const fn is_key_frame(self) -> bool {
        self.flags & 0x80 != 0
    }
}

#[derive(Debug, Clone, Default)]
pub struct IndexTableSegment {
    pub index_edit_rate: Option<Rational>,
    pub index_start_position: i64,
    pub index_duration: i64,
    /// Nonzero: CBE, every edit unit is this many bytes. Zero: VBE, use
    /// `entries`.
    pub edit_unit_byte_count: u32,
    pub index_sid: u32,
    pub body_sid: u32,
    pub slice_count: u8,
    pub entries: Vec<IndexTableEntry>,
}

impl IndexTableSegment {
    #[must_use]
    pub const fn is_cbe(&self) -> bool {
        self.edit_unit_byte_count != 0
    }

    /// The byte size of one `IndexEntryArray` item, given this segment's
    /// `SliceCount`. `PosTableCount` would add `8 * PosTableCount` more
    /// (spec-derived; see the module docs for why this crate has not
    /// measured that case) — not modelled here, so a segment that uses it
    /// will have its `entries` misparsed. Detected and reported rather than
    /// silently wrong: [`parse`] checks the declared item length against
    /// this formula and returns [`Error::Unsupported`] on a mismatch.
    #[must_use]
    pub const fn index_entry_len(&self) -> usize {
        11 + 4 * self.slice_count as usize
    }

    /// The byte offset (relative to the essence container's start, i.e. add
    /// the owning partition's `BodyOffset`) of edit unit `n`, for a CBE
    /// segment.
    #[must_use]
    pub fn cbe_offset(&self, n: u64) -> Option<u64> {
        if !self.is_cbe() {
            return None;
        }
        n.checked_mul(u64::from(self.edit_unit_byte_count))
    }
}

/// Real files carry a handful of entries per index table segment (the
/// corpus file: 25, one per frame of a one-second clip); a very long
/// programme could plausibly reach the low millions. 16 million caps memory
/// at a few hundred MB even at the widest `SliceCount` while refusing an
/// obviously hostile count before it is used to size anything.
const MAX_INDEX_ENTRIES: u64 = 16 * 1024 * 1024;

/// Parse one Index Table Segment from its already-bounded value bytes.
///
/// # Errors
/// [`Error::InvalidData`] on a malformed local set or batch.
/// [`Error::LimitExceeded`] if `IndexEntryArray`'s count is implausible.
/// [`Error::Unsupported`] if `PosTableCount` is nonzero (see module docs).
#[allow(
    clippy::implicit_hasher,
    reason = "internal API; every caller in this crate uses the standard HashMap"
)]
pub fn parse(
    value: &[u8],
    primer: &HashMap<u16, Ul>,
    resolver: &Resolver,
    budget: &mut Budget,
) -> Result<IndexTableSegment> {
    let mut seg = IndexTableSegment::default();
    let mut raw_entries: Option<Vec<u8>> = None;
    localset::for_each_item(value, budget, |item| {
        let Some(prop) = resolver.resolve(primer, item.tag) else {
            return Ok(());
        };
        match prop {
            PropertyId::IndexEditRate => seg.index_edit_rate = localset::rational_be(item.value),
            PropertyId::IndexStartPosition => {
                seg.index_start_position = localset::i64_be(item.value).unwrap_or(0);
            }
            PropertyId::IndexDuration => {
                seg.index_duration = localset::i64_be(item.value).unwrap_or(0);
            }
            PropertyId::EditUnitByteCount => {
                seg.edit_unit_byte_count = localset::u32_be(item.value).unwrap_or(0);
            }
            PropertyId::IndexSid => seg.index_sid = localset::u32_be(item.value).unwrap_or(0),
            PropertyId::BodySid => seg.body_sid = localset::u32_be(item.value).unwrap_or(0),
            PropertyId::SliceCount => seg.slice_count = localset::u8_(item.value).unwrap_or(0),
            PropertyId::IndexEntryArray => raw_entries = Some(item.value.to_vec()),
            _ => {}
        }
        Ok(())
    })?;

    let Some(raw) = raw_entries else {
        return Ok(seg);
    };
    let batch = localset::batch(&raw, budget)?;
    let expected = seg.index_entry_len();
    if batch.item_len != expected {
        // Either PosTableCount is nonzero (not modelled — see module docs)
        // or the file is corrupt. Both are a real "we cannot read this"
        // rather than a guess, so this is Unsupported, not InvalidData.
        return Err(Error::Unsupported(
            "mxf: index entry length implies a non-zero PosTableCount, which this crate does not parse yet",
        ));
    }
    let count = u64::try_from(batch.items.len())
        .unwrap_or(u64::MAX)
        .checked_div(expected.max(1) as u64)
        .unwrap_or(0);
    budget.check_count("mxf_index_entries", count, MAX_INDEX_ENTRIES)?;
    for item in batch.iter() {
        let temporal_offset = i8::from_be_bytes([item.first().copied().unwrap_or(0)]);
        let key_frame_offset = i8::from_be_bytes([item.get(1).copied().unwrap_or(0)]);
        let flags = item.get(2).copied().unwrap_or(0);
        let stream_offset = item.get(3..11).and_then(localset::u64_be).unwrap_or(0);
        seg.entries.push(IndexTableEntry {
            temporal_offset,
            key_frame_offset,
            flags,
            stream_offset,
        });
    }
    Ok(seg)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn tag_for(resolver: &Resolver, primer: &mut HashMap<u16, Ul>, tag: u16, prop: PropertyId) {
        let ul = crate::properties::TABLE
            .iter()
            .find(|&&(p, _)| p == prop)
            .map(|&(_, ul)| ul)
            .unwrap();
        primer.insert(tag, ul);
        let _ = resolver;
    }

    fn item(tag: u16, value: &[u8]) -> Vec<u8> {
        let mut v = tag.to_be_bytes().to_vec();
        v.extend_from_slice(&(value.len() as u16).to_be_bytes());
        v.extend_from_slice(value);
        v
    }

    #[test]
    fn parses_a_vbe_segment_matching_the_measured_shape() {
        let resolver = Resolver::new();
        let mut primer = HashMap::new();
        tag_for(&resolver, &mut primer, 0x3f0b, PropertyId::IndexEditRate);
        tag_for(
            &resolver,
            &mut primer,
            0x3f0c,
            PropertyId::IndexStartPosition,
        );
        tag_for(&resolver, &mut primer, 0x3f0d, PropertyId::IndexDuration);
        tag_for(
            &resolver,
            &mut primer,
            0x3f05,
            PropertyId::EditUnitByteCount,
        );
        tag_for(&resolver, &mut primer, 0x3f06, PropertyId::IndexSid);
        tag_for(&resolver, &mut primer, 0x3f07, PropertyId::BodySid);
        tag_for(&resolver, &mut primer, 0x3f08, PropertyId::SliceCount);
        tag_for(&resolver, &mut primer, 0x3f0a, PropertyId::IndexEntryArray);

        let mut value = Vec::new();
        value.extend(item(0x3f0b, &[0, 0, 0, 25, 0, 0, 0, 1]));
        value.extend(item(0x3f0c, &0i64.to_be_bytes()));
        value.extend(item(0x3f0d, &3i64.to_be_bytes()));
        value.extend(item(0x3f05, &0u32.to_be_bytes())); // VBE
        value.extend(item(0x3f06, &2u32.to_be_bytes()));
        value.extend(item(0x3f07, &1u32.to_be_bytes()));
        value.extend(item(0x3f08, &[1]));
        let mut entries = 2u32.to_be_bytes().to_vec();
        entries.extend_from_slice(&15u32.to_be_bytes()); // item_len = 11 + 4*1
        // Entry 0: keyframe, stream_offset 0, one slice offset (ignored by
        // this parser today; present only so the item length matches).
        entries.extend_from_slice(&[0, 0, 0x80]);
        entries.extend_from_slice(&0u64.to_be_bytes());
        entries.extend_from_slice(&0u32.to_be_bytes());
        // Entry 1: not a keyframe, stream_offset 26049.
        entries.extend_from_slice(&[1, 0xFF, 0x00]);
        entries.extend_from_slice(&26049u64.to_be_bytes());
        entries.extend_from_slice(&0u32.to_be_bytes());
        value.extend(item(0x3f0a, &entries));

        let mut budget = Budget::new(Limits::permissive());
        let seg = parse(&value, &primer, &resolver, &mut budget).unwrap();
        assert!(!seg.is_cbe());
        assert_eq!(seg.index_edit_rate, Some(Rational { num: 25, den: 1 }));
        assert_eq!(seg.index_duration, 3);
        assert_eq!(seg.entries.len(), 2);
        assert!(seg.entries[0].is_key_frame());
        assert!(!seg.entries[1].is_key_frame());
        assert_eq!(seg.entries[1].stream_offset, 26049);
        assert_eq!(seg.entries[1].key_frame_offset, -1);
    }

    #[test]
    fn a_cbe_segment_computes_offsets_arithmetically() {
        let seg = IndexTableSegment {
            edit_unit_byte_count: 150_000,
            ..Default::default()
        };
        assert!(seg.is_cbe());
        assert_eq!(seg.cbe_offset(0), Some(0));
        assert_eq!(seg.cbe_offset(3), Some(450_000));
    }

    #[test]
    fn a_hostile_entry_count_is_rejected() {
        let resolver = Resolver::new();
        let mut primer = HashMap::new();
        tag_for(&resolver, &mut primer, 0x3f08, PropertyId::SliceCount);
        tag_for(&resolver, &mut primer, 0x3f0a, PropertyId::IndexEntryArray);
        let mut value = item(0x3f08, &[0]);
        let mut entries = u32::MAX.to_be_bytes().to_vec();
        entries.extend_from_slice(&11u32.to_be_bytes());
        value.extend(item(0x3f0a, &entries));
        let mut budget = Budget::new(Limits::permissive());
        assert!(parse(&value, &primer, &resolver, &mut budget).is_err());
    }
}
