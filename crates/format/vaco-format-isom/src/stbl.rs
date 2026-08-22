//! The sample tables, and the sample → (byte offset, size, timestamp) mapping
//! they compose into.
//!
//! ISO/IEC 14496-12 §8.6 and §8.7. This is the crate's centre of gravity: every
//! other module exists so that this one can answer, for an arbitrary sample
//! number and for an arbitrary decode time, *where in the file is it and when
//! does it play*.
//!
//! # The composition
//!
//! ```text
//! stsc   sample  -> chunk, and position within the chunk
//! stco   chunk   -> file offset
//! stsz   sample  -> size          (so the within-chunk offset is a running sum)
//! stts   sample  -> decode time   (run-length coded deltas)
//! ctts   sample  -> pts - dts     (run-length coded, v0 unsigned / v1 signed)
//! stss   sample  -> is it a sync sample (absent => every sample is)
//! ```
//!
//! Two access paths, deliberately separate:
//!
//! * [`SampleCursor`] — O(1) amortised forward iteration, for `read_packet`.
//! * [`SampleTable::sample`] and [`SampleTable::sample_at_dts`] — O(log n)
//!   random access, for `seek`. A seek does the second one repeatedly and must
//!   never walk from sample zero; that requirement is what shaped
//!   [`crate::table::RunIndex`].
//!
//! # Nothing here trusts the tables
//!
//! Every cross-reference is validated at the point of use rather than assumed:
//! a `stsc` run naming a chunk `stco` does not have yields `None`, a sample
//! index past `stsz` yields `None`, and a chunk offset plus a running size that
//! overflows `u64` yields `None`. Where a table is structurally impossible —
//! `stsc` whose `first_chunk` values do not increase — the table is rejected at
//! parse time, because every later answer derived from it would be a guess.

use vaco_core::{Error, Result};

use crate::boxes::{FullBox, IsoBox};
use crate::fourcc::boxes;
use crate::table::{EntryTable, RunIndex};

/// One sample, fully resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    /// Zero-based sample number within the track.
    pub index: u32,
    /// Absolute byte offset in the file.
    pub offset: u64,
    /// Length in bytes.
    pub size: u32,
    /// Decode timestamp, in media timescale ticks, before any edit-list or
    /// composition shift.
    pub dts: i64,
    /// `pts - dts`, from `ctts`. Zero when the track has no `ctts`.
    pub cts_offset: i32,
    /// This sample's `stts` delta.
    pub duration: u32,
    /// Whether decoding may start here.
    pub is_sync: bool,
    /// One-based chunk number, as `stsc` and `stco` count them.
    pub chunk: u32,
    /// One-based index into `stsd`.
    pub description_index: u32,
}

impl Sample {
    /// `dts + cts_offset`, saturating.
    #[must_use]
    pub const fn pts(&self) -> i64 {
        self.dts.saturating_add(self.cts_offset as i64)
    }

    /// One past the last byte of the sample, saturating.
    #[must_use]
    pub const fn end(&self) -> u64 {
        self.offset.saturating_add(self.size as u64)
    }
}

// ----------------------------------------------------------------- stts

/// `stts` — decode-time deltas, run-length coded (§8.6.1.2).
#[derive(Debug, Clone)]
pub struct TimeToSample<'a> {
    runs: EntryTable<'a>,
    index: RunIndex,
}

impl<'a> TimeToSample<'a> {
    /// Parse from a `stts` full-box body.
    #[must_use]
    pub fn parse(full: &FullBox<'a>) -> Self {
        let (declared, rest) = count_and_rest(full.body);
        let runs = EntryTable::new(rest, 8, declared);
        let index = RunIndex::build(runs.len(), |i| {
            (
                runs.get_u32(i, 0).unwrap_or(0),
                i64::from(runs.get_u32(i, 4).unwrap_or(0)),
            )
        });
        Self { runs, index }
    }

    /// An empty table: no samples, no time.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            runs: EntryTable::new(&[], 8, 0),
            index: RunIndex::default(),
        }
    }

    /// Samples the table accounts for.
    #[must_use]
    pub const fn sample_count(&self) -> u64 {
        self.index.total_samples()
    }

    /// Sum of every delta — the track's media duration as `stts` states it.
    #[must_use]
    pub const fn total_duration(&self) -> i64 {
        self.index.total_value()
    }

    /// Runs held.
    #[must_use]
    pub const fn runs(&self) -> u32 {
        self.runs.len()
    }

    fn run(&self, i: u32) -> Option<(u32, u32)> {
        Some((self.runs.get_u32(i, 0)?, self.runs.get_u32(i, 4)?))
    }

    fn last_delta(&self) -> u32 {
        self.runs
            .len()
            .checked_sub(1)
            .and_then(|i| self.run(i))
            .map_or(0, |(_, d)| d)
    }

    /// The run covering sample `n`, found by one binary search plus a bounded
    /// walk.
    ///
    /// Past the last run this reports a synthetic tail run of unbounded length
    /// carrying the final delta, which is what makes the extrapolation in
    /// [`TimeToSample::dts_and_duration`] and the cursor's carried position the
    /// same computation rather than two that have to be kept in step.
    fn seek_run(&self, n: u64) -> TimePosition {
        let mut cp = self.index.checkpoint_for_sample(n);
        loop {
            let Some((count, delta)) = self.run(cp.run) else {
                return TimePosition {
                    run: cp.run,
                    first_sample: self.index.total_samples(),
                    dts: self.index.total_value(),
                    count: u64::MAX,
                    delta: self.last_delta(),
                };
            };
            let next = cp.samples.saturating_add(u64::from(count));
            if n < next {
                return TimePosition {
                    run: cp.run,
                    first_sample: cp.samples,
                    dts: cp.value,
                    count: u64::from(count),
                    delta,
                };
            }
            cp.value = cp
                .value
                .saturating_add(i64::from(count).saturating_mul(i64::from(delta)));
            cp.samples = next;
            cp.run = cp.run.saturating_add(1);
        }
    }

    /// Move a carried position forward to cover sample `n`.
    ///
    /// `n` must not go backwards; the cursor guarantees that. Amortised O(1)
    /// across a whole track, because each run is entered exactly once.
    fn advance_to(&self, pos: &mut TimePosition, n: u64) {
        while n >= pos.first_sample.saturating_add(pos.count) {
            let Some((count, delta)) = self.run(pos.run) else {
                *pos = self.seek_run(n);
                return;
            };
            pos.dts = pos
                .dts
                .saturating_add(i64::from(count).saturating_mul(i64::from(delta)));
            pos.first_sample = pos.first_sample.saturating_add(u64::from(count));
            pos.run = pos.run.saturating_add(1);
            let Some((c, d)) = self.run(pos.run) else {
                pos.count = u64::MAX;
                pos.delta = self.last_delta();
                return;
            };
            pos.count = u64::from(c);
            pos.delta = d;
        }
    }

    /// Decode time and duration of sample `n`.
    ///
    /// Samples past the table's coverage are **extrapolated** with the last
    /// delta rather than refused. A `stts` shorter than `stsz` is a real and
    /// recoverable defect — the alternative is a track that reports 50 000
    /// samples and can time none of them.
    #[must_use]
    pub fn dts_and_duration(&self, n: u32) -> (i64, u32) {
        self.seek_run(u64::from(n)).at(u64::from(n))
    }

    /// The greatest sample number whose decode time is at or below `dts`.
    ///
    /// `None` when the table is empty or `dts` precedes sample zero.
    #[must_use]
    pub fn sample_at_or_before_dts(&self, dts: i64) -> Option<u32> {
        if self.index.total_samples() == 0 || dts < 0 {
            return None;
        }
        let mut cp = self.index.checkpoint_for_value(dts);
        loop {
            let Some((count, delta)) = self.run(cp.run) else {
                return u32::try_from(self.index.total_samples().saturating_sub(1)).ok();
            };
            let span = i64::from(count).saturating_mul(i64::from(delta));
            let end_value = cp.value.saturating_add(span);
            if dts < end_value || delta == 0 {
                let into = if delta == 0 {
                    0
                } else {
                    #[allow(
                        clippy::integer_division,
                        reason = "delta is proven non-zero on this branch"
                    )]
                    let q = dts.saturating_sub(cp.value) / i64::from(delta);
                    q.clamp(0, i64::from(count).saturating_sub(1))
                };
                let n = cp.samples.saturating_add(into.unsigned_abs());
                return u32::try_from(n.min(self.index.total_samples().saturating_sub(1))).ok();
            }
            cp.value = end_value;
            cp.samples = cp.samples.saturating_add(u64::from(count));
            cp.run = cp.run.saturating_add(1);
        }
    }
}

// ----------------------------------------------------------------- ctts

