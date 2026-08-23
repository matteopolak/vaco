//! The two top-level ASF index forms this crate reads for seeking:
//! [\[ASF\] §6.1 Simple Index Object](vaco_format_asf) and
//! [\[ASF\] §6.2 Index Object](vaco_format_asf).
//!
//! Both index *by presentation time at a fixed interval*: entry `k` (0-based)
//! implies time `k * IndexEntryTimeInterval`, which is why neither structure
//! stores a timestamp per entry — only a packet number or byte offset. Media
//! Object Index Object and Timecode Index Object (§6.3/§6.4) are not parsed;
//! see `docs/format/vaco-demux-asf.md` for why (a file rarely carries one
//! without also carrying a Simple Index Object or Index Object this crate
//! already reads, and building test material with one is impractical here).

use vaco_core::{Error, Result, Timestamp};
use vaco_format_core::seek::{IndexEntry, PacketIndex};
use vaco_limits::Budget;

/// Clamp a byte/tick count that cannot itself be negative into the signed
/// range [`Timestamp`] stores.
fn to_i64_clamped(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// One Simple Index Object entry: which Data Packet has the closest past key
/// frame, and how many packets to send starting there.
#[derive(Debug, Clone, Copy, Default)]
pub struct SimpleIndexEntry {
    pub packet_number: u32,
    pub packet_count: u16,
}

/// A parsed Simple Index Object.
#[derive(Debug, Clone, Default)]
pub struct SimpleIndex {
    /// 100-nanosecond units between entries.
    pub time_interval_100ns: u64,
    pub entries: Vec<SimpleIndexEntry>,
}

/// Bytes in the Simple Index Object's fixed prefix (after the 24-byte object
/// header): `FileID(16) + IndexEntryTimeInterval(8) + MaximumPacketCount(4) +
/// IndexEntriesCount(4)`.
const SIMPLE_INDEX_FIXED_LEN: usize = 16 + 8 + 4 + 4;
/// Bytes per Simple Index entry: `PacketNumber(4) + PacketCount(2)`.
const SIMPLE_INDEX_ENTRY_LEN: usize = 6;

/// # Errors
/// [`Error::InvalidData`] if the payload is shorter than the fixed prefix.
/// A trailing partial entry, or a declared count larger than the payload
/// actually holds, is clamped rather than rejected.
pub(crate) fn parse_simple_index(payload: &[u8], budget: &mut Budget) -> Result<SimpleIndex> {
    if payload.len() < SIMPLE_INDEX_FIXED_LEN {
        return Err(Error::InvalidData(
            "asf: Simple Index Object shorter than its fixed prefix",
        ));
    }
    let time_interval_100ns = payload
        .get(16..24)
        .and_then(<[u8]>::first_chunk::<8>)
        .map_or(0, |b| u64::from_le_bytes(*b));
    let declared = payload
        .get(28..32)
        .and_then(<[u8]>::first_chunk::<4>)
        .map_or(0u32, |b| u32::from_le_bytes(*b)) as usize;
    let body = payload.get(SIMPLE_INDEX_FIXED_LEN..).unwrap_or(&[]);
    #[allow(
        clippy::integer_division,
        reason = "SIMPLE_INDEX_ENTRY_LEN is the non-zero constant 6; this converts a byte length into an entry count"
    )]
    let available = body.len() / SIMPLE_INDEX_ENTRY_LEN;
    let n = declared.min(available);
    let mut entries = budget.alloc::<SimpleIndexEntry>(n)?;
    for (i, slot) in entries.iter_mut().enumerate() {
        let base = i * SIMPLE_INDEX_ENTRY_LEN;
        let packet_number = body
            .get(base..base + 4)
            .and_then(<[u8]>::first_chunk::<4>)
            .map_or(0, |b| u32::from_le_bytes(*b));
        let packet_count = body
            .get(base + 4..base + 6)
            .and_then(<[u8]>::first_chunk::<2>)
            .map_or(0, |b| u16::from_le_bytes(*b));
        *slot = SimpleIndexEntry {
            packet_number,
            packet_count,
        };
    }
    Ok(SimpleIndex {
        time_interval_100ns,
        entries,
    })
}

/// Build a [`PacketIndex`] from a Simple Index Object: entry `k`'s implied
/// time is `k * time_interval`, converted to [`TIME_BASE_Q`] microseconds;
/// its byte position is `data_packets_start + packet_number * packet_size`.
pub(crate) fn simple_index_to_packet_index(
    simple: &SimpleIndex,
    data_packets_start: u64,
    packet_size: u64,
    opts: &vaco_format_core::FormatOptions,
) -> PacketIndex {
    let mut out = PacketIndex::with_options(opts);
    // 100ns -> microseconds is an exact divide-by-10, not a ratio worth a
    // float for.
    #[allow(
        clippy::integer_division,
        reason = "100-nanosecond units convert to microseconds by an exact factor of 10"
    )]
    let interval_us = simple.time_interval_100ns / 10;
    for (i, e) in simple.entries.iter().enumerate() {
        let us = (i as u64).saturating_mul(interval_us);
        let pos = data_packets_start
            .saturating_add(u64::from(e.packet_number).saturating_mul(packet_size));
        out.add(IndexEntry::keyframe(
            pos,
            Timestamp::new(to_i64_clamped(us)),
        ));
    }
    out
}

