//! The local-set item encoding shared by every header-metadata set and the
//! Index Table Segment: a run of `Tag(u16 BE) Length(u16 BE) Value` items.
//!
//! Measured directly off real files (see `ul` module docs): every set whose
//! key carries the `0x53` group-2 byte — [`crate::ul::STRUCTURAL_SET_PREFIX`]
//! and [`crate::ul::INDEX_TABLE_SEGMENT_PREFIX`] alike — uses a **fixed**
//! 2-byte length here, not a BER length. That is a real difference from the
//! top-level KLV framing in [`crate::klv`], confirmed by decoding a real
//! Index Table Segment (whose `IndexEntryArray` item alone is 383 bytes,
//! comfortably requiring the full 16-bit width) and a real Preface (whose
//! items are all well under 128 bytes and would have looked identical under
//! a BER short form, which is why the partition pack's own KLV framing was
//! decoded first, deliberately, to rule that reading out).

use vaco_core::{Error, Result};
use vaco_limits::Budget;

/// One `Tag Length Value` item inside a local set.
#[derive(Debug, Clone, Copy)]
pub struct Item<'a> {
    pub tag: u16,
    pub value: &'a [u8],
}

/// Walk every item in `data`, calling `f(tag, value)` for each.
///
/// Stops cleanly at the end of `data`. A truncated final item (fewer than 4
/// header bytes left, or a declared length longer than what remains) is
/// [`Error::InvalidData`] rather than a panic or a silent short read — a
/// local set is not "forgiving" the way essence demuxing is (see the
/// project-wide detection/demuxing distinction): a truncated set genuinely
/// cannot be interpreted, so every property after the truncation point would
/// otherwise be silently fabricated from garbage.
///
/// `budget` charges one fuel unit per item, bounding the loop independently
/// of `data`'s length: a set built entirely of zero-length items cannot make
/// this loop run longer than `fuel` allows.
///
/// # Errors
/// [`Error::InvalidData`] on truncation. [`Error::LimitExceeded`] if fuel
/// runs out ([`vaco_limits::LimitError::FuelExhausted`], converted).
pub fn for_each_item<'a>(
    data: &'a [u8],
    budget: &mut Budget,
    mut f: impl FnMut(Item<'a>) -> Result<()>,
) -> Result<()> {
    let mut pos = 0usize;
    while pos < data.len() {
        budget.consume_fuel(1)?;
        let header_end = pos
            .checked_add(4)
            .ok_or(Error::InvalidData("local-set item header truncated"))?;
        let header: &[u8; 4] = data
            .get(pos..header_end)
            .and_then(|s| s.first_chunk::<4>())
            .ok_or(Error::InvalidData("local-set item header truncated"))?;
        let tag = u16::from_be_bytes([header[0], header[1]]);
        let len = usize::from(u16::from_be_bytes([header[2], header[3]]));
        let value_end = header_end
            .checked_add(len)
            .ok_or(Error::InvalidData("local-set item value truncated"))?;
        let value = data
            .get(header_end..value_end)
            .ok_or(Error::InvalidData("local-set item value truncated"))?;
        f(Item { tag, value })?;
        pos = value_end;
    }
    Ok(())
}

// ------------------------------------------------------------ value helpers

/// Read a big-endian value of a fixed width out of an item's bytes.
///
/// Every structural-metadata property has a spec-defined width; a value
/// whose length does not match it is data corruption, not a variant to
/// tolerate, so this returns `None` rather than reading a truncated prefix.
#[must_use]
pub fn u8_(v: &[u8]) -> Option<u8> {
    v.first().copied()
}

#[must_use]
pub fn u16_be(v: &[u8]) -> Option<u16> {
    v.first_chunk::<2>().copied().map(u16::from_be_bytes)
}

#[must_use]
pub fn u32_be(v: &[u8]) -> Option<u32> {
    v.first_chunk::<4>().copied().map(u32::from_be_bytes)
}

#[must_use]
pub fn i32_be(v: &[u8]) -> Option<i32> {
    v.first_chunk::<4>().copied().map(i32::from_be_bytes)
}

#[must_use]
pub fn u64_be(v: &[u8]) -> Option<u64> {
    v.first_chunk::<8>().copied().map(u64::from_be_bytes)
}

#[must_use]
pub fn i64_be(v: &[u8]) -> Option<i64> {
    v.first_chunk::<8>().copied().map(i64::from_be_bytes)
}

/// An 8-byte `{ numerator: i32, denominator: i32 }` rational, the encoding
/// every `EditRate`/`SampleRate`/`AspectRatio` property in this crate uses.
#[must_use]
pub fn rational_be(v: &[u8]) -> Option<vaco_core::Rational> {
    let bytes = v.first_chunk::<8>()?;
    let num = i32::from_be_bytes(*bytes.first_chunk::<4>()?);
    let den = i32::from_be_bytes(bytes.get(4..8)?.first_chunk::<4>().copied()?);
    Some(vaco_core::Rational { num, den })
}