/// A carried position inside `stts`: which run, where it starts, and the
/// decode time at its first sample.
///
/// The cursor holds one of these so that stepping to the next sample is an
/// addition rather than a binary search. Past the last run, `count` is
/// `u64::MAX` and `delta` is the final delta, which makes extrapolation the
/// same arithmetic as interpolation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimePosition {
    run: u32,
    first_sample: u64,
    dts: i64,
    count: u64,
    delta: u32,
}

impl TimePosition {
    /// Decode time and duration of sample `n`, which must lie in this run.
    fn at(&self, n: u64) -> (i64, u32) {
        let into = n.saturating_sub(self.first_sample).cast_signed();
        (
            self.dts
                .saturating_add(into.saturating_mul(i64::from(self.delta))),
            self.delta,
        )
    }
}

/// `ctts` — composition offsets, run-length coded (§8.6.1.3).
///
/// Version 0 offsets are **unsigned**, version 1 **signed**. The distinction is
/// not cosmetic: it decides whether a track needs a negative DTS shift, and
/// getting it backwards moves every presentation timestamp on the track.
#[derive(Debug, Clone)]
pub struct CompositionOffsets<'a> {
    runs: EntryTable<'a>,
    index: RunIndex,
    version: u8,
    min_offset: i32,
    max_offset: i32,
}

impl<'a> CompositionOffsets<'a> {
    /// Parse from a `ctts` full box.
    #[must_use]
    pub fn parse(full: &FullBox<'a>) -> Self {
        let (declared, rest) = count_and_rest(full.body);
        let runs = EntryTable::new(rest, 8, declared);
        let version = full.version;
        let read = |i: u32| -> i32 {
            let raw = runs.get_u32(i, 4).unwrap_or(0);
            if version == 0 {
                // Unsigned in the file; values above i32::MAX are not
                // representable as an offset and are clamped rather than
                // wrapped into a large negative shift.
                i32::try_from(raw).unwrap_or(i32::MAX)
            } else {
                raw.cast_signed()
            }
        };
        let index = RunIndex::build(runs.len(), |i| (runs.get_u32(i, 0).unwrap_or(0), 0));
        let mut min_offset = i32::MAX;
        let mut max_offset = i32::MIN;
        for i in 0..runs.len() {
            let v = read(i);
            min_offset = min_offset.min(v);
            max_offset = max_offset.max(v);
        }
        if runs.is_empty() {
            min_offset = 0;
            max_offset = 0;
        }
        Self {
            runs,
            index,
            version,
            min_offset,
            max_offset,
        }
    }

    /// The `version` byte, which decides the sign of every offset.
    #[must_use]
    pub const fn version(&self) -> u8 {
        self.version
    }

    /// Smallest offset in the table, zero when it is empty.
    #[must_use]
    pub const fn min_offset(&self) -> i32 {
        self.min_offset
    }

    /// Largest offset in the table, zero when it is empty.
    #[must_use]
    pub const fn max_offset(&self) -> i32 {
        self.max_offset
    }

    /// Samples the table accounts for.
    #[must_use]
    pub const fn sample_count(&self) -> u64 {
        self.index.total_samples()
    }

    /// Runs held.
    #[must_use]
    pub const fn runs(&self) -> u32 {
        self.runs.len()
    }

    fn raw(&self, i: u32) -> i32 {
        let v = self.runs.get_u32(i, 4).unwrap_or(0);
        if self.version == 0 {
            i32::try_from(v).unwrap_or(i32::MAX)
        } else {
            v.cast_signed()
        }
    }

    /// `pts - dts` for sample `n`; zero past the table's coverage.
    #[must_use]
    pub fn offset(&self, n: u32) -> i32 {
        let n64 = u64::from(n);
        let mut cp = self.index.checkpoint_for_sample(n64);
        loop {
            let Some(count) = self.runs.get_u32(cp.run, 0) else {
                return 0;
            };
            let next = cp.samples.saturating_add(u64::from(count));
            if n64 < next {
                return self.raw(cp.run);
            }
            cp.samples = next;
            cp.run = cp.run.saturating_add(1);
        }
    }
}

// ----------------------------------------------------------------- stsc

/// Where a sample sits within the chunk layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkLocation {
    /// One-based chunk number.
    pub chunk: u32,
    /// Sample number of the chunk's first sample.
    pub first_sample: u64,
    /// Samples in this chunk.
    pub samples_per_chunk: u32,
    /// One-based `stsd` index for every sample in this chunk.
    pub description_index: u32,
}

/// `stsc` — sample-to-chunk runs (§8.7.4).
#[derive(Debug, Clone)]
pub struct SampleToChunk<'a> {
    runs: EntryTable<'a>,
    index: RunIndex,
    chunk_count: u32,
}

impl<'a> SampleToChunk<'a> {
    /// Parse from a `stsc` full box, given the chunk count from `stco`/`co64`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] when `first_chunk` does not start at 1 or does
    /// not strictly increase. Both make the run's extent undefined, and every
    /// sample offset derived from it would be invented rather than read
    /// (`planning/18-formats.md` §3.1.10).
    pub fn parse(full: &FullBox<'a>, chunk_count: u32) -> Result<Self> {
        let (declared, rest) = count_and_rest(full.body);
        let runs = EntryTable::new(rest, 12, declared);
        let mut previous: Option<u32> = None;
        for i in 0..runs.len() {
            let first = runs.get_u32(i, 0).unwrap_or(0);
            match previous {
                None if first != 1 => {
                    return Err(Error::InvalidData("isom: stsc does not start at chunk 1"));
                }
                Some(p) if first <= p => {
                    return Err(Error::InvalidData("isom: stsc first_chunk not increasing"));
                }
                _ => {}
            }
            previous = Some(first);
        }
        let index = Self::build_index(&runs, chunk_count);
        Ok(Self {
            runs,
            index,
            chunk_count,
        })
    }

    /// An empty table.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            runs: EntryTable::new(&[], 12, 0),
            index: RunIndex::default(),
            chunk_count: 0,
        }
    }

    fn build_index(runs: &EntryTable<'a>, chunk_count: u32) -> RunIndex {
        let n = runs.len();
        RunIndex::build(n, |i| {
            let first = runs.get_u32(i, 0).unwrap_or(1);
            let spc = runs.get_u32(i, 4).unwrap_or(0);
            let next = if i.saturating_add(1) < n {
                runs.get_u32(i.saturating_add(1), 0).unwrap_or(first)
            } else {
                chunk_count.saturating_add(1)
            };
            let chunks = next.saturating_sub(first);
            // Samples in the run can exceed u32; the index accumulates in u64,
            // so the count is clamped rather than wrapped.
            let samples = u64::from(chunks).saturating_mul(u64::from(spc));
            (u32::try_from(samples).unwrap_or(u32::MAX), 0)
        })
    }

    /// Chunks the table was built against.
    #[must_use]
    pub const fn chunk_count(&self) -> u32 {
        self.chunk_count
    }

    /// Samples the chunk layout accounts for.
    #[must_use]
    pub const fn sample_count(&self) -> u64 {
        self.index.total_samples()
    }

    /// Runs held.
    #[must_use]
    pub const fn runs(&self) -> u32 {
        self.runs.len()
    }

    fn run(&self, i: u32) -> Option<(u32, u32, u32)> {
        Some((
            self.runs.get_u32(i, 0)?,
            self.runs.get_u32(i, 4)?,
            self.runs.get_u32(i, 8)?,
        ))
    }

    fn run_chunks(&self, i: u32, first: u32) -> u32 {
        let next = if i.saturating_add(1) < self.runs.len() {
            self.runs.get_u32(i.saturating_add(1), 0).unwrap_or(first)
        } else {
            self.chunk_count.saturating_add(1)
        };
        next.saturating_sub(first)
    }

    /// Locate sample `n`, or `None` when the chunk layout does not reach it.
    #[must_use]
    pub fn locate(&self, n: u64) -> Option<ChunkLocation> {
        let mut cp = self.index.checkpoint_for_sample(n);
        loop {
            let (first, spc, sdi) = self.run(cp.run)?;
            let chunks = self.run_chunks(cp.run, first);
            let in_run = u64::from(chunks).saturating_mul(u64::from(spc));
            let next = cp.samples.saturating_add(in_run);
            if n < next && spc > 0 {
                let into = n.saturating_sub(cp.samples);
                #[allow(
                    clippy::integer_division,
                    reason = "spc is proven non-zero on this branch"
                )]
                let chunks_into = into / u64::from(spc);
                let chunk = u64::from(first).saturating_add(chunks_into);
                return Some(ChunkLocation {
                    chunk: u32::try_from(chunk).ok()?,
                    first_sample: cp
                        .samples
                        .saturating_add(chunks_into.saturating_mul(u64::from(spc))),
                    samples_per_chunk: spc,
                    description_index: sdi,
                });
            }
            cp.samples = next;
            cp.run = cp.run.saturating_add(1);
        }
    }
}