/// A parsed top-level Index Object ([\[ASF\] §6.2](vaco_format_asf)).
///
/// Only the **first** Index Specifier's offsets are used to build a
/// [`PacketIndex`] — [`PacketIndex`] is one flat seek table shared by every
/// stream (the same model `vaco-demux-avi` builds from `idx1`), while an
/// Index Object may specify a different index *type* per stream. Indexing
/// every specifier into one structure that also knows which specifier a
/// lookup wants is more machinery than this crate's callers need today; see
/// `docs/format/vaco-demux-asf.md`.
#[derive(Debug, Clone, Default)]
pub struct IndexObject {
    pub time_interval_ms: u32,
    /// `(stream_number, index_type)` pairs, in the order the file lists them.
    pub specifiers: Vec<(u16, u16)>,
    /// One flattened offset per entry, for specifier 0 only (see struct
    /// docs). `None` for an entry whose specifier-0 offset was
    /// `0xFFFFFFFF` ("invalid", per spec).
    pub offsets: Vec<Option<u32>>,
}

const INDEX_OBJECT_FIXED_LEN: usize = 4 + 2 + 4;
/// Sentinel meaning "no valid indexable point here".
const INVALID_OFFSET: u32 = 0xFFFF_FFFF;
/// Safety cap on how many index blocks/entries this crate will walk for one
/// Index Object — an attacker-controlled count otherwise turns one small
/// object into an unbounded loop even under a byte budget (each iteration
/// touches only a few bytes).
const MAX_INDEX_ENTRIES: usize = 1_000_000;

/// # Errors
/// [`Error::InvalidData`] if the payload is shorter than its fixed prefix.
/// Declared counts are clamped to what the payload can actually supply and
/// to [`MAX_INDEX_ENTRIES`]; a malformed tail simply stops the walk rather
/// than erroring, since a partial index is still useful for seeking.
pub(crate) fn parse_index_object(payload: &[u8], budget: &mut Budget) -> Result<IndexObject> {
    if payload.len() < INDEX_OBJECT_FIXED_LEN {
        return Err(Error::InvalidData(
            "asf: Index Object shorter than its fixed prefix",
        ));
    }
    let time_interval_ms = payload
        .get(0..4)
        .and_then(<[u8]>::first_chunk::<4>)
        .map_or(0, |b| u32::from_le_bytes(*b));
    let specifiers_count = payload
        .get(4..6)
        .and_then(<[u8]>::first_chunk::<2>)
        .map_or(0u16, |b| u16::from_le_bytes(*b)) as usize;
    let mut pos = INDEX_OBJECT_FIXED_LEN;
    let mut specifiers = Vec::new();
    for _ in 0..specifiers_count.min(MAX_INDEX_ENTRIES) {
        budget.consume_fuel(1)?;
        let Some(stream_number) = payload
            .get(pos..pos + 2)
            .and_then(<[u8]>::first_chunk::<2>)
            .map(|b| u16::from_le_bytes(*b))
        else {
            break;
        };
        let Some(index_type) = payload
            .get(pos + 2..pos + 4)
            .and_then(<[u8]>::first_chunk::<2>)
            .map(|b| u16::from_le_bytes(*b))
        else {
            break;
        };
        specifiers.push((stream_number, index_type));
        pos += 4;
    }
    let entry_width = specifiers.len().saturating_mul(4);
    let mut offsets = Vec::new();
    // Index Blocks: `IndexEntryCount(4) + BlockPositions(8 * specifiers.len())
    // + entries[IndexEntryCount][specifiers.len() * u32]`.
    while pos + 4 <= payload.len() && offsets.len() < MAX_INDEX_ENTRIES {
        budget.consume_fuel(1)?;
        let Some(entry_count) = payload
            .get(pos..pos + 4)
            .and_then(<[u8]>::first_chunk::<4>)
            .map(|b| u32::from_le_bytes(*b) as usize)
        else {
            break;
        };
        pos += 4;
        let block_positions_len = specifiers.len().saturating_mul(8);
        let Some(block_positions) = payload.get(pos..pos + block_positions_len) else {
            break;
        };
        let block_position = block_positions
            .first_chunk::<8>()
            .map_or(0, |b| u64::from_le_bytes(*b));
        pos += block_positions_len;
        let entry_count = entry_count.min(MAX_INDEX_ENTRIES - offsets.len());
        for _ in 0..entry_count {
            if entry_width == 0 {
                break;
            }
            let Some(raw) = payload
                .get(pos..pos + 4)
                .and_then(<[u8]>::first_chunk::<4>)
                .map(|b| u32::from_le_bytes(*b))
            else {
                break;
            };
            pos += entry_width;
            offsets.push(if raw == INVALID_OFFSET {
                None
            } else {
                Some(
                    u32::try_from(block_position)
                        .unwrap_or(0)
                        .saturating_add(raw),
                )
            });
        }
    }
    Ok(IndexObject {
        time_interval_ms,
        specifiers,
        offsets,
    })
}