/// A 16-byte instance/strong-reference UID, or any other fixed 16-byte
/// field (a UL embedded as a property value).
#[must_use]
pub fn uid16(v: &[u8]) -> Option<[u8; 16]> {
    v.first_chunk::<16>().copied()
}

/// A `StrongRefArray`/`WeakRefArray`/generic batch of fixed-size items:
/// `Count(u32 BE) ItemLength(u32 BE)` followed by `Count` items of
/// `ItemLength` bytes each.
///
/// Bounded by `budget.check_count`, so a batch whose declared `Count` would
/// walk past `v`'s actual length is refused before the caller iterates it —
/// the array is real memory that already arrived (`v` is a slice into an
/// already-bounded value buffer), so what needs bounding here is the
/// `Count`/`ItemLength` *product* against what is actually present, not a
/// fresh allocation.
#[derive(Debug)]
pub struct Batch<'a> {
    pub item_len: usize,
    pub items: &'a [u8],
}

/// # Errors
/// [`Error::InvalidData`] if the batch header is truncated, or its declared
/// `Count * ItemLength` overflows or exceeds `v`'s actual length.
pub fn batch<'a>(v: &'a [u8], budget: &Budget) -> Result<Batch<'a>> {
    let count = u32_be(v).ok_or(Error::InvalidData("batch header truncated"))?;
    let item_len = v
        .get(4..8)
        .and_then(u32_be)
        .ok_or(Error::InvalidData("batch header truncated"))?;
    budget.check_count("mxf_batch_count", u64::from(count), u64::from(u32::MAX))?;
    let total = u64::from(count)
        .checked_mul(u64::from(item_len))
        .ok_or(Error::InvalidData("batch count*item_len overflows"))?;
    let total = usize::try_from(total).map_err(|_| Error::InvalidData("batch too large"))?;
    let end = 8usize
        .checked_add(total)
        .ok_or(Error::InvalidData("batch shorter than count*item_len"))?;
    let items = v
        .get(8..end)
        .ok_or(Error::InvalidData("batch shorter than count*item_len"))?;
    Ok(Batch {
        item_len: usize::try_from(item_len)
            .map_err(|_| Error::InvalidData("item_len too large"))?,
        items,
    })
}

impl<'a> Batch<'a> {
    /// Iterate the fixed-size items.
    pub fn iter(&self) -> impl Iterator<Item = &'a [u8]> {
        self.items.chunks_exact(self.item_len.max(1))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn item(tag: u16, value: &[u8]) -> Vec<u8> {
        let mut v = tag.to_be_bytes().to_vec();
        v.extend_from_slice(&(value.len() as u16).to_be_bytes());
        v.extend_from_slice(value);
        v
    }

    #[test]
    fn walks_items_matching_a_real_preface_prefix() {
        // Tag 0x3c0a (InstanceUID), 16-byte value — measured shape.
        let mut data = item(0x3c0a, &[0xAB; 16]);
        data.extend(item(0x3b05, &[0x01, 0x03]));
        let mut budget = Budget::new(Limits::strict());
        let mut seen = Vec::new();
        for_each_item(&data, &mut budget, |it| {
            seen.push((it.tag, it.value.to_vec()));
            Ok(())
        })
        .unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].0, 0x3c0a);
        assert_eq!(seen[1].1, vec![0x01, 0x03]);
    }

    #[test]
    fn truncated_item_value_is_invalid_data_not_a_panic() {
        let mut data = 0x3c0au16.to_be_bytes().to_vec();
        data.extend_from_slice(&100u16.to_be_bytes()); // claims 100 bytes
        data.extend_from_slice(&[0u8; 4]); // only 4 present
        let mut budget = Budget::new(Limits::strict());
        let err = for_each_item(&data, &mut budget, |_| Ok(())).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn rational_decodes_25_over_1() {
        let bytes = [0, 0, 0, 25, 0, 0, 0, 1];
        let r = rational_be(&bytes).unwrap();
        assert_eq!(r.num, 25);
        assert_eq!(r.den, 1);
    }

    #[test]
    fn batch_of_one_ul_matches_essence_containers_property() {
        // count=1, item_len=16, one 16-byte item — the shape of Preface's
        // EssenceContainers batch in a real file.
        let mut v = 1u32.to_be_bytes().to_vec();
        v.extend_from_slice(&16u32.to_be_bytes());
        v.extend_from_slice(&[0x42; 16]);
        let budget = Budget::new(Limits::strict());
        let b = batch(&v, &budget).unwrap();
        let items: Vec<_> = b.iter().collect();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0], &[0x42; 16]);
    }

    #[test]
    fn batch_with_hostile_count_is_rejected_not_oom() {
        // count claims 4 billion items of 16 bytes each; no such buffer
        // exists, so this must fail cleanly rather than try to slice it.
        let mut v = u32::MAX.to_be_bytes().to_vec();
        v.extend_from_slice(&16u32.to_be_bytes());
        let budget = Budget::new(Limits::strict());
        assert!(batch(&v, &budget).is_err());
    }
}