// ----------------------------------------------------------------- stsz

/// How `stsz`/`stz2` stores its sizes.
#[derive(Debug, Clone, Copy)]
enum SizeStorage<'a> {
    /// `stsz` with a non-zero `sample_size`: every sample is the same length.
    Uniform(u32),
    /// `stz2` with `field_size == 4`: two samples per byte, high nibble first.
    Bits4(&'a [u8]),
    /// `stz2` with `field_size == 8`.
    Bits8(&'a [u8]),
    /// `stz2` with `field_size == 16`.
    Bits16(&'a [u8]),
    /// `stsz` with `sample_size == 0`: one `u32` per sample.
    Bits32(&'a [u8]),
}

/// `stsz` or `stz2` — sample sizes (§8.7.3).
#[derive(Debug, Clone)]
pub struct SampleSizes<'a> {
    storage: SizeStorage<'a>,
    count: u32,
    /// Cumulative byte offsets, decimated. Absent for a uniform table, where
    /// the cumulative sum is a multiplication.
    prefix: Option<RunIndex>,
}

impl<'a> SampleSizes<'a> {
    /// Parse a `stsz` full box.
    #[must_use]
    pub fn parse_stsz(full: &FullBox<'a>) -> Self {
        let mut r = vaco_bitstream::ByteReader::new(full.body);
        let sample_size = r.be32();
        let declared = r.be32();
        let rest = full.body.get(8..).unwrap_or(&[]);
        if sample_size != 0 {
            return Self::uniform(sample_size, declared);
        }
        Self::variable(SizeStorage::Bits32(rest), clamp(declared, rest.len(), 4))
    }

    /// Parse a `stz2` full box.
    ///
    /// `field_size` is 4, 8 or 16 per §8.7.3.3; anything else yields an empty
    /// table rather than a guessed stride.
    #[must_use]
    pub fn parse_stz2(full: &FullBox<'a>) -> Self {
        let mut r = vaco_bitstream::ByteReader::new(full.body);
        let _reserved = r.be24();
        let field_size = r.u8();
        let declared = r.be32();
        let rest = full.body.get(8..).unwrap_or(&[]);
        match field_size {
            4 => {
                let cap = rest.len().saturating_mul(2);
                Self::variable(
                    SizeStorage::Bits4(rest),
                    declared.min(u32::try_from(cap).unwrap_or(u32::MAX)),
                )
            }
            8 => Self::variable(SizeStorage::Bits8(rest), clamp(declared, rest.len(), 1)),
            16 => Self::variable(SizeStorage::Bits16(rest), clamp(declared, rest.len(), 2)),
            _ => Self::uniform(0, 0),
        }
    }

    /// A table where every sample is `size` bytes.
    ///
    /// # The one count this crate cannot clamp
    ///
    /// Every other table bounds its declared count against its payload, because
    /// every other table has one. A `stsz` with a non-zero `sample_size` has
    /// **no per-sample payload at all** — twelve bytes of header can legally
    /// declare `sample_count = 4_294_967_295`, and that is a real description
    /// of a real (if enormous) track, not a corruption.
    ///
    /// Nothing here allocates for it: a uniform table needs no prefix index and
    /// its cumulative sum is a multiplication. But
    /// [`SampleTable::sample_count`] will report four billion, and a demuxer
    /// that iterates without checking its reads will iterate four billion
    /// times. **Bounding that is the demuxer's job**, and it happens naturally:
    /// the chunk offsets such a file carries point past its own end, so the
    /// first read fails.
    ///
    /// Found by the `isom_sample_table` fuzz target in 25 executions, against
    /// an assertion that claimed the cursor could not yield 2^20 samples.
    #[must_use]
    pub fn uniform(size: u32, count: u32) -> Self {
        Self {
            storage: SizeStorage::Uniform(size),
            count,
            prefix: None,
        }
    }

    fn variable(storage: SizeStorage<'a>, count: u32) -> Self {
        let read = |i: u32| Self::read_at(storage, i, count);
        let prefix = RunIndex::build(count, |i| (1, i64::from(read(i))));
        Self {
            storage,
            count,
            prefix: Some(prefix),
        }
    }

    fn read_at(storage: SizeStorage<'a>, i: u32, count: u32) -> u32 {
        if i >= count {
            return 0;
        }
        let i = i as usize;
        match storage {
            SizeStorage::Uniform(s) => s,
            SizeStorage::Bits4(d) => {
                #[allow(
                    clippy::integer_division,
                    reason = "two four-bit fields per byte; the divisor is the literal 2"
                )]
                let byte = d.get(i / 2).copied().unwrap_or(0);
                if i.is_multiple_of(2) {
                    u32::from(byte >> 4)
                } else {
                    u32::from(byte & 0x0F)
                }
            }
            SizeStorage::Bits8(d) => u32::from(d.get(i).copied().unwrap_or(0)),
            SizeStorage::Bits16(d) => d
                .get(i.saturating_mul(2)..)
                .and_then(<[u8]>::first_chunk::<2>)
                .map_or(0, |b| u32::from(u16::from_be_bytes(*b))),
            SizeStorage::Bits32(d) => d
                .get(i.saturating_mul(4)..)
                .and_then(<[u8]>::first_chunk::<4>)
                .map_or(0, |b| u32::from_be_bytes(*b)),
        }
    }

    /// Samples the table describes.
    #[must_use]
    pub const fn count(&self) -> u32 {
        self.count
    }

    /// Whether every sample is the same size.
    #[must_use]
    pub const fn is_uniform(&self) -> bool {
        matches!(self.storage, SizeStorage::Uniform(_))
    }

    /// Size of sample `n`, or `None` past the end.
    #[must_use]
    pub fn size(&self, n: u32) -> Option<u32> {
        if n >= self.count {
            return None;
        }
        Some(Self::read_at(self.storage, n, self.count))
    }

    /// Total bytes of the first `n` samples.
    ///
    /// The within-chunk offset of a sample is the difference of two of these,
    /// which is why it exists at all.
    #[must_use]
    pub fn cumulative(&self, n: u32) -> u64 {
        let n = n.min(self.count);
        match (&self.storage, &self.prefix) {
            (SizeStorage::Uniform(s), _) => u64::from(n).saturating_mul(u64::from(*s)),
            (_, Some(prefix)) => {
                let cp = prefix.checkpoint_for_sample(u64::from(n));
                let mut total = cp.value.max(0) as u64;
                let mut at = u32::try_from(cp.samples).unwrap_or(u32::MAX);
                while at < n {
                    total = total.saturating_add(u64::from(Self::read_at(
                        self.storage,
                        at,
                        self.count,
                    )));
                    at = at.saturating_add(1);
                }
                total
            }
            (_, None) => {
                let mut total = 0u64;
                for i in 0..n {
                    total =
                        total.saturating_add(u64::from(Self::read_at(self.storage, i, self.count)));
                }
                total
            }
        }
    }
}

fn clamp(declared: u32, bytes: usize, stride: usize) -> u32 {
    if stride == 0 {
        return 0;
    }
    #[allow(
        clippy::integer_division,
        reason = "stride is proven non-zero immediately above"
    )]
    let cap = bytes / stride;
    declared.min(u32::try_from(cap).unwrap_or(u32::MAX))
}

// ----------------------------------------------------------------- stco

/// `stco` or `co64` — chunk offsets (§8.7.5).
#[derive(Debug, Clone, Copy)]
pub struct ChunkOffsets<'a> {
    table: EntryTable<'a>,
    wide: bool,
}

impl<'a> ChunkOffsets<'a> {
    /// Parse a `stco` (32-bit) full box.
    #[must_use]
    pub fn parse_stco(full: &FullBox<'a>) -> Self {
        let (declared, rest) = count_and_rest(full.body);
        Self {
            table: EntryTable::new(rest, 4, declared),
            wide: false,
        }
    }

    /// Parse a `co64` (64-bit) full box.
    #[must_use]
    pub fn parse_co64(full: &FullBox<'a>) -> Self {
        let (declared, rest) = count_and_rest(full.body);
        Self {
            table: EntryTable::new(rest, 8, declared),
            wide: true,
        }
    }

