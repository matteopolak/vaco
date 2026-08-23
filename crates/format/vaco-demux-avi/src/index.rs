//! `idx1` (`AVIOLDINDEX`) and the `OpenDML` `indx`/`ix##` two-level index.
//!
//! # The `idx1` offset ambiguity, measured rather than assumed
//!
//! `dwOffset` in an `idx1` entry is documented as relative to the start of the
//! `movi` list's data, but readers have to cope with files that instead wrote
//! it relative to the start of the file — both conventions exist in
//! deployed encoders. Measured directly: `ffmpeg -f lavfi -i testsrc=... -f
//! lavfi -i sine=... -c:v mpeg4 -c:a pcm_s16le out.avi`, then a byte-exact
//! Python walk of the result (`docs/format/vaco-demux-avi.md` has the script
//! and the full trace). The first three `idx1` entries:
//!
//! ```text
//! entry  ckid     dwOffset   candidate A (movi-fourcc + offset)   candidate B (absolute)
//! 0      00dc     4          9982 -> b"00dc"  MATCH                 4    -> garbage
//! 1      01wb     1572       11550 -> b"01wb" MATCH                 1572 -> zeros
//! 2      00dc     3628       13606 -> b"00dc" MATCH                 3628 -> zeros
//! ```
//!
//! `ffmpeg 8.1`'s own writer uses the documented convention: `dwOffset` is
//! relative to the byte at which the four-character `"movi"` list-type text
//! itself begins (not the chunk header before it, and not file byte zero).
//! [`detect_offset_base`] does not assume this holds for every writer — it
//! probes the first few entries against both candidates and adopts whichever
//! one's bytes at the computed position actually equal the entry's own
//! `dwChunkId`, falling back to the documented convention only when neither
//! candidate can be checked (a non-seekable source, or a corrupt entry).
//!
//! # `OpenDML` (`indx`/`ix##`)
//!
//! The >2 GiB extension: a `strl`'s `indx` (`AVISUPERINDEX`) chunk names a
//! sequence of standalone `ix##` (`AVISTDINDEX`) chunks elsewhere in the file,
//! each carrying a run of entries relative to its own `qwBaseOffset` — an
//! absolute file offset, so this level has no equivalent ambiguity. See
//! *`OpenDML` AVI File Format Extensions, v1.02* (published by the `OpenDML`
//! committee), §"Extended AVI Indexes". This module's `OpenDML` parsers have unit tests
//! against hand-built bytes; nothing here has been exercised against a real
//! multi-gigabyte file, since building one to test against is not practical in
//! this environment. See `docs/format/vaco-demux-avi.md`.

use std::collections::BTreeMap;

use vaco_bitstream::ByteReader;
use vaco_core::{Error, Result, Timestamp};
use vaco_format_core::seek::{IndexEntry, PacketIndex};
use vaco_format_core::time::TIME_BASE_Q;
use vaco_io::IoContext;
use vaco_limits::Budget;

/// The clock inputs `build_from_idx1` needs about one stream — deliberately
/// not `hdrl::StreamBuild` itself, since by the time the demuxer replays
/// `idx1` it has already split that into its public [`vaco_format_core::Stream`]
/// and a private clock-state struct of its own; this is the join of the two
/// facts this module actually reads.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ClockView {
    pub time_base: vaco_core::Rational,
    pub sample_size: u32,
    pub start: u32,
}

/// `AVIIF_KEYFRAME`.
pub(crate) const AVIIF_KEYFRAME: u32 = 0x0000_0010;
/// `AVIIF_NO_TIME` — this entry does not advance the stream's clock (a
/// palette change, typically).
pub(crate) const AVIIF_NO_TIME: u32 = 0x0000_0100;

/// One `AVIOLDINDEX` entry.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Idx1Entry {
    pub chunk_id: [u8; 4],
    pub flags: u32,
    pub offset: u32,
    pub size: u32,
}

/// Bytes per `idx1` entry: `dwChunkId(4) + dwFlags(4) + dwOffset(4) + dwSize(4)`.
const IDX1_ENTRY_LEN: usize = 16;

