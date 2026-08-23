//! The Partition Pack (SMPTE ST 377-1 §6) and the Random Index Pack (§11).
//!
//! Every field's byte offset below was cross-checked against a real header,
//! body and footer partition pack written by `ffmpeg 8.1` (see `ul` module
//! docs for the exact command) — the fixed-position layout matched the spec
//! exactly, byte for byte, which is why this module reads positionally
//! rather than through the local-set machinery in [`crate::localset`] (the
//! partition pack predates that convention and never adopted it).

use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::klv::{self, KlvHeader};
use crate::ul::{PartitionFamilyKind, Ul};

/// A minimal bounds-checked cursor over a fixed-layout byte buffer.
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or(Error::InvalidData("mxf: fixed-layout field out of range"))?;
        let s = self
            .data
            .get(self.pos..end)
            .ok_or(Error::InvalidData("mxf: fixed-layout struct truncated"))?;
        self.pos = end;
        Ok(s)
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(
            self.take(2)?.try_into().unwrap_or([0; 2]),
        ))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().unwrap_or([0; 4]),
        ))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().unwrap_or([0; 8]),
        ))
    }

    fn ul(&mut self) -> Result<Ul> {
        Ok(Ul::new(self.take(16)?.try_into().unwrap_or([0; 16])))
    }
}

/// Which of the three partition kinds this is (SMPTE ST 377-1 §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionKind {
    Header,
    Body,
    Footer,
}

/// A parsed Partition Pack.
#[derive(Debug, Clone)]
pub struct PartitionPack {
    pub kind: PartitionKind,
    /// Absolute file offset of this partition pack's own key.
    pub this_partition: u64,
    pub previous_partition: u64,
    pub footer_partition: u64,
    /// Bytes of header metadata (primer pack + structural-metadata sets)
    /// following this partition pack, when this is a header or footer
    /// partition that carries one.
    pub header_byte_count: u64,
    /// Bytes of index table segments following the header metadata.
    pub index_byte_count: u64,
    pub index_sid: u32,
    /// Distance from the essence container's start to this partition's
    /// first edit unit, for a body partition carrying essence.
    pub body_offset: u64,
    pub body_sid: u32,
    pub operational_pattern: Ul,
    pub essence_containers: Vec<Ul>,
    /// Where the KLV immediately after this partition pack starts —
    /// `this_partition` plus the pack's own key+length+value size.
    pub content_offset: u64,
}

/// Generous but real: the widest header partition pack seen in the wild
/// carries a handful of essence-container labels, never hundreds. 64 KiB
/// leaves two orders of magnitude of headroom over anything a real file
/// does and still refuses a hostile length before it is read.
const MAX_PARTITION_PACK_BYTES: u64 = 64 * 1024;

/// Read one partition pack, whose key must already be known to be
/// [`Ul::is_any_partition_pack`] — the caller (the top-level scanner) is the
/// one deciding "this looks like a partition pack", because it is the one
/// that already read the key to make that decision.
///
/// # Errors
///
/// [`Error::InvalidData`] if the value is too short for the fixed layout, or
/// [`Error::LimitExceeded`] if its declared length is implausible.
pub fn parse(
    io: &mut vaco_io::IoContext,
    budget: &mut Budget,
    header: &KlvHeader,
) -> Result<PartitionPack> {
    let kind = match header.key.partition_family_kind() {
        Some(PartitionFamilyKind::Header) => PartitionKind::Header,
        Some(PartitionFamilyKind::Body) => PartitionKind::Body,
        Some(PartitionFamilyKind::Footer) => PartitionKind::Footer,
        _ => return Err(Error::InvalidData("mxf: not a partition pack key")),
    };
    let value = klv::read_value(io, budget, header, MAX_PARTITION_PACK_BYTES)?;
    let mut c = Cursor::new(&value);
    let _major = c.u16()?;
    let _minor = c.u16()?;
    let _kag_size = c.u32()?;
    let this_partition = c.u64()?;
    let previous_partition = c.u64()?;
    let footer_partition = c.u64()?;
    let header_byte_count = c.u64()?;
    let index_byte_count = c.u64()?;
    let index_sid = c.u32()?;
    let body_offset = c.u64()?;
    let body_sid = c.u32()?;
    let operational_pattern = c.ul()?;
    let ec_count = c.u32()?;
    let ec_item_len = c.u32()?;
    budget.check_count("mxf_essence_containers", u64::from(ec_count), 4096)?;
    let mut essence_containers = Vec::new();
    for _ in 0..ec_count {
        let raw = c.take(usize::try_from(ec_item_len).unwrap_or(0))?;
        if let Some(ul) = Ul::parse(raw) {
            essence_containers.push(ul);
        }
    }
    Ok(PartitionPack {
        kind,
        this_partition,
        previous_partition,
        footer_partition,
        header_byte_count,
        index_byte_count,
        index_sid,
        body_offset,
        body_sid,
        operational_pattern,
        essence_containers,
        content_offset: header.end(),
    })
}