    /// An empty table.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            table: EntryTable::new(&[], 4, 0),
            wide: false,
        }
    }

    /// Chunks held.
    #[must_use]
    pub const fn len(&self) -> u32 {
        self.table.len()
    }

    /// Whether the table has no chunks.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    /// Whether offsets are 64-bit (`co64`).
    #[must_use]
    pub const fn is_wide(&self) -> bool {
        self.wide
    }

    /// File offset of chunk `chunk`, which is **one-based** as `stsc` counts.
    #[must_use]
    pub fn offset(&self, chunk: u32) -> Option<u64> {
        let i = chunk.checked_sub(1)?;
        if self.wide {
            self.table.get_u64(i, 0)
        } else {
            self.table.get_u32(i, 0).map(u64::from)
        }
    }
}

// ----------------------------------------------------------------- stss

/// `stss` — sync samples (§8.6.2).
///
/// Sample numbers are **one-based** in the file. Absence of the box means every
/// sample is a sync sample, which is why [`SampleTable::is_sync`] treats
/// `None` and "present but empty" differently.
#[derive(Debug, Clone, Copy)]
pub struct SyncSamples<'a> {
    table: EntryTable<'a>,
}

impl<'a> SyncSamples<'a> {
    /// Parse a `stss` full box.
    #[must_use]
    pub fn parse(full: &FullBox<'a>) -> Self {
        let (declared, rest) = count_and_rest(full.body);
        Self {
            table: EntryTable::new(rest, 4, declared),
        }
    }

    /// Entries held.
    #[must_use]
    pub const fn len(&self) -> u32 {
        self.table.len()
    }

    /// Whether the table lists no sync samples.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    /// The one-based sample number at entry `i`.
    #[must_use]
    pub fn entry(&self, i: u32) -> Option<u32> {
        self.table.get_u32(i, 0)
    }

    /// Position of the first entry whose value is at or above `want`.
    ///
    /// `want` is a **one-based** sample number widened to `u64`, which is not
    /// fussiness: the zero-based sample `u32::MAX` has one-based number 2^32,
    /// which no `stss` entry can hold. Computing `n + 1` in `u32` saturated
    /// instead, so `at_or_after(u32::MAX)` found the entry for sample
    /// `u32::MAX - 1` and reported a sync sample *before* the one asked for.
    /// Found by the `isom_sample_table` and `isom_file` fuzz targets within
    /// thirty executions of each.
    ///
    /// Binary search assumes the table is ascending, which §8.6.2 requires. A
    /// descending or shuffled table is not detected — it yields a wrong-but-
    /// bounded answer, never a panic — because verifying the order costs a full
    /// scan on every open and the payoff is a better answer for a file that is
    /// already corrupt.
    fn position(&self, want: u64) -> u32 {
        let (mut lo, mut hi) = (0u32, self.table.len());
        while lo < hi {
            #[allow(
                clippy::integer_division,
                reason = "bisection midpoint; the divisor is the literal 2"
            )]
            let mid = lo.saturating_add((hi.saturating_sub(lo)) / 2);
            if u64::from(self.entry(mid).unwrap_or(u32::MAX)) < want {
                lo = mid.saturating_add(1);
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// Whether the zero-based sample `n` is listed.
    #[must_use]
    pub fn contains(&self, n: u32) -> bool {
        let Some(want) = n.checked_add(1) else {
            return false;
        };
        self.entry(self.position(u64::from(want))) == Some(want)
    }

    /// The greatest listed sync sample at or before `n`, zero-based.
    #[must_use]
    pub fn at_or_before(&self, n: u32) -> Option<u32> {
        let want = u64::from(n).saturating_add(1);
        let at = self.position(want);
        if self.entry(at).is_some_and(|e| u64::from(e) == want) {
            return Some(n);
        }
        self.entry(at.checked_sub(1)?)?.checked_sub(1)
    }

    /// The least listed sync sample at or after `n`, zero-based.
    #[must_use]
    pub fn at_or_after(&self, n: u32) -> Option<u32> {
        let want = u64::from(n).saturating_add(1);
        self.entry(self.position(want))?.checked_sub(1)
    }
}

// ----------------------------------------------------------------- cslg

/// `cslg` — composition-to-decode shift (§8.6.1.4).
///
/// When present it *states* the shift a demuxer would otherwise have to infer
/// from the `ctts` extremes, so it is always preferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompositionToDecode {
    /// Value which, added to every composition time, makes them all at or above
    /// their decode times.
    pub composition_to_dts_shift: i64,
    /// Smallest `pts - dts` in the track.
    pub least_offset: i64,
    /// Largest `pts - dts` in the track.
    pub greatest_offset: i64,
    /// Earliest composition time of any sample.
    pub start_time: i64,
    /// End composition time of the track.
    pub end_time: i64,
}

impl CompositionToDecode {
    /// Parse a `cslg` full box. Version 0 fields are 32-bit, version 1 64-bit.
    #[must_use]
    pub fn parse(full: &FullBox<'_>) -> Option<Self> {
        let mut r = vaco_bitstream::ByteReader::new(full.body);
        let mut next = || -> i64 {
            if full.version == 0 {
                i64::from(r.be32().cast_signed())
            } else {
                r.be64().cast_signed()
            }
        };
        let me = Self {
            composition_to_dts_shift: next(),
            least_offset: next(),
            greatest_offset: next(),
            start_time: next(),
            end_time: next(),
        };
        r.check().ok()?;
        Some(me)
    }
}

// ----------------------------------------------------------------- stbl

/// Every table in one `stbl`, plus the derived quantities a demuxer needs.
#[derive(Debug, Clone)]
pub struct SampleTable<'a> {
    /// The raw `stsd` payload, parsed on demand by [`crate::stsd`].
    pub sample_descriptions: Option<IsoBox<'a>>,
    /// `stts`.
    pub time_to_sample: TimeToSample<'a>,
    /// `ctts`, when present.
    pub composition_offsets: Option<CompositionOffsets<'a>>,
    /// `cslg`, when present.
    pub composition_to_decode: Option<CompositionToDecode>,
    /// `stss`; `None` means every sample is a sync sample.
    pub sync_samples: Option<SyncSamples<'a>>,
    /// `stsc`.
    pub sample_to_chunk: SampleToChunk<'a>,
    /// `stsz` or `stz2`.
    pub sample_sizes: SampleSizes<'a>,
    /// `stco` or `co64`.
    pub chunk_offsets: ChunkOffsets<'a>,
    /// `sdtp` payload — one byte of dependency flags per sample.
    pub dependency_flags: Option<EntryTable<'a>>,
}