/// Parse an `idx1` chunk payload into its flat entry list.
///
/// A trailing partial entry (a payload whose length is not a multiple of 16)
/// is silently ignored rather than rejected — the same "declared counts are
/// clamped to what is actually there" discipline the rest of this crate's
/// dependencies use. Bounded by `budget`.
pub(crate) fn parse_idx1(payload: &[u8], budget: &mut Budget) -> Result<Vec<Idx1Entry>> {
    #[allow(
        clippy::integer_division,
        reason = "IDX1_ENTRY_LEN is the non-zero constant 16; this converts a byte length into an entry count"
    )]
    let n = payload.len() / IDX1_ENTRY_LEN;
    let mut out = budget.alloc::<Idx1Entry>(n)?;
    let mut r = ByteReader::new(payload);
    for slot in &mut out {
        let chunk_id = [r.u8(), r.u8(), r.u8(), r.u8()];
        let flags = r.le32();
        let offset = r.le32();
        let size = r.le32();
        *slot = Idx1Entry {
            chunk_id,
            flags,
            offset,
            size,
        };
    }
    // `ByteReader` zero-fills and flags rather than panicking on truncation,
    // but every entry above was taken from bytes we already bounded to `n *
    // IDX1_ENTRY_LEN <= payload.len()`, so `r.check()` cannot fail here.
    Ok(out)
}

/// Which convention an `idx1`'s `dwOffset` field uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OffsetBase {
    /// Relative to the byte at which the `movi` list's four-character
    /// list-type text begins. The convention the specification documents,
    /// and what `ffmpeg 8.1`'s own writer measured out to (see module docs).
    MoviRelative,
    /// Relative to byte zero of the file.
    Absolute,
}

impl OffsetBase {
    pub(crate) fn resolve(self, entry: &Idx1Entry, movi_fourcc_pos: u64) -> u64 {
        match self {
            Self::MoviRelative => movi_fourcc_pos.saturating_add(u64::from(entry.offset)),
            Self::Absolute => u64::from(entry.offset),
        }
    }
}

/// Probe the first few entries against both conventions and adopt whichever
/// one's bytes at the computed position equal the entry's own `dwChunkId`.
///
/// Checks more than the first entry because a degenerate first entry (a
/// `dwOffset` of zero landing on both candidates' shared prefix, or one this
/// crate cannot read back) should not force the fallback for a file where a
/// later entry disambiguates cleanly.
pub(crate) fn detect_offset_base(
    io: &mut IoContext,
    entries: &[Idx1Entry],
    movi_fourcc_pos: u64,
) -> OffsetBase {
    let file_size = io.size();
    for entry in entries.iter().take(8) {
        let movi_pos = movi_fourcc_pos.saturating_add(u64::from(entry.offset));
        if tag_at(io, movi_pos, file_size) == Some(entry.chunk_id) {
            return OffsetBase::MoviRelative;
        }
        let abs_pos = u64::from(entry.offset);
        if tag_at(io, abs_pos, file_size) == Some(entry.chunk_id) {
            return OffsetBase::Absolute;
        }
    }
    // Neither candidate could be confirmed (corrupt index, or a source that
    // cannot seek back to check) — fall back to the documented convention.
    OffsetBase::MoviRelative
}

/// Read four bytes at `pos` without disturbing the caller's position.
/// `None` on any failure, including a position past a known file size.
fn tag_at(io: &mut IoContext, pos: u64, file_size: Option<u64>) -> Option<[u8; 4]> {
    if let Some(sz) = file_size
        && pos.saturating_add(4) > sz
    {
        return None;
    }
    let resume = io.pos();
    let got = io.seek(pos).ok().and_then(|_| io.tag().ok());
    let _ = io.seek(resume);
    got
}