/// One `(BodySID, ByteOffset)` entry from a Random Index Pack.
#[derive(Debug, Clone, Copy)]
pub struct RipEntry {
    pub body_sid: u32,
    pub byte_offset: u64,
}

/// The Random Index Pack: every partition's byte offset, so a seekable
/// reader can jump straight to any partition without walking the file.
#[derive(Debug, Clone)]
pub struct RandomIndexPack {
    pub entries: Vec<RipEntry>,
}

/// Real files stay under a few hundred partitions; 65536 leaves three
/// orders of magnitude of headroom while still refusing a hostile trailer
/// length before allocating anything sized by it.
const MAX_RIP_ENTRIES: u64 = 65536;

/// Locate and parse the Random Index Pack at the tail of a seekable file.
///
/// Per SMPTE ST 377-1 §11: the last 4 bytes of the file are a big-endian
/// `u32` giving the RIP's *total* KLV length (key + BER length + value), so
/// `file_len - that length` is the RIP key's offset. Verified against a real
/// file: `out.mxf`'s trailing `u32` is exactly `57`, the sum of its 16-byte
/// key, its 1-byte short-form BER length, and its 40-byte value.
///
/// Returns `Ok(None)` when the file is too short to hold a RIP, or when the
/// key at the computed offset is not a Random Index Pack — a partial or
/// pre-footer file legitimately has none, and that is not a parse error.
///
/// # Errors
/// [`Error::LimitExceeded`] if the RIP claims an implausible entry count.
pub fn find_rip(
    io: &mut vaco_io::IoContext,
    budget: &mut Budget,
    file_len: u64,
) -> Result<Option<RandomIndexPack>> {
    if file_len < 4 {
        return Ok(None);
    }
    io.seek(file_len - 4)?;
    let mut trailer = [0u8; 4];
    io.read_exact(&mut trailer)?;
    let total_len = u64::from(u32::from_be_bytes(trailer));
    // The trailer counts itself as part of the value; the RIP's own key is
    // 16 bytes, so anything smaller than that cannot be a real RIP.
    if total_len < 20 || total_len > file_len {
        return Ok(None);
    }
    let rip_offset = file_len - total_len;
    io.seek(rip_offset)?;
    let header = klv::read_header(io)?;
    if header.key.partition_family_kind() != Some(PartitionFamilyKind::RandomIndexPack) {
        return Ok(None);
    }
    let max_bytes = MAX_RIP_ENTRIES.saturating_mul(12).saturating_add(4);
    let value = klv::read_value(io, budget, &header, max_bytes)?;
    // `Count * 12 + 4` (the trailing length re-stated inside the value) is
    // the only valid shape; anything else is truncated or corrupt.
    if value.len() < 4 {
        return Err(Error::InvalidData("mxf: random index pack too short"));
    }
    let n = (value.len() - 4).checked_div(12).unwrap_or(0);
    budget.check_count("mxf_rip_entries", n as u64, MAX_RIP_ENTRIES)?;
    let mut entries = Vec::new();
    for i in 0..n {
        let base = i * 12;
        let Some(chunk) = value
            .get(base..base + 12)
            .and_then(|c| c.first_chunk::<12>())
        else {
            break;
        };
        let Some(body_sid_bytes) = chunk.get(0..4).and_then(|s| s.first_chunk::<4>()) else {
            break;
        };
        let Some(offset_bytes) = chunk.get(4..12).and_then(|s| s.first_chunk::<8>()) else {
            break;
        };
        let body_sid = u32::from_be_bytes(*body_sid_bytes);
        let byte_offset = u64::from_be_bytes(*offset_bytes);
        entries.push(RipEntry {
            body_sid,
            byte_offset,
        });
    }
    Ok(Some(RandomIndexPack { entries }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use crate::testutil::header_partition_pack_bytes;
    use vaco_io::{IoContext, IoOptions, MemorySource};
    use vaco_limits::Limits;

    #[test]
    fn parses_a_real_measured_header_partition_pack() {
        let bytes = header_partition_pack_bytes();
        let mut io =
            IoContext::new(Box::new(MemorySource::new(bytes)), &IoOptions::default()).unwrap();
        let mut budget = Budget::new(Limits::permissive());
        let header = klv::read_header(&mut io).unwrap();
        let pp = parse(&mut io, &mut budget, &header).unwrap();
        assert_eq!(pp.kind, PartitionKind::Header);
        assert_eq!(pp.this_partition, 0);
        assert_eq!(pp.footer_partition, 172_032);
        assert_eq!(pp.header_byte_count, 4608);
        assert_eq!(pp.body_sid, 0);
        assert_eq!(pp.essence_containers.len(), 1);
    }

    #[test]
    fn finds_a_real_measured_random_index_pack() {
        // Header partition pack, then padding, then the exact RIP bytes
        // measured from `out.mxf`.
        let mut bytes = header_partition_pack_bytes();
        bytes.resize(173_568, 0);
        // RIP key + short-form length 40 + 3 entries + trailing length 57.
        bytes.extend_from_slice(&[
            0x06, 0x0e, 0x2b, 0x34, 0x02, 0x05, 0x01, 0x01, 0x0d, 0x01, 0x02, 0x01, 0x01, 0x11,
            0x01, 0x00, 0x28,
        ]);
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u64.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&5120u64.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&172_032u64.to_be_bytes());
        bytes.extend_from_slice(&57u32.to_be_bytes());
        let file_len = bytes.len() as u64;
        let mut io =
            IoContext::new(Box::new(MemorySource::new(bytes)), &IoOptions::default()).unwrap();
        let mut budget = Budget::new(Limits::permissive());
        let rip = find_rip(&mut io, &mut budget, file_len).unwrap().unwrap();
        assert_eq!(rip.entries.len(), 3);
        assert_eq!(rip.entries[1].body_sid, 1);
        assert_eq!(rip.entries[1].byte_offset, 5120);
    }

    #[test]
    fn a_file_too_short_for_a_rip_reports_none_not_an_error() {
        let mut io = IoContext::new(
            Box::new(MemorySource::new(vec![1, 2, 3])),
            &IoOptions::default(),
        )
        .unwrap();
        let mut budget = Budget::new(Limits::strict());
        assert!(find_rip(&mut io, &mut budget, 3).unwrap().is_none());
    }

    #[test]
    fn a_hostile_rip_entry_count_is_rejected() {
        // A declared entry count (~833K) far past `MAX_RIP_ENTRIES`, backed
        // by a real (if wastefully padded) file so the rejection is provably
        // the entry-count guard and not merely a short read.
        const N: u32 = 10_000_000;
        let mut bytes = vec![0u8; 32];
        bytes.extend_from_slice(&[
            0x06, 0x0e, 0x2b, 0x34, 0x02, 0x05, 0x01, 0x01, 0x0d, 0x01, 0x02, 0x01, 0x01, 0x11,
            0x01, 0x00,
        ]);
        bytes.push(0x84);
        bytes.extend_from_slice(&N.to_be_bytes());
        bytes.resize(bytes.len() + N as usize, 0);
        // The trailing length is the *last 4 bytes of the value itself*
        // (measured: see `finds_a_real_measured_random_index_pack` above),
        // not a field appended after the KLV.
        let total_len = 16 + 5 + N; // key + long-form length + value
        let end = bytes.len();
        bytes[end - 4..].copy_from_slice(&total_len.to_be_bytes());
        let file_len = bytes.len() as u64;
        let mut io =
            IoContext::new(Box::new(MemorySource::new(bytes)), &IoOptions::default()).unwrap();
        let mut budget = Budget::new(Limits::permissive());
        let err = find_rip(&mut io, &mut budget, file_len).unwrap_err();
        assert!(matches!(err, Error::LimitExceeded { .. }));
    }
}