impl<'a> SampleTable<'a> {
    /// A table with nothing in it, for a track whose `stbl` could not be used.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            sample_descriptions: None,
            time_to_sample: TimeToSample::empty(),
            composition_offsets: None,
            composition_to_decode: None,
            sync_samples: None,
            sample_to_chunk: SampleToChunk::empty(),
            sample_sizes: SampleSizes::uniform(0, 0),
            chunk_offsets: ChunkOffsets::empty(),
            dependency_flags: None,
        }
    }

    /// Parse a `stbl` container.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] for a malformed child box or a `stsc` whose runs
    /// are not increasing.
    pub fn parse(stbl: &IsoBox<'a>) -> Result<Self> {
        // Two passes: the chunk table must be known before `stsc` can be
        // interpreted, because the last `stsc` run's extent is "to the last
        // chunk" and nothing else states where that is.
        let mut chunk_offsets = ChunkOffsets::empty();
        let mut stsc_box: Option<IsoBox<'a>> = None;
        let mut me = Self {
            sample_descriptions: None,
            time_to_sample: TimeToSample::empty(),
            composition_offsets: None,
            composition_to_decode: None,
            sync_samples: None,
            sample_to_chunk: SampleToChunk::empty(),
            sample_sizes: SampleSizes::uniform(0, 0),
            chunk_offsets: ChunkOffsets::empty(),
            dependency_flags: None,
        };
        for child in stbl.children() {
            let child = child?;
            match child.kind() {
                boxes::STSD => me.sample_descriptions = Some(child),
                boxes::STTS => me.time_to_sample = TimeToSample::parse(&child.full()?),
                boxes::CTTS => {
                    me.composition_offsets = Some(CompositionOffsets::parse(&child.full()?));
                }
                boxes::CSLG => {
                    me.composition_to_decode = CompositionToDecode::parse(&child.full()?);
                }
                boxes::STSS => me.sync_samples = Some(SyncSamples::parse(&child.full()?)),
                boxes::STSC => stsc_box = Some(child),
                boxes::STSZ => me.sample_sizes = SampleSizes::parse_stsz(&child.full()?),
                boxes::STZ2 => me.sample_sizes = SampleSizes::parse_stz2(&child.full()?),
                boxes::STCO => chunk_offsets = ChunkOffsets::parse_stco(&child.full()?),
                boxes::CO64 => chunk_offsets = ChunkOffsets::parse_co64(&child.full()?),
                boxes::SDTP => {
                    let full = child.full()?;
                    me.dependency_flags = Some(EntryTable::new(full.body, 1, u32::MAX));
                }
                _ => {}
            }
        }
        me.chunk_offsets = chunk_offsets;
        if let Some(b) = stsc_box {
            me.sample_to_chunk = SampleToChunk::parse(&b.full()?, chunk_offsets.len())?;
        }
        Ok(me)
    }

    /// Samples in the track.
    ///
    /// Taken from `stsz`, which is the table `ffprobe` reports as `nb_frames`
    /// — measured on `ffprobe 8.1`, and it holds even when `mdat` is short of
    /// what the table promises.
    #[must_use]
    pub const fn sample_count(&self) -> u32 {
        self.sample_sizes.count()
    }

    /// Chunks in the track.
    #[must_use]
    pub const fn chunk_count(&self) -> u32 {
        self.chunk_offsets.len()
    }

    /// Sum of the `stts` deltas — the media duration the sample table implies.
    #[must_use]
    pub const fn total_duration(&self) -> i64 {
        self.time_to_sample.total_duration()
    }

    /// Whether sample `n` may be decoded from cold.
    ///
    /// An absent `stss` means every sample qualifies (§8.6.2); a *present but
    /// empty* `stss` means none do, and the two must not be conflated.
    #[must_use]
    pub fn is_sync(&self, n: u32) -> bool {
        self.sync_samples.as_ref().is_none_or(|s| s.contains(n))
    }

    /// The greatest sync sample at or before `n`.
    #[must_use]
    pub fn sync_at_or_before(&self, n: u32) -> Option<u32> {
        match &self.sync_samples {
            None => (n < self.sample_count()).then_some(n),
            Some(s) => s.at_or_before(n),
        }
    }

    /// The least sync sample at or after `n`.
    #[must_use]
    pub fn sync_at_or_after(&self, n: u32) -> Option<u32> {
        match &self.sync_samples {
            None => (n < self.sample_count()).then_some(n),
            Some(s) => s.at_or_after(n).filter(|&i| i < self.sample_count()),
        }
    }

    /// Decode time of sample `n`, before any shift.
    #[must_use]
    pub fn dts(&self, n: u32) -> i64 {
        self.time_to_sample.dts_and_duration(n).0
    }

    /// `pts - dts` for sample `n`.
    #[must_use]
    pub fn cts_offset(&self, n: u32) -> i32 {
        self.composition_offsets.as_ref().map_or(0, |c| c.offset(n))
    }

    /// The DTS shift a demuxer should apply to the whole track.
    ///
    /// # D17
    ///
    /// ISO/IEC 14496-12 §8.6.1.4 defines `cslg.compositionToDTSShift` as a
    /// value **added to composition times** so that every composition time is
    /// at or above its decode time. `ffmpeg`/`ffprobe` 8.1 instead **subtracts**
    /// the equivalent quantity from every decode time, leaving presentation
    /// times anchored where the file put them. Measured on an MP4 written with
    /// `-movflags +negative_cts_offsets` (`ctts` version 1, offsets
    /// `0, 1024, -512, …`, `elst` `media_time = 0`):
    ///
    /// ```text
    /// ffprobe -show_packets  ->  pts=0 dts=-512   pts=1536 dts=0   pts=512 dts=512
    /// ```
    ///
    /// so the applied shift is `min(ctts) = -512`, applied to DTS only. The
    /// spec's reading would instead have produced `pts=512 dts=0`. Both express
    /// the same `pts - dts` relationship; only one matches what
    /// `-show_packets` prints, and D6 makes that the contract. **This must not
    /// be "corrected" to the spec's sign convention.**
    ///
    /// The value returned is therefore `min(0, least_offset)`, taken from
    /// `cslg` when present and from the `ctts` extremes otherwise. A `ctts`
    /// version 0 table cannot carry a negative offset, so such a track always
    /// gets zero — confirmed against a `-bf 2` MP4 whose `ctts` v0 minimum is
    /// 512 and whose reported DTS shift came entirely from its edit list.
    #[must_use]
    pub fn dts_shift(&self) -> i64 {
        if let Some(c) = self.composition_to_decode {
            return c.least_offset.min(0);
        }
        self.composition_offsets
            .as_ref()
            .map_or(0, |c| i64::from(c.min_offset()).min(0))
    }

    /// Byte offset of sample `n`, or `None` when the chunk layout cannot place
    /// it.
    #[must_use]
    pub fn offset(&self, n: u32) -> Option<u64> {
        let loc = self.sample_to_chunk.locate(u64::from(n))?;
        let base = self.chunk_offsets.offset(loc.chunk)?;
        let first = u32::try_from(loc.first_sample).ok()?;
        let within = self
            .sample_sizes
            .cumulative(n)
            .checked_sub(self.sample_sizes.cumulative(first))?;
        base.checked_add(within)
    }

    /// Fully resolve sample `n` — the crate's headline operation.
    ///
    /// `None` when `n` is past `stsz`, or when `stsc`/`stco` cannot place it.
    /// Both are ordinary outcomes on a damaged file, never errors: a track
    /// whose `mdat` was truncated still reports its stream, its codec and its
    /// frame count.
    #[must_use]
    pub fn sample(&self, n: u32) -> Option<Sample> {
        let size = self.sample_sizes.size(n)?;
        let loc = self.sample_to_chunk.locate(u64::from(n))?;
        let base = self.chunk_offsets.offset(loc.chunk)?;
        let first = u32::try_from(loc.first_sample).ok()?;
        let within = self
            .sample_sizes
            .cumulative(n)
            .checked_sub(self.sample_sizes.cumulative(first))?;
        let (dts, duration) = self.time_to_sample.dts_and_duration(n);
        Some(Sample {
            index: n,
            offset: base.checked_add(within)?,
            size,
            dts,
            cts_offset: self.cts_offset(n),
            duration,
            is_sync: self.is_sync(n),
            chunk: loc.chunk,
            description_index: loc.description_index,
        })
    }

    /// The greatest sample whose DTS is at or below `dts`.
    #[must_use]
    pub fn sample_at_dts(&self, dts: i64) -> Option<u32> {
        let n = self.time_to_sample.sample_at_or_before_dts(dts)?;
        Some(n.min(self.sample_count().saturating_sub(1)))
    }

    /// A cursor for forward iteration from sample zero.
    #[must_use]
    pub fn cursor(&self) -> SampleCursor<'_, 'a> {
        SampleCursor::new(self, 0)
    }

    /// A cursor positioned at sample `n`.
    ///
    /// Positioning costs one random access; iteration after it is O(1) per
    /// sample. This is exactly the shape a seek wants.
    #[must_use]
    pub fn cursor_at(&self, n: u32) -> SampleCursor<'_, 'a> {
        SampleCursor::new(self, n)
    }

    /// The one-byte `sdtp` dependency flags for sample `n`, when the box is
    /// present.
    #[must_use]
    pub fn dependency_flags(&self, n: u32) -> Option<u8> {
        self.dependency_flags
            .as_ref()
            .and_then(|t| t.entry(n))
            .and_then(|e| e.first().copied())
    }
}

/// Forward iteration over a [`SampleTable`].
///
/// Amortised O(1) per sample: the `stts` run, the chunk, and the within-chunk
/// byte offset are all carried, so `next` is a table read and three additions
/// rather than three binary searches.
///
/// # The contract, which is not "stops at the first problem"
///
/// The cursor yields **every sample [`SampleTable::sample`] can resolve, in
/// index order**, and skips the ones it cannot — a chunk whose `stco` entry is
/// missing, or one whose offset plus the running size overflows `u64`. It stops
/// only when the chunk layout no longer covers the index at all, which is
/// monotone: `stsc` runs out once and stays out.
///
/// The first version stopped at the first unresolvable sample instead, and the
/// `isom_sample_table` fuzz target refuted it in 27 executions with a `co64`
/// whose first chunk offset was `u64::MAX`: sample 0 resolved, samples 1..43
/// overflowed, and sample 44 — in the *second* chunk, at a perfectly ordinary
/// offset — was reachable by random access and not by the cursor. One bad chunk
/// offset must cost one chunk, not the rest of the track, which is also what
/// `planning/18-formats.md` §3.1.10 says about samples past the end of `mdat`.
///
/// # Why skipping is bounded
///
/// A sample fails to resolve for exactly two reasons, and they are bounded
/// differently:
///
/// * **The chunk has no `stco`/`co64` entry.** Chunk numbers are non-decreasing
///   in the sample index — within a `stsc` run by construction, and across runs
///   because `first_chunk` is required to increase — so once one chunk is past
///   the end of the offset table, every later one is too. That is the **end**
///   of the iteration, not a hole.
/// * **`chunk_offset + within` overflows `u64`.** `within` only grows inside a
///   chunk, so this is monotone *within* the chunk but says nothing about the
///   next one. The cursor jumps to the next chunk's first sample in one step.
///
/// Both paths are therefore bounded by the number of chunks, which is bounded
/// by the `stco` payload. The fuzzer is what established this: a first version
/// stopped at the first hole (refuted in 27 executions), a second skipped one
/// sample at a time and a `stsc` declaring 4.2 billion single-sample chunks
/// with no offsets took **13.8 seconds** on a 78-byte input, which libFuzzer
/// filed as a `slow-unit` and `cargo fuzz` exited zero on. Timing the target's
/// sections was what localised it; the parse and every point query were under
/// 400 µs and the cursor was all of the rest.
#[derive(Debug, Clone)]
pub struct SampleCursor<'t, 'a> {
    table: &'t SampleTable<'a>,
    index: u32,
    /// Cached location of the chunk `index` falls in.
    loc: Option<ChunkLocation>,
    /// Running byte offset of `index` within its chunk.
    within: u64,
    /// Carried `stts` run position.
    time: TimePosition,
}

