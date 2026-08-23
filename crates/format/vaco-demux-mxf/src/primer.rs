//! The Primer Pack (SMPTE ST 377-1 §8.2): the table that maps a header
//! metadata set's two-byte local tags to full 16-byte Universal Labels.
//!
//! Nothing above the KLV layer is readable without this — every property in
//! every structural-metadata set (see [`crate::metadata`]) is addressed by a
//! local tag that means nothing until it is looked up here, which is why
//! this file is read immediately after the header partition pack and before
//! anything else in header metadata is interpreted.
//!
//! Layout, measured against a real file (see `ul` module docs): `Count(u32
//! BE) ItemLength(u32 BE)` then `Count` entries of `Tag(u16 BE) UID(16
//! bytes)` — `ItemLength` is `18` in every file this crate has seen, and is
//! trusted as stated rather than hardcoded, since nothing prevents a
//! conformant future revision from widening it.

use std::collections::HashMap;

use vaco_core::{Error, Result};
use vaco_io::IoContext;
use vaco_limits::Budget;

use crate::klv::{self, KlvHeader};
use crate::ul::Ul;

/// A real primer pack holds a few hundred entries at most (the corpus file
/// this crate measured against has 100). 65536 covers the entire local-tag
/// address space and is still refused before an oversized `Count` causes any
/// real work.
const MAX_PRIMER_ENTRIES: u64 = 65536;

/// Generous bound on a primer pack's total encoded size: `65536 * 18 + 8`
/// bytes, rounded up.
const MAX_PRIMER_BYTES: u64 = MAX_PRIMER_ENTRIES * 18 + 4096;

/// Parse a Primer Pack into a local-tag → UL map.
///
/// # Errors
///
/// [`Error::InvalidData`] if `header.key` is not a primer pack, or the value
/// is shorter than its own declared `Count * ItemLength`.
/// [`Error::LimitExceeded`] if `Count` is implausible.
pub fn parse(
    io: &mut IoContext,
    budget: &mut Budget,
    header: &KlvHeader,
) -> Result<HashMap<u16, Ul>> {
    use crate::ul::PartitionFamilyKind;
    if header.key.partition_family_kind() != Some(PartitionFamilyKind::Primer) {
        return Err(Error::InvalidData("mxf: not a primer pack key"));
    }
    let value = klv::read_value(io, budget, header, MAX_PRIMER_BYTES)?;
    let batch = crate::localset::batch(&value, budget)?;
    let count = u64::try_from(batch.items.len())
        .unwrap_or(u64::MAX)
        .checked_div(batch.item_len.max(1) as u64)
        .unwrap_or(0);
    budget.check_count("mxf_primer_entries", count, MAX_PRIMER_ENTRIES)?;
    let mut map = HashMap::new();
    for item in batch.iter() {
        // `Tag(2) UID(16)`. A shorter-than-18-byte item length is either a
        // future minor revision this crate has not seen or corruption; both
        // are handled the same way — skip the entry, keep going, since a
        // primer pack that is mostly readable should still unlock the tags
        // it can.
        let Some(tag) = item.first_chunk::<2>().copied().map(u16::from_be_bytes) else {
            continue;
        };
        let Some(uid) = item.get(2..18).and_then(Ul::parse) else {
            continue;
        };
        map.insert(tag, uid);
    }
    Ok(map)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_io::{IoOptions, MemorySource};
    use vaco_limits::Limits;

    fn primer_pack_bytes(entries: &[(u16, [u8; 16])]) -> Vec<u8> {
        let mut key = vec![
            0x06, 0x0e, 0x2b, 0x34, 0x02, 0x05, 0x01, 0x01, 0x0d, 0x01, 0x02, 0x01, 0x01, 0x05,
            0x01, 0x00,
        ];
        let mut value = (entries.len() as u32).to_be_bytes().to_vec();
        value.extend_from_slice(&18u32.to_be_bytes());
        for (tag, uid) in entries {
            value.extend_from_slice(&tag.to_be_bytes());
            value.extend_from_slice(uid);
        }
        key.push(value.len() as u8); // short-form BER length
        key.extend_from_slice(&value);
        key
    }

    #[test]
    fn parses_two_entries_matching_a_real_measured_pair() {
        let bytes = primer_pack_bytes(&[
            (
                0x3c0a,
                [
                    0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x15, 0x02, 0x00,
                    0x00, 0x00, 0x00,
                ],
            ),
            (0x0201, [0xAB; 16]),
        ]);
        let mut io =
            IoContext::new(Box::new(MemorySource::new(bytes)), &IoOptions::default()).unwrap();
        let mut budget = Budget::new(Limits::permissive());
        let header = klv::read_header(&mut io).unwrap();
        let map = parse(&mut io, &mut budget, &header).unwrap();
        assert_eq!(map.len(), 2);
        assert_eq!(
            map[&0x3c0a].as_bytes(),
            [
                0x06, 0x0e, 0x2b, 0x34, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x15, 0x02, 0x00, 0x00,
                0x00, 0x00
            ]
        );
    }

    #[test]
    fn a_hostile_entry_count_is_rejected() {
        let mut key = vec![
            0x06, 0x0e, 0x2b, 0x34, 0x02, 0x05, 0x01, 0x01, 0x0d, 0x01, 0x02, 0x01, 0x01, 0x05,
            0x01, 0x00,
        ];
        let mut value = u32::MAX.to_be_bytes().to_vec();
        value.extend_from_slice(&18u32.to_be_bytes());
        key.push(0x82);
        key.extend_from_slice(&8u16.to_be_bytes());
        key.extend_from_slice(&value);
        let mut io =
            IoContext::new(Box::new(MemorySource::new(key)), &IoOptions::default()).unwrap();
        let mut budget = Budget::new(Limits::permissive());
        let header = klv::read_header(&mut io).unwrap();
        assert!(parse(&mut io, &mut budget, &header).is_err());
    }
}