/// Parse a chunk id's stream index and two-character kind (`"db"`, `"dc"`,
/// `"wb"`, `"tx"`, `"pc"`, …), e.g. `b"01wb"` -> `(1, *b"wb")`.
///
/// `None` for anything that is not two ASCII digits followed by two bytes —
/// which is also how an `OpenDML` `"ix00"` standard-index chunk (whose first two
/// bytes are `"ix"`, not digits) is correctly excluded from being mistaken for
/// stream data.
pub(crate) fn parse_chunk_tag(id: [u8; 4]) -> Option<(u32, [u8; 2])> {
    let d0 = (id[0] as char).to_digit(10)?;
    let d1 = (id[1] as char).to_digit(10)?;
    Some((d0 * 10 + d1, [id[2], id[3]]))
}

/// The result of replaying an `idx1` (or resolved `OpenDML`) index against the
/// per-stream clock: seek points in a common time base, and the keyframe
/// status of every named byte position.
#[derive(Debug, Clone, Default)]
pub(crate) struct Resolved {
    /// Seek points, with every timestamp rescaled into
    /// [`vaco_format_core::time::TIME_BASE_Q`] microseconds — necessary
    /// because, unlike MPEG-TS or Matroska, different AVI streams routinely
    /// have genuinely different time bases, so raw tick comparison across
    /// streams would be comparing incompatible units. [`PacketIndex`] itself
    /// is oblivious to which stream an entry came from; this crate is what
    /// keeps that safe by never handing it two different units.
    pub index: PacketIndex,
    /// Chunk header byte offset -> keyframe flag, consulted during the
    /// sequential `movi` walk so [`crate::demux::AviDemuxer::read_packet`]
    /// does not have to re-derive keyframe status (`idx1` states it; nothing
    /// about the chunk bytes themselves does).
    pub keyframe_by_pos: BTreeMap<u64, bool>,
}

/// Per-stream state while replaying entries in file order — the same clock
/// [`crate::demux`] runs during the real sequential read, applied here to
/// `idx1`/`OpenDML` metadata instead of to bytes actually read off disk.
#[derive(Debug, Clone, Copy, Default)]
struct Clock {
    chunks: u64,
    bytes: u64,
}

/// Replay `idx1` entries (in file order, which is required to match `movi`'s
/// own order for the timestamps to come out right) against each stream's
/// declared clock, producing [`Resolved`].
pub(crate) fn build_from_idx1(
    entries: &[Idx1Entry],
    base: OffsetBase,
    movi_fourcc_pos: u64,
    streams: &[ClockView],
    opts: &vaco_format_core::FormatOptions,
) -> Resolved {
    let mut clocks = vec![Clock::default(); streams.len()];
    let mut out = Resolved {
        index: PacketIndex::with_options(opts),
        keyframe_by_pos: BTreeMap::new(),
    };
    for entry in entries {
        let Some((stream_idx, _kind)) = parse_chunk_tag(entry.chunk_id) else {
            continue;
        };
        let Some(i) = usize::try_from(stream_idx).ok() else {
            continue;
        };
        let Some(build) = streams.get(i) else {
            continue;
        };
        let Some(clock) = clocks.get_mut(i) else {
            continue;
        };
        let pos = base.resolve(entry, movi_fourcc_pos);
        let is_key = entry.flags & AVIIF_KEYFRAME != 0;
        out.keyframe_by_pos.insert(pos, is_key);

        let no_time = entry.flags & AVIIF_NO_TIME != 0;
        let ticks = if build.sample_size == 0 {
            i64::try_from(clock.chunks).unwrap_or(i64::MAX)
        } else {
            // As in `crate::demux`: `dwSampleSize` divides a byte count into
            // an exact sample count, not a ratio a float would approximate.
            #[allow(
                clippy::integer_division,
                reason = "dwSampleSize divides a byte count into an exact sample count, not a ratio"
            )]
            let ticks = clock.bytes / u64::from(build.sample_size).max(1);
            i64::try_from(ticks).unwrap_or(i64::MAX)
        };
        if !no_time {
            if build.sample_size == 0 {
                clock.chunks = clock.chunks.saturating_add(1);
            } else {
                clock.bytes = clock.bytes.saturating_add(u64::from(entry.size));
            }
        }
        let ts = Timestamp::new(ticks.saturating_add(i64::from(build.start)));
        let us = ts.rescale(build.time_base, TIME_BASE_Q, vaco_core::Rounding::default());
        let e = if is_key {
            IndexEntry::keyframe(pos, us)
        } else {
            IndexEntry::frame(pos, us)
        };
        out.index.add(e);
    }
    out
}

