//! The `RF64`/`ds64` 64-bit-size extension.
//!
//! EBU Tech 3306 (also published by Microsoft as the `RF64` extension to the
//! WAVE format): a WAVE file larger than 4 GiB cannot express its true size in
//! a 32-bit `ckSize`, so the outer container id becomes `RF64` (in place of
//! `RIFF`), its own declared size is `0xFFFFFFFF`, and a `ds64` chunk —
//! mandatory and always first inside the container — carries the real 64-bit
//! sizes for the `RF64` container itself and for `data`, plus an optional
//! table of sizes for any other chunk that also overflowed 32 bits.
//!
//! This module only *parses* `ds64`. Deciding which chunk's declared size to
//! override with which table entry is a demuxer's job, not this crate's —
//! see the module-level docs on [`crate::chunk`] for why a declared size is
//! something this crate clamps rather than a promise it enforces.

use vaco_bitstream::ByteReader;
use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::chunk::ChunkId;

/// One entry in the `ds64` table: the true size of a chunk whose ordinary
/// `ckSize` also read `0xFFFFFFFF`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ds64TableEntry {
    pub id: ChunkId,
    pub size: u64,
}

/// The parsed `ds64` chunk payload.
#[derive(Debug, Clone)]
pub struct Ds64 {
    /// The true size of the outer `RF64` container (in place of its
    /// `0xFFFFFFFF` placeholder).
    pub riff_size: u64,
    /// The true size of the `data` chunk.
    pub data_size: u64,
    /// The true sample count, overriding a `fact` chunk's 32-bit count when
    /// present.
    pub sample_count: u64,
    /// Overrides for any other chunk whose own `ckSize` also overflowed.
    pub table: Vec<Ds64TableEntry>,
}

/// Bytes in the fixed part of `ds64`, before the table:
/// `riffSize(8) + dataSize(8) + sampleCount(8) + tableLength(4)`.
const FIXED_LEN: usize = 28;
/// Bytes per table entry: `chunkId(4) + chunkSize(8)`.
const ENTRY_LEN: usize = 12;

impl Ds64 {
    /// Parse a `ds64` chunk payload.
    ///
    /// `table_len` in the input is untrusted; the table is read through
    /// `budget` and clamped to the entries the payload can actually hold, the
    /// same discipline `vaco-format-isom`'s fixed-stride tables use.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when the payload is shorter than the fixed
    /// portion. [`vaco_core::Error::LimitExceeded`] if the table would exceed
    /// `budget`.
    pub fn parse(payload: &[u8], budget: &mut Budget) -> Result<Self> {
        if payload.len() < FIXED_LEN {
            return Err(Error::InvalidData(
                "riff: ds64 chunk shorter than its fixed fields",
            ));
        }
        let mut r = ByteReader::new(payload);
        let riff_size = r.le64();
        let data_size = r.le64();
        let sample_count = r.le64();
        // Unlike the three sizes above (each a 64-bit quantity stored as two
        // consecutive 32-bit little-endian words, bit-identical to one LE64
        // read), `dwTableLength` is a single `DWORD`.
        let declared_len = r.le32();
        r.check()?;

        let body = r.rest();
        #[allow(
            clippy::integer_division,
            reason = "ENTRY_LEN is the constant 12; this clamps the declared count to what the payload can actually hold"
        )]
        let available = (body.len() / ENTRY_LEN) as u64;
        let n = u64::from(declared_len).min(available);
        let mut table = budget.alloc::<Ds64TableEntry>(usize::try_from(n).unwrap_or(usize::MAX))?;
        let mut er = ByteReader::new(body);
        for slot in &mut table {
            let mut id = [0u8; 4];
            let tag = er.bytes(4);
            let take = tag.len().min(4);
            if let (Some(dst), Some(src)) = (id.get_mut(..take), tag.get(..take)) {
                dst.copy_from_slice(src);
            }
            let size = er.le64();
            *slot = Ds64TableEntry {
                id: ChunkId(id),
                size,
            };
        }

        Ok(Self {
            riff_size,
            data_size,
            sample_count,
            table,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn ds64_bytes(riff: u64, data: u64, samples: u64, table: &[(&[u8; 4], u64)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&riff.to_le_bytes());
        out.extend_from_slice(&data.to_le_bytes());
        out.extend_from_slice(&samples.to_le_bytes());
        out.extend_from_slice(&(table.len() as u32).to_le_bytes());
        for (id, size) in table {
            out.extend_from_slice(*id);
            out.extend_from_slice(&size.to_le_bytes());
        }
        out
    }

    #[test]
    fn parses_the_fixed_fields_with_no_table() {
        let bytes = ds64_bytes(1 << 33, 1 << 32, 12345, &[]);
        let mut budget = Budget::new(Limits::permissive());
        let ds = Ds64::parse(&bytes, &mut budget).unwrap();
        assert_eq!(ds.riff_size, 1 << 33);
        assert_eq!(ds.data_size, 1 << 32);
        assert_eq!(ds.sample_count, 12345);
        assert!(ds.table.is_empty());
    }

    #[test]
    fn parses_table_entries() {
        let bytes = ds64_bytes(0, 0, 0, &[(b"data", 1 << 34), (b"fact", 99)]);
        let mut budget = Budget::new(Limits::permissive());
        let ds = Ds64::parse(&bytes, &mut budget).unwrap();
        assert_eq!(ds.table.len(), 2);
        assert_eq!(ds.table[0].id, ChunkId::new(b"data"));
        assert_eq!(ds.table[0].size, 1 << 34);
        assert_eq!(ds.table[1].id, ChunkId::new(b"fact"));
        assert_eq!(ds.table[1].size, 99);
    }

    #[test]
    fn a_declared_table_length_past_the_payload_is_clamped() {
        let mut bytes = ds64_bytes(0, 0, 0, &[(b"data", 1)]);
        // Lie about the table length: claim a million entries.
        let lie = 1_000_000u64.to_le_bytes();
        bytes[24..28].copy_from_slice(&lie[..4]);
        let mut budget = Budget::new(Limits::permissive());
        let ds = Ds64::parse(&bytes, &mut budget).unwrap();
        // Only the one real entry is present; nothing panics or over-allocates.
        assert_eq!(ds.table.len(), 1);
    }

    #[test]
    fn too_short_a_payload_is_rejected() {
        let mut budget = Budget::new(Limits::permissive());
        assert!(Ds64::parse(&[0; 10], &mut budget).is_err());
    }
}