impl<'t, 'a> SampleCursor<'t, 'a> {
    fn new(table: &'t SampleTable<'a>, at: u32) -> Self {
        let mut me = Self {
            table,
            index: at,
            loc: None,
            within: 0,
            time: table.time_to_sample.seek_run(u64::from(at)),
        };
        me.reposition(at);
        me
    }

    fn reposition(&mut self, n: u32) {
        self.index = n;
        self.time = self.table.time_to_sample.seek_run(u64::from(n));
        self.loc = self.table.sample_to_chunk.locate(u64::from(n));
        self.within = match self.loc {
            Some(loc) => u32::try_from(loc.first_sample).map_or(0, |first| {
                self.table
                    .sample_sizes
                    .cumulative(n)
                    .saturating_sub(self.table.sample_sizes.cumulative(first))
            }),
            None => 0,
        };
    }

    /// The sample number the cursor will return next.
    #[must_use]
    pub const fn position(&self) -> u32 {
        self.index
    }

    /// Jump to sample `n`, paying one random access.
    pub fn seek(&mut self, n: u32) {
        self.reposition(n);
    }
}

impl Iterator for SampleCursor<'_, '_> {
    type Item = Sample;

    fn next(&mut self) -> Option<Sample> {
        loop {
            let n = self.index;
            let size = self.table.sample_sizes.size(n)?;
            // A `locate` failure is the end, not a hole: it means the chunk
            // layout has run out, and that is monotone in `n`.
            let loc = self.loc?;
            self.table
                .time_to_sample
                .advance_to(&mut self.time, u64::from(n));
            let (dts, duration) = self.time.at(u64::from(n));
            // A chunk with no offset entry is the end: chunk numbers only
            // increase, so no later sample can have one either.
            let Some(base) = self.table.chunk_offsets.offset(loc.chunk) else {
                self.loc = None;
                return None;
            };
            // An offset that overflows `u64` *is* a hole: the next chunk may be
            // perfectly fine.
            let out = base.checked_add(self.within).map(|offset| Sample {
                index: n,
                offset,
                size,
                dts,
                cts_offset: self.table.cts_offset(n),
                duration,
                is_sync: self.table.is_sync(n),
                chunk: loc.chunk,
                description_index: loc.description_index,
            });
            // Advance, whether or not the sample resolved. `size(n)` returning
            // `Some` proves `n < sample_count <= u32::MAX`, so the saturating
            // add is a real increment and iteration terminates.
            self.index = n.saturating_add(1);
            let next_in_chunk = u64::from(self.index).saturating_sub(loc.first_sample);
            if next_in_chunk >= u64::from(loc.samples_per_chunk) {
                self.loc = self.table.sample_to_chunk.locate(u64::from(self.index));
                self.within = 0;
            } else {
                self.within = self.within.saturating_add(u64::from(size));
            }
            if out.is_some() {
                return out;
            }
            // Nothing in the rest of this chunk can resolve either, so jump to
            // the next one instead of walking a chunk that may hold billions
            // of samples.
            let next_chunk_start = loc
                .first_sample
                .saturating_add(u64::from(loc.samples_per_chunk));
            if next_chunk_start > u64::from(self.index) {
                self.index = u32::try_from(next_chunk_start).unwrap_or(u32::MAX);
                self.loc = self.table.sample_to_chunk.locate(u64::from(self.index));
                self.within = 0;
            }
        }
    }
}

/// Read a `u32` count and return it with the bytes that follow.
fn count_and_rest(body: &[u8]) -> (u32, &[u8]) {
    let count = body
        .first_chunk::<4>()
        .map_or(0, |b| u32::from_be_bytes(*b));
    (count, body.get(4..).unwrap_or(&[]))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::integer_division,
    reason = "test code"
)]
mod tests {
    use super::*;
    use crate::testutil::{self, StblSpec};

    /// The measured layout of `prog.mp4`'s video track — the file this crate
    /// was calibrated against. See `docs/format/vaco-format-isom.md`.
    fn measured_video() -> StblSpec {
        StblSpec {
            stts: vec![(50, 512)],
            ctts_v0: vec![(1, 1024), (1, 2048), (2, 512), (1, 2048), (2, 512)],
            stss: vec![1, 16, 31, 46],
            stsc: vec![(1, 2, 1), (2, 1, 1)],
            // The first eight sizes and chunk offsets are the file's own; the
            // rest are padding so the table covers all 50 samples and 49
            // chunks, which is what makes the sync-sample queries meaningful.
            stsz: {
                let mut v = vec![4822, 1668, 1011, 629, 1744, 1081, 778, 2021];
                v.extend((8..50).map(|i| 700 + i * 3));
                v
            },
            stco: {
                let mut v = vec![3017, 9765, 11181, 12226, 14400, 15652, 16810, 19310];
                v.extend((8..49).map(|i| 19310 + (i - 7) * 2500));
                v
            },
            ..StblSpec::default()
        }
    }

    #[test]
    fn sample_offsets_match_the_reference_byte_for_byte() {
        // ffprobe -show_packets on prog.mp4 reported pos = 3017, 7839, 9765,
        // 11181, 12226 for the first five video samples. Chunk 1 holds two
        // samples, so sample 1 sits at 3017 + 4822.
        let raw = testutil::stbl(&measured_video());
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        let offs: Vec<u64> = (0..5).map(|i| t.offset(i).unwrap()).collect();
        assert_eq!(offs, vec![3017, 7839, 9765, 11181, 12226]);
    }

    #[test]
    fn timestamps_match_the_reference() {
        let raw = testutil::stbl(&measured_video());
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        // dts = n * 512, pts = dts + ctts.
        let pts: Vec<i64> = (0..4).map(|i| t.sample(i).unwrap().pts()).collect();
        assert_eq!(pts, vec![1024, 2560, 1536, 2048]);
        let dts: Vec<i64> = (0..4).map(|i| t.sample(i).unwrap().dts).collect();
        assert_eq!(dts, vec![0, 512, 1024, 1536]);
        // ctts version 0 offsets are all positive, so no DTS shift is implied.
        assert_eq!(t.dts_shift(), 0);
    }

    #[test]
    fn sync_samples_are_one_based_in_the_file_and_zero_based_out() {
        let raw = testutil::stbl(&measured_video());
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        assert!(t.is_sync(0));
        assert!(!t.is_sync(1));
        assert!(t.is_sync(15));
        assert_eq!(t.sync_at_or_before(20), Some(15));
        assert_eq!(t.sync_at_or_after(20), Some(30));
        assert_eq!(t.sync_at_or_before(0), Some(0));
        assert_eq!(t.sync_at_or_after(46), None);
    }

    #[test]
    fn sync_queries_at_the_top_of_the_index_space_do_not_go_backwards() {
        // The zero-based sample u32::MAX has one-based number 2^32, which no
        // `stss` entry can hold. Computing n + 1 in u32 saturated and made
        // `at_or_after(u32::MAX)` return u32::MAX - 1.
        let spec = StblSpec {
            stts: vec![(1, 1)],
            stsc: vec![(1, 1, 1)],
            stco: vec![0],
            stsz: vec![1],
            stss: vec![1, u32::MAX],
            ..StblSpec::default()
        };
        let raw = testutil::stbl(&spec);
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        let s = t.sync_samples.as_ref().unwrap();
        assert_eq!(s.at_or_after(u32::MAX), None);
        assert!(!s.contains(u32::MAX));
        // And at_or_before still answers, with something at or before.
        assert_eq!(s.at_or_before(u32::MAX), Some(u32::MAX - 1));
        // The entry for sample u32::MAX - 1 is still reachable by its own index.
        assert!(s.contains(u32::MAX - 1));
        assert_eq!(s.at_or_after(u32::MAX - 1), Some(u32::MAX - 1));
    }