// ------------------------------------------------------------- `OpenDML`

/// One entry of an `AVISUPERINDEX` (`indx`): the location of one `ix##`
/// standard-index chunk.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SuperIndexEntry {
    pub offset: u64,
}

/// Bytes in `AVISUPERINDEX`'s fixed prefix, before its entries:
/// `wLongsPerEntry(2) + bIndexSubType(1) + bIndexType(1) + nEntriesInUse(4) +
/// dwChunkId(4) + dwReserved[3](12)`.
const SUPER_INDEX_FIXED_LEN: usize = 24;
/// Bytes per `AVISUPERINDEX` entry: `qwOffset(8) + dwSize(4) + dwDuration(4)`.
const SUPER_INDEX_ENTRY_LEN: usize = 16;
/// Safety cap on how many super-index entries (i.e. how many `ix##` chunks)
/// this crate will follow for one stream — an attacker-controlled count
/// otherwise turns one `indx` chunk into thousands of seeks.
const MAX_SUPER_INDEX_ENTRIES: usize = 4096;

/// Parse an `indx` (`AVISUPERINDEX`) payload.
///
/// Only `bIndexType == 0` (`AVI_INDEX_OF_INDEXES`, pointing at `ix##` chunks)
/// is meaningful here; `AVI_INDEX_OF_CHUNKS` embedded directly in `indx` is a
/// form this crate has not observed and does not parse — see
/// `docs/format/vaco-demux-avi.md`.
pub(crate) fn parse_super_index(
    payload: &[u8],
    budget: &mut Budget,
) -> Result<Vec<SuperIndexEntry>> {
    if payload.len() < SUPER_INDEX_FIXED_LEN {
        return Err(Error::InvalidData("avi: indx shorter than AVISUPERINDEX"));
    }
    let mut r = ByteReader::new(payload);
    let _longs_per_entry = r.le16();
    let _sub_type = r.u8();
    let index_type = r.u8();
    let declared = r.le32();
    r.check()?;
    if index_type != 0 {
        return Ok(Vec::new());
    }
    let body = payload.get(SUPER_INDEX_FIXED_LEN..).unwrap_or(&[]);
    #[allow(
        clippy::integer_division,
        reason = "SUPER_INDEX_ENTRY_LEN is the non-zero constant 16"
    )]
    let available = body.len() / SUPER_INDEX_ENTRY_LEN;
    let n = (declared as usize)
        .min(available)
        .min(MAX_SUPER_INDEX_ENTRIES);
    let mut out = budget.alloc::<SuperIndexEntry>(n)?;
    let mut br = ByteReader::new(body);
    for slot in &mut out {
        let offset = br.le64();
        let _size = br.le32();
        let _duration = br.le32();
        *slot = SuperIndexEntry { offset };
    }
    Ok(out)
}

/// One entry of an `AVISTDINDEX` (`ix##`), already resolved to an absolute
/// file position.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StdIndexEntry {
    pub pos: u64,
    pub is_key: bool,
}

/// Bytes in `AVISTDINDEX`'s fixed prefix: `wLongsPerEntry(2) +
/// bIndexSubType(1) + bIndexType(1) + nEntriesInUse(4) + dwChunkId(4) +
/// qwBaseOffset(8) + dwReserved3(4)`.
const STD_INDEX_FIXED_LEN: usize = 24;
/// Bytes per `AVISTDINDEX` entry: `dwOffset(4) + dwSize(4)`.
const STD_INDEX_ENTRY_LEN: usize = 8;
/// `dwSize`'s high bit: set means this frame is **not** a sync sample — the
/// opposite polarity from `idx1`'s `AVIIF_KEYFRAME`, per the `OpenDML` spec.
const STD_INDEX_NOT_KEYFRAME_BIT: u32 = 0x8000_0000;