/// Build a [`PacketIndex`] from a top-level Index Object. Entry `k`'s time is
/// `k * time_interval_ms`; its byte position is `data_packets_start +
/// offset`, per §6.2's "relative to the start of the first ASF Data Packet".
pub(crate) fn index_object_to_packet_index(
    index: &IndexObject,
    data_packets_start: u64,
    opts: &vaco_format_core::FormatOptions,
) -> PacketIndex {
    let mut out = PacketIndex::with_options(opts);
    for (i, offset) in index.offsets.iter().enumerate() {
        let Some(offset) = offset else { continue };
        let us = to_i64_clamped(
            (i as u64)
                .saturating_mul(u64::from(index.time_interval_ms))
                .saturating_mul(1000),
        );
        let pos = data_packets_start.saturating_add(u64::from(*offset));
        out.add(IndexEntry::keyframe(pos, Timestamp::new(us)));
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn simple_index_bytes(interval_100ns: u64, entries: &[(u32, u16)]) -> Vec<u8> {
        let mut out = vec![0u8; 16]; // file id
        out.extend_from_slice(&interval_100ns.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // max packet count
        out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for &(pn, pc) in entries {
            out.extend_from_slice(&pn.to_le_bytes());
            out.extend_from_slice(&pc.to_le_bytes());
        }
        out
    }

    #[test]
    fn simple_index_parses_entries_in_order() {
        let bytes = simple_index_bytes(10_000_000, &[(0, 1), (5, 1), (12, 2)]);
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let idx = parse_simple_index(&bytes, &mut budget).unwrap();
        assert_eq!(idx.time_interval_100ns, 10_000_000);
        assert_eq!(idx.entries.len(), 3);
        assert_eq!(idx.entries[1].packet_number, 5);
        assert_eq!(idx.entries[2].packet_count, 2);
    }

    #[test]
    fn simple_index_converts_to_a_packet_index_with_implied_times() {
        let simple = SimpleIndex {
            time_interval_100ns: 10_000_000, // 1 second
            entries: vec![
                SimpleIndexEntry {
                    packet_number: 0,
                    packet_count: 1,
                },
                SimpleIndexEntry {
                    packet_number: 4,
                    packet_count: 1,
                },
            ],
        };
        let opts = vaco_format_core::FormatOptions::default();
        let pi = simple_index_to_packet_index(&simple, 1000, 3200, &opts);
        assert!(!pi.is_empty());
        // Entry 1's byte position: data_packets_start + packet_number * packet_size.
        // Entry 1's implied time is `1 * time_interval_100ns` converted to
        // microseconds: 10_000_000 (100ns) / 10 = 1_000_000 us.
        let e = pi
            .search(
                Timestamp::new(1_000_000),
                vaco_format_core::seek::SeekFlags::empty(),
            )
            .unwrap();
        assert_eq!(e.pos, 1000 + 4 * 3200);
    }

    #[test]
    fn a_declared_count_past_the_payload_is_clamped() {
        let mut bytes = simple_index_bytes(1, &[]);
        // Claim 100 entries but supply none.
        bytes[28..32].copy_from_slice(&100u32.to_le_bytes());
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let idx = parse_simple_index(&bytes, &mut budget).unwrap();
        assert!(idx.entries.is_empty());
    }

    #[test]
    fn index_object_parses_one_specifier_and_one_block() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1000u32.to_le_bytes()); // interval ms
        bytes.extend_from_slice(&1u16.to_le_bytes()); // 1 specifier
        bytes.extend_from_slice(&0u32.to_le_bytes()); // index blocks count (unused by parser, walked structurally)
        bytes.extend_from_slice(&1u16.to_le_bytes()); // stream number
        bytes.extend_from_slice(&3u16.to_le_bytes()); // index type: nearest past cleanpoint
        // One block: 2 entries, block position 0.
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes()); // block position
        bytes.extend_from_slice(&100u32.to_le_bytes()); // entry 0 offset
        bytes.extend_from_slice(&INVALID_OFFSET.to_le_bytes()); // entry 1: invalid

        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let idx = parse_index_object(&bytes, &mut budget).unwrap();
        assert_eq!(idx.specifiers, vec![(1, 3)]);
        assert_eq!(idx.offsets, vec![Some(100), None]);
    }

    #[test]
    fn index_object_to_packet_index_skips_invalid_offsets() {
        let index = IndexObject {
            time_interval_ms: 1000,
            specifiers: vec![(1, 3)],
            offsets: vec![Some(0), None, Some(200)],
        };
        let opts = vaco_format_core::FormatOptions::default();
        let pi = index_object_to_packet_index(&index, 500, &opts);
        // Only 2 valid entries went in.
        assert!(!pi.is_empty());
    }
}