    #[test]
    fn a_missing_stss_makes_every_sample_a_sync_sample() {
        let spec = StblSpec {
            stss: vec![],
            has_stss: false,
            ..measured_video()
        };
        let raw = testutil::stbl(&spec);
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        assert!(t.is_sync(0));
        assert!(t.is_sync(7));
        assert_eq!(t.sync_at_or_before(5), Some(5));
    }

    #[test]
    fn a_present_but_empty_stss_makes_none_of_them_sync() {
        let spec = StblSpec {
            stss: vec![],
            has_stss: true,
            ..measured_video()
        };
        let raw = testutil::stbl(&spec);
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        assert!(!t.is_sync(0));
        assert_eq!(t.sync_at_or_before(5), None);
        assert_eq!(t.sync_at_or_after(0), None);
    }

    #[test]
    fn the_cursor_agrees_with_random_access_at_every_sample() {
        let raw = testutil::stbl(&measured_video());
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        for (i, s) in t.cursor().enumerate() {
            let want = t.sample(i as u32).unwrap();
            assert_eq!(s, want, "sample {i}");
        }
    }

    #[test]
    fn a_cursor_positioned_mid_track_matches_one_walked_there() {
        let raw = testutil::stbl(&measured_video());
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        let walked: Vec<Sample> = t.cursor().skip(5).take(3).collect();
        let jumped: Vec<Sample> = t.cursor_at(5).take(3).collect();
        assert_eq!(walked, jumped);
    }

    #[test]
    fn dts_lookup_is_the_inverse_of_dts() {
        let raw = testutil::stbl(&measured_video());
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        for n in 0..8u32 {
            assert_eq!(t.sample_at_dts(t.dts(n)), Some(n));
            // A tick short of the next sample still lands on this one.
            assert_eq!(t.sample_at_dts(t.dts(n) + 511), Some(n));
        }
        assert_eq!(t.sample_at_dts(-1), None);
    }

    #[test]
    fn stsc_that_does_not_start_at_chunk_one_is_rejected() {
        let spec = StblSpec {
            stsc: vec![(2, 1, 1)],
            ..measured_video()
        };
        let raw = testutil::stbl(&spec);
        let b = crate::testutil::first_box(&raw);
        assert!(SampleTable::parse(&b).is_err());
    }

    #[test]
    fn stsc_with_non_increasing_first_chunk_is_rejected() {
        let spec = StblSpec {
            stsc: vec![(1, 1, 1), (1, 2, 1)],
            ..measured_video()
        };
        let raw = testutil::stbl(&spec);
        let b = crate::testutil::first_box(&raw);
        assert!(SampleTable::parse(&b).is_err());
    }

    #[test]
    fn a_stsc_run_naming_a_chunk_stco_lacks_yields_none() {
        let spec = StblSpec {
            stsc: vec![(1, 1, 1)],
            stco: vec![100],
            stsz: vec![10, 10, 10],
            stts: vec![(3, 1)],
            stss: vec![1],
            ..measured_video()
        };
        let raw = testutil::stbl(&spec);
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        assert_eq!(t.offset(0), Some(100));
        // Samples 1 and 2 want chunks 2 and 3, which do not exist.
        assert_eq!(t.offset(1), None);
        assert_eq!(t.sample(2), None);
        // But the sample count still reports what the table claims.
        assert_eq!(t.sample_count(), 3);
    }

    #[test]
    fn a_uniform_stsz_needs_no_prefix_index() {
        let spec = StblSpec {
            stts: vec![(4, 100)],
            stsc: vec![(1, 4, 1)],
            stco: vec![50],
            stsz_uniform: Some((7, 4)),
            stsz: vec![],
            stss: vec![1],
            ..StblSpec::default()
        };
        let raw = testutil::stbl(&spec);
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        assert!(t.sample_sizes.is_uniform());
        assert_eq!(t.sample_count(), 4);
        let offs: Vec<u64> = (0..4).map(|i| t.offset(i).unwrap()).collect();
        assert_eq!(offs, vec![50, 57, 64, 71]);
    }

    #[test]
    fn stz2_four_bit_fields_pack_two_samples_per_byte() {
        // Sizes 3, 5, 7, 1 as nibbles: 0x35, 0x71.
        let spec = StblSpec {
            stts: vec![(4, 10)],
            stsc: vec![(1, 4, 1)],
            stco: vec![1000],
            stz2: Some((4, vec![0x35, 0x71], 4)),
            stsz: vec![],
            stss: vec![1],
            ..StblSpec::default()
        };
        let raw = testutil::stbl(&spec);
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        let sizes: Vec<u32> = (0..4).map(|i| t.sample_sizes.size(i).unwrap()).collect();
        assert_eq!(sizes, vec![3, 5, 7, 1]);
        let offs: Vec<u64> = (0..4).map(|i| t.offset(i).unwrap()).collect();
        assert_eq!(offs, vec![1000, 1003, 1008, 1015]);
    }

    #[test]
    fn stz2_with_an_odd_count_reads_the_high_nibble_of_the_last_byte() {
        let spec = StblSpec {
            stts: vec![(3, 10)],
            stsc: vec![(1, 3, 1)],
            stco: vec![0],
            stz2: Some((4, vec![0x24, 0x90], 3)),
            stsz: vec![],
            stss: vec![1],
            ..StblSpec::default()
        };
        let raw = testutil::stbl(&spec);
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        assert_eq!(t.sample_sizes.size(2), Some(9));
        assert_eq!(t.sample_sizes.size(3), None);
    }

    #[test]
    fn an_unknown_stz2_field_size_yields_an_empty_table() {
        let spec = StblSpec {
            stts: vec![(3, 10)],
            stsc: vec![(1, 3, 1)],
            stco: vec![0],
            stz2: Some((7, vec![1, 2, 3], 3)),
            stsz: vec![],
            stss: vec![1],
            ..StblSpec::default()
        };
        let raw = testutil::stbl(&spec);
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        assert_eq!(t.sample_count(), 0);
    }

    #[test]
    fn co64_offsets_survive_beyond_four_gigabytes() {
        let spec = StblSpec {
            stts: vec![(2, 1)],
            stsc: vec![(1, 1, 1)],
            co64: Some(vec![0x1_0000_0000, 0x2_0000_0000]),
            stco: vec![],
            stsz: vec![16, 16],
            stss: vec![1],
            ..StblSpec::default()
        };
        let raw = testutil::stbl(&spec);
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        assert!(t.chunk_offsets.is_wide());
        assert_eq!(t.offset(0), Some(0x1_0000_0000));
        assert_eq!(t.offset(1), Some(0x2_0000_0000));
    }

    #[test]
    fn ctts_version_one_offsets_are_signed_and_drive_the_dts_shift() {
        // Measured from ncts.mp4: ctts v1 runs (1,0) (1,1024) (2,-512).
        let spec = StblSpec {
            stts: vec![(4, 512)],
            ctts_v0: vec![],
            ctts_v1: vec![(1, 0), (1, 1024), (2, -512)],
            stsc: vec![(1, 4, 1)],
            stco: vec![52],
            stsz: vec![4824, 1668, 1011, 700],
            stss: vec![1],
            ..StblSpec::default()
        };
        let raw = testutil::stbl(&spec);
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        assert_eq!(t.cts_offset(0), 0);
        assert_eq!(t.cts_offset(1), 1024);
        assert_eq!(t.cts_offset(2), -512);
        // D17: the reference shifts DTS by min(ctts), not PTS by -min(ctts).
        assert_eq!(t.dts_shift(), -512);
        let dts: Vec<i64> = (0..3).map(|i| t.dts(i) + t.dts_shift()).collect();
        assert_eq!(dts, vec![-512, 0, 512]);
        let pts: Vec<i64> = (0..3).map(|i| t.sample(i).unwrap().pts()).collect();
        assert_eq!(pts, vec![0, 1536, 512]);
    }

    #[test]
    fn a_ctts_version_zero_offset_above_i32_max_clamps_rather_than_wrapping() {
        let spec = StblSpec {
            stts: vec![(1, 1)],
            ctts_raw_v0: Some(vec![(1, 0xFFFF_FFFF)]),
            stsc: vec![(1, 1, 1)],
            stco: vec![0],
            stsz: vec![1],
            stss: vec![1],
            ..StblSpec::default()
        };
        let raw = testutil::stbl(&spec);
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        assert_eq!(t.cts_offset(0), i32::MAX);
        assert_eq!(t.dts_shift(), 0);
    }