/// Parse a standard index (`ix##`) chunk payload into absolute-position
/// entries, given the chunk's own `qwBaseOffset` (read from the payload
/// itself; unlike `idx1` this level of the format states its base explicitly,
/// so there is no ambiguity to detect).
pub(crate) fn parse_std_index(payload: &[u8], budget: &mut Budget) -> Result<Vec<StdIndexEntry>> {
    if payload.len() < STD_INDEX_FIXED_LEN {
        return Err(Error::InvalidData("avi: ix## shorter than AVISTDINDEX"));
    }
    let mut r = ByteReader::new(payload);
    let _longs_per_entry = r.le16();
    let _sub_type = r.u8();
    let _index_type = r.u8();
    let declared = r.le32();
    let _chunk_id = r.bytes(4);
    let base_offset = r.le64();
    r.check()?;
    let body = payload.get(STD_INDEX_FIXED_LEN..).unwrap_or(&[]);
    #[allow(
        clippy::integer_division,
        reason = "STD_INDEX_ENTRY_LEN is the non-zero constant 8"
    )]
    let available = body.len() / STD_INDEX_ENTRY_LEN;
    let n = (declared as usize).min(available);
    let mut out = budget.alloc::<StdIndexEntryRaw>(n)?;
    let mut br = ByteReader::new(body);
    for slot in &mut out {
        let offset = br.le32();
        let size = br.le32();
        *slot = StdIndexEntryRaw { offset, size };
    }
    Ok(out
        .into_iter()
        .map(|e| StdIndexEntry {
            pos: base_offset.saturating_add(u64::from(e.offset)),
            is_key: e.size & STD_INDEX_NOT_KEYFRAME_BIT == 0,
        })
        .collect())
}

/// Raw `(dwOffset, dwSize)` pair, before `qwBaseOffset` is applied — needed
/// only so [`Budget::alloc`]'s `Copy + Default` bound has something to build.
#[derive(Debug, Clone, Copy, Default)]
struct StdIndexEntryRaw {
    offset: u32,
    size: u32,
}

/// Resolve one stream's `OpenDML` super-index into [`StdIndexEntry`]s, seeking to
/// and reading each named `ix##` chunk in turn.
///
/// Bounded twice over: [`MAX_SUPER_INDEX_ENTRIES`] on how many `ix##` chunks are
/// followed, and `budget` on every allocation along the way. Never called for a
/// non-seekable source — resolving it means seeking to positions the super-index
/// names, which may be anywhere in the file.
pub(crate) fn resolve_opendml(
    io: &mut IoContext,
    super_index: &[SuperIndexEntry],
    budget: &mut Budget,
) -> Vec<StdIndexEntry> {
    let mut out = Vec::new();
    let resume = io.pos();
    for e in super_index.iter().take(MAX_SUPER_INDEX_ENTRIES) {
        let Ok(chunk) = read_chunk_at(io, e.offset, budget) else {
            continue;
        };
        if let Ok(entries) = parse_std_index(&chunk, budget) {
            out.extend(entries);
        }
    }
    let _ = io.seek(resume);
    out
}

/// Read one RIFF chunk's id and payload at an absolute position, without
/// trusting its declared size beyond what `budget` allows.
fn read_chunk_at(io: &mut IoContext, pos: u64, budget: &mut Budget) -> Result<Vec<u8>> {
    io.seek(pos)?;
    let _id = io.tag()?;
    let size = io.rl32()?;
    let n = usize::try_from(size).unwrap_or(usize::MAX);
    let mut buf = budget.alloc::<u8>(n)?;
    io.read_exact(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn entry(id: [u8; 4], flags: u32, offset: u32, size: u32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&id);
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out
    }

    #[test]
    fn idx1_parses_a_flat_entry_list() {
        let mut payload = entry(*b"00dc", AVIIF_KEYFRAME, 4, 100);
        payload.extend_from_slice(&entry(*b"01wb", 0, 200, 50));
        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::permissive());
        let entries = parse_idx1(&payload, &mut budget).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].chunk_id, *b"00dc");
        assert!(entries[0].flags & AVIIF_KEYFRAME != 0);
        assert_eq!(entries[1].offset, 200);
    }

    #[test]
    fn a_trailing_partial_entry_is_ignored() {
        let mut payload = entry(*b"00dc", AVIIF_KEYFRAME, 4, 100);
        payload.extend_from_slice(&[1, 2, 3]); // short trailing garbage
        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::permissive());
        let entries = parse_idx1(&payload, &mut budget).unwrap();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn parse_chunk_tag_splits_stream_and_kind() {
        assert_eq!(parse_chunk_tag(*b"00dc"), Some((0, *b"dc")));
        assert_eq!(parse_chunk_tag(*b"12wb"), Some((12, *b"wb")));
        // `OpenDML` `ix00` is correctly not mistaken for stream data.
        assert_eq!(parse_chunk_tag(*b"ix00"), None);
        assert_eq!(parse_chunk_tag(*b"idx1"), None);
    }

    #[test]
    fn offset_base_resolves_both_conventions() {
        let e = Idx1Entry {
            chunk_id: *b"00dc",
            flags: 0,
            offset: 4,
            size: 0,
        };
        assert_eq!(OffsetBase::MoviRelative.resolve(&e, 1000), 1004);
        assert_eq!(OffsetBase::Absolute.resolve(&e, 1000), 4);
    }

    #[test]
    fn super_index_parses_pointers_to_standard_index_chunks() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&4u16.to_le_bytes()); // wLongsPerEntry
        payload.push(0); // sub type
        payload.push(0); // index type: AVI_INDEX_OF_INDEXES
        payload.extend_from_slice(&2u32.to_le_bytes()); // nEntriesInUse
        payload.extend_from_slice(b"00dc"); // dwChunkId
        payload.extend_from_slice(&[0; 12]); // reserved
        payload.extend_from_slice(&5_000_000_000u64.to_le_bytes()); // qwOffset
        payload.extend_from_slice(&256u32.to_le_bytes()); // dwSize
        payload.extend_from_slice(&0u32.to_le_bytes()); // dwDuration
        payload.extend_from_slice(&6_000_000_000u64.to_le_bytes());
        payload.extend_from_slice(&512u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());

        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::permissive());
        let entries = parse_super_index(&payload, &mut budget).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].offset, 5_000_000_000);
        assert_eq!(entries[1].offset, 6_000_000_000);
    }

    #[test]
    fn standard_index_resolves_offsets_against_its_own_base_and_polarity() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&2u16.to_le_bytes());
        payload.push(0);
        payload.push(1); // AVI_INDEX_OF_CHUNKS
        payload.extend_from_slice(&2u32.to_le_bytes());
        payload.extend_from_slice(b"00dc");
        payload.extend_from_slice(&1000u64.to_le_bytes()); // qwBaseOffset
        payload.extend_from_slice(&0u32.to_le_bytes()); // reserved3
        payload.extend_from_slice(&8u32.to_le_bytes()); // dwOffset (keyframe)
        payload.extend_from_slice(&100u32.to_le_bytes()); // dwSize, high bit clear
        payload.extend_from_slice(&2000u32.to_le_bytes()); // dwOffset
        payload.extend_from_slice(&(0x0000_00c8_u32 | STD_INDEX_NOT_KEYFRAME_BIT).to_le_bytes());

        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::permissive());
        let entries = parse_std_index(&payload, &mut budget).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].pos, 1008);
        assert!(entries[0].is_key);
        assert_eq!(entries[1].pos, 3000);
        assert!(!entries[1].is_key);
    }

    #[test]
    fn a_declared_super_index_count_past_the_payload_is_clamped() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&4u16.to_le_bytes());
        payload.push(0);
        payload.push(0);
        payload.extend_from_slice(&1_000_000u32.to_le_bytes()); // lies: claims a million
        payload.extend_from_slice(b"00dc");
        payload.extend_from_slice(&[0; 12]);
        // Only one real entry follows.
        payload.extend_from_slice(&1u64.to_le_bytes());
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(&0u32.to_le_bytes());

        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::permissive());
        let entries = parse_super_index(&payload, &mut budget).unwrap();
        assert_eq!(entries.len(), 1);
    }
}