    #[test]
    fn cslg_beats_the_ctts_extremes() {
        let spec = StblSpec {
            stts: vec![(2, 512)],
            ctts_v1: vec![(2, -1024)],
            cslg: Some((3000, 3000, 3000, 0, 1024)),
            stsc: vec![(1, 2, 1)],
            stco: vec![0],
            stsz: vec![1, 1],
            stss: vec![1],
            ..StblSpec::default()
        };
        let raw = testutil::stbl(&spec);
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        // least_offset is positive, so no shift, even though ctts says -1024.
        assert_eq!(t.dts_shift(), 0);
    }

    #[test]
    fn a_stts_shorter_than_stsz_extrapolates_the_last_delta() {
        let spec = StblSpec {
            stts: vec![(2, 100)],
            stsc: vec![(1, 4, 1)],
            stco: vec![0],
            stsz: vec![1, 1, 1, 1],
            stss: vec![1],
            ..StblSpec::default()
        };
        let raw = testutil::stbl(&spec);
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        assert_eq!(t.dts(1), 100);
        assert_eq!(t.dts(2), 200);
        assert_eq!(t.dts(3), 300);
    }

    #[test]
    fn a_zero_delta_run_reports_the_first_sample_sharing_the_time() {
        let spec = StblSpec {
            stts: vec![(3, 0), (2, 100)],
            stsc: vec![(1, 5, 1)],
            stco: vec![0],
            stsz: vec![1; 5],
            stss: vec![1],
            ..StblSpec::default()
        };
        let raw = testutil::stbl(&spec);
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        assert_eq!(t.dts(0), 0);
        assert_eq!(t.dts(3), 0);
        assert_eq!(t.dts(4), 100);
        assert_eq!(t.sample_at_dts(0), Some(0));
        assert_eq!(t.sample_at_dts(150), Some(4));
    }

    #[test]
    fn an_empty_stbl_is_parseable_and_answers_nothing() {
        let raw = testutil::bx(b"stbl", &[]);
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        assert_eq!(t.sample_count(), 0);
        assert_eq!(t.chunk_count(), 0);
        assert_eq!(t.sample(0), None);
        assert_eq!(t.offset(0), None);
        assert_eq!(t.sample_at_dts(0), None);
        assert_eq!(t.cursor().count(), 0);
        assert_eq!(t.total_duration(), 0);
    }

    #[test]
    fn one_bad_chunk_offset_costs_one_chunk_not_the_rest_of_the_track() {
        // The `isom_sample_table` finding, reduced: a `co64` whose first chunk
        // sits at u64::MAX. Sample 0 is exactly representable; every later
        // sample in that chunk overflows; the second chunk is ordinary.
        let spec = StblSpec {
            stts: vec![(4, 10)],
            stsc: vec![(1, 2, 1)],
            co64: Some(vec![u64::MAX, 5000]),
            stco: vec![],
            stsz_uniform: Some((1024, 4)),
            stsz: vec![],
            has_stss: false,
            ..StblSpec::default()
        };
        let raw = testutil::stbl(&spec);
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        assert_eq!(t.sample(0).map(|s| s.offset), Some(u64::MAX));
        assert_eq!(
            t.sample(1),
            None,
            "sample 1 overflows u64 and must not resolve"
        );
        assert_eq!(t.sample(2).map(|s| s.offset), Some(5000));
        assert_eq!(t.sample(3).map(|s| s.offset), Some(5000 + 1024));
        // The cursor must reach the second chunk, not stop at the hole.
        let got: Vec<(u32, u64)> = t.cursor().map(|s| (s.index, s.offset)).collect();
        assert_eq!(got, vec![(0, u64::MAX), (2, 5000), (3, 6024)]);
    }

    #[test]
    fn a_stsc_declaring_billions_of_offsetless_chunks_ends_at_once() {
        // The `slow-unit` shape, reduced: run 0 spans chunks 1..4_000_000_000
        // at one sample each, and `stco` has three of them. Iterating the
        // remaining 3_999_999_997 one at a time took 13.8 seconds on a 78-byte
        // input; chunk numbers are monotone, so the fourth is the end.
        let spec = StblSpec {
            stts: vec![(4, 10)],
            stsc: vec![(1, 1, 1), (4_000_000_000, 1, 1)],
            stco: vec![100, 200, 300],
            stsz_uniform: Some((8, u32::MAX)),
            stsz: vec![],
            has_stss: false,
            ..StblSpec::default()
        };
        let raw = testutil::stbl(&spec);
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        // Three chunks have offsets, so three samples resolve and then it ends.
        let got: Vec<u64> = t.cursor().map(|s| s.offset).collect();
        assert_eq!(got, vec![100, 200, 300]);
        // Random access agrees: sample 3 is in chunk 4, which has no offset.
        assert_eq!(t.sample(3), None);
        assert_eq!(t.sample(2).map(|s| s.offset), Some(300));
    }

    #[test]
    fn the_cursor_yields_exactly_what_random_access_resolves() {
        // A chunk table with a gap in the middle: chunk 2 has no `stco` entry
        // because the table is short, so its samples are unresolvable.
        let spec = StblSpec {
            stts: vec![(6, 10)],
            stsc: vec![(1, 2, 1), (2, 2, 1), (3, 2, 1)],
            stco: vec![100, 0, 300],
            stsz_uniform: Some((10, 6)),
            stsz: vec![],
            has_stss: false,
            ..StblSpec::default()
        };
        let raw = testutil::stbl(&spec);
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        let expected: Vec<Sample> = (0..t.sample_count()).filter_map(|n| t.sample(n)).collect();
        let got: Vec<Sample> = t.cursor().collect();
        assert_eq!(got, expected);
        assert_eq!(got.len(), 6);
    }

    #[test]
    fn the_carried_time_position_matches_random_access_across_runs() {
        // Several `stts` runs with different deltas: the cursor's carried run
        // position and the table's binary search must agree at every sample,
        // including the extrapolated tail past the end of `stts`.
        let spec = StblSpec {
            stts: vec![(3, 100), (2, 0), (4, 250)],
            stsc: vec![(1, 3, 1)],
            stco: vec![0, 1000, 2000, 3000, 4000],
            stsz_uniform: Some((8, 12)),
            stsz: vec![],
            has_stss: false,
            ..StblSpec::default()
        };
        let raw = testutil::stbl(&spec);
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        for (i, s) in t.cursor().enumerate() {
            let n = i as u32;
            assert_eq!(s.dts, t.dts(n), "dts at sample {n}");
            assert_eq!(s, t.sample(n).unwrap(), "sample {n}");
        }
        // 9..11 are past the nine samples `stts` covers, so they extrapolate
        // at the final delta of 250.
        // Three samples at 100, two at 0, four at 250: 300 + 0 + 4*250.
        assert_eq!(t.dts(9), 300 + 1000);
        assert_eq!(t.dts(10), 300 + 1000 + 250);
    }

    #[test]
    fn a_uniform_stsz_can_declare_more_samples_than_bytes_and_still_terminates() {
        // Twelve bytes of header declaring four billion samples. Legal, and the
        // one count with no payload to clamp it — see `SampleSizes::uniform`.
        let spec = StblSpec {
            stts: vec![(1, 100)],
            stsc: vec![(1, u32::MAX, 1)],
            stco: vec![1000, 2000],
            stsz_uniform: Some((9727, u32::MAX)),
            stsz: vec![],
            stss: vec![1],
            ..StblSpec::default()
        };
        let raw = testutil::stbl(&spec);
        let b = crate::testutil::first_box(&raw);
        let t = SampleTable::parse(&b).unwrap();
        assert_eq!(t.sample_count(), u32::MAX);
        // Nothing was allocated for it: the table is uniform, so there is no
        // prefix index at all.
        assert!(t.sample_sizes.is_uniform());
        // And the cursor still terminates: it refuses an index at the count.
        assert!(t.cursor_at(u32::MAX).next().is_none());
        // Random access stays coherent for an index the chunks can place.
        assert_eq!(t.offset(0), Some(1000));
        assert_eq!(t.offset(1), Some(1000 + 9727));
    }

    #[test]
    fn a_declared_table_count_far_beyond_the_payload_allocates_nothing() {
        // stsz claiming four billion samples in a 12-byte box.
        let mut body = vec![0u8, 0, 0, 0]; // version/flags
        body.extend_from_slice(&0u32.to_be_bytes()); // sample_size = 0
        body.extend_from_slice(&u32::MAX.to_be_bytes()); // sample_count
        body.extend_from_slice(&[0, 0, 0, 5]); // one entry
        let raw = testutil::bx(b"stsz", &body);
        let b = crate::testutil::first_box(&raw);
        let sizes = SampleSizes::parse_stsz(&b.full().unwrap());
        assert_eq!(sizes.count(), 1);
        assert_eq!(sizes.size(0), Some(5));
        assert_eq!(sizes.size(1), None);
    }
}
