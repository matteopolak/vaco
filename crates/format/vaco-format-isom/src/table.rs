//! Fixed-stride table views and the decimated summaries that make them
//! randomly accessible.
//!
//! # The memory decision, with the arithmetic
//!
//! A sample table is a count followed by `count` fixed-width entries. The naive
//! parse — `budget.alloc::<Entry>(count)` — is a denial of service: `stsz`
//! claims a sample count in a 4-byte header, and believing a claim of
//! `0xFFFF_FFFF` costs 16 GiB before a single entry has been read.
//!
//! Two rules remove the whole class:
//!
//! 1. **The declared count is clamped to what the box payload can actually
//!    hold.** Every one of these tables has a fixed entry stride, so
//!    `usable = min(declared, payload_len / stride)` is exact, not a heuristic.
//!    A 16-byte `stsz` can no longer claim four billion samples.
//! 2. **Nothing is decoded up front.** [`EntryTable`] is a `&[u8]` plus a
//!    stride; `get_u32(i, off)` reads big-endian bytes on demand. The parse of
//!    an entire `stbl` allocates *nothing* proportional to the sample count.
//!
//! The arithmetic that follows, for a 3-hour 30 fps video track (324 000
//! samples, one chunk per sample, worst case):
//!
//! | Table | In the file | Our residency |
//! |---|---:|---:|
//! | `stsz` | 1.30 MB | 0 (borrowed) |
//! | `stco` | 1.30 MB | 0 (borrowed) |
//! | `stts` (1 run) | 8 B | 0 (borrowed) |
//! | `stsc` (2 runs) | 24 B | 0 (borrowed) |
//! | summaries | — | ≤ 4 × 64 KiB |
//!
//! Against a materialised `Vec<SampleRef>` at 32 bytes per sample — 10.4 MB per
//! track, and 130 MB for a 9-hour surveillance file — this is a 20× to 400×
//! reduction, and it is bounded by a constant rather than by the input.
//!
//! # Why the summaries exist
//!
//! Borrowing alone gives O(1) access to entry *i*, but the questions a demuxer
//! asks are cumulative: *which sample is at DTS t*, *what is the byte offset of
//! sample n*. Answering those from run-length tables is O(runs) or O(samples)
//! per query, and a seek asks them repeatedly.
//!
//! [`RunIndex`] is a **decimated prefix sum**: a checkpoint every `stride`
//! entries, at most [`MAX_CHECKPOINTS`] of them. Lookup is a binary search over
//! the checkpoints plus at most `stride` linear steps. Both memory and query
//! cost are then bounded by constants no input can move — which is the property
//! that matters, because the alternative (a full prefix sum) is 16 bytes per
//! run and a pathological `stts` has one run per sample.

use vaco_bitstream::ByteReader;

/// Largest number of checkpoints any one summary holds.
///
/// 4096 × 24 bytes ≈ 96 KiB per summary. Four summaries per track and a
/// hundred tracks is under 40 MiB in the worst case, and a normal file's
/// summaries are a handful of entries because a normal file has a handful of
/// runs.
pub const MAX_CHECKPOINTS: usize = 4096;

/// A borrowed table of fixed-width entries.
///
/// The declared count is already clamped against the payload, so
/// [`EntryTable::len`] never exceeds what the bytes can supply.
#[derive(Debug, Clone, Copy)]
pub struct EntryTable<'a> {
    data: &'a [u8],
    stride: usize,
    len: u32,
}

impl<'a> EntryTable<'a> {
    /// Take up to `declared` entries of `stride` bytes from `data`.
    ///
    /// `stride` of zero yields an empty table rather than dividing by zero.
    #[must_use]
    pub fn new(data: &'a [u8], stride: usize, declared: u32) -> Self {
        if stride == 0 {
            return Self {
                data: &[],
                stride: 1,
                len: 0,
            };
        }
        #[allow(
            clippy::integer_division,
            reason = "stride is proven non-zero immediately above; this is the clamp that makes a declared count trustworthy"
        )]
        let capacity = data.len() / stride;
        let len = u32::try_from(capacity).unwrap_or(u32::MAX).min(declared);
        Self { data, stride, len }
    }

    /// Entries actually available.
    #[must_use]
    pub const fn len(&self) -> u32 {
        self.len
    }

    /// Whether the table holds no entries.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Bytes per entry.
    #[must_use]
    pub const fn stride(&self) -> usize {
        self.stride
    }

    /// Entry `i`'s bytes.
    #[must_use]
    pub fn entry(&self, i: u32) -> Option<&'a [u8]> {
        if i >= self.len {
            return None;
        }
        let at = (i as usize).checked_mul(self.stride)?;
        let end = at.checked_add(self.stride)?;
        self.data.get(at..end)
    }

    /// A reader positioned at entry `i`.
    #[must_use]
    pub fn reader_at(&self, i: u32) -> Option<ByteReader<'a>> {
        self.entry(i).map(ByteReader::new)
    }

    /// The big-endian `u32` at byte `off` within entry `i`.
    #[must_use]
    pub fn get_u32(&self, i: u32, off: usize) -> Option<u32> {
        let e = self.entry(i)?;
        let at = e.get(off..)?.first_chunk::<4>()?;
        Some(u32::from_be_bytes(*at))
    }

    /// The big-endian `u64` at byte `off` within entry `i`.
    #[must_use]
    pub fn get_u64(&self, i: u32, off: usize) -> Option<u64> {
        let e = self.entry(i)?;
        let at = e.get(off..)?.first_chunk::<8>()?;
        Some(u64::from_be_bytes(*at))
    }

    /// The raw bytes the table spans, for a caller that wants to hash or copy
    /// the table verbatim.
    #[must_use]
    pub fn as_bytes(&self) -> &'a [u8] {
        let n = (self.len as usize).saturating_mul(self.stride);
        self.data.get(..n).unwrap_or(self.data)
    }
}

/// One checkpoint in a [`RunIndex`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Checkpoint {
    /// Index of the run this checkpoint sits *before*.
    pub run: u32,
    /// Samples covered by every earlier run.
    pub samples: u64,
    /// Accumulated value (a DTS, or a byte count) before this run.
    pub value: i64,
}

/// A decimated prefix sum over run-length-coded entries.
///
/// Built once, queried per seek. Memory is `O(MAX_CHECKPOINTS)`, query cost is
/// `O(log MAX_CHECKPOINTS + stride)`, and neither depends on the input.
#[derive(Debug, Clone, Default)]
pub struct RunIndex {
    points: Vec<Checkpoint>,
    stride: u32,
    total_samples: u64,
    total_value: i64,
    runs: u32,
}

impl RunIndex {
    /// Build a summary from `runs` entries, where `entry(i)` reports
    /// `(sample_count, per_sample_value)` for run `i`.
    ///
    /// Accumulation is saturating rather than wrapping. Both sequences are
    /// non-decreasing, so saturation preserves the ordering the binary searches
    /// rely on; wrapping would not, and panicking on overflow is not available
    /// to a parser of untrusted input.
    #[must_use]
    pub fn build<F>(runs: u32, mut entry: F) -> Self
    where
        F: FnMut(u32) -> (u32, i64),
    {
        let stride = decimation_stride(runs);
        let mut points: Vec<Checkpoint> = Vec::new();
        let mut samples = 0u64;
        let mut value = 0i64;
        for i in 0..runs {
            if i.is_multiple_of(stride) {
                points.push(Checkpoint {
                    run: i,
                    samples,
                    value,
                });
            }
            let (count, per) = entry(i);
            samples = samples.saturating_add(u64::from(count));
            value = value.saturating_add(i64::from(count).saturating_mul(per));
        }
        Self {
            points,
            stride,
            total_samples: samples,
            total_value: value,
            runs,
        }
    }

    /// Samples covered by every run.
    #[must_use]
    pub const fn total_samples(&self) -> u64 {
        self.total_samples
    }

    /// Accumulated value across every run.
    #[must_use]
    pub const fn total_value(&self) -> i64 {
        self.total_value
    }

    /// Number of runs summarised.
    #[must_use]
    pub const fn runs(&self) -> u32 {
        self.runs
    }

    /// Runs between checkpoints.
    #[must_use]
    pub const fn stride(&self) -> u32 {
        self.stride
    }

    /// Checkpoints held, for tests and for the memory audit.
    #[must_use]
    pub fn checkpoints(&self) -> usize {
        self.points.len()
    }

    /// The last checkpoint at or before sample `n`.
    #[must_use]
    pub fn checkpoint_for_sample(&self, n: u64) -> Checkpoint {
        let at = self.points.partition_point(|c| c.samples <= n);
        self.points
            .get(at.saturating_sub(1))
            .copied()
            .unwrap_or(Checkpoint {
                run: 0,
                samples: 0,
                value: 0,
            })
    }

    /// The last checkpoint whose accumulated value is **strictly below** `v`,
    /// or the first checkpoint when none is.
    ///
    /// Strictly below, not "at or below", because the value sequence is only
    /// *non*-decreasing: a run of zero-duration samples gives several
    /// checkpoints the same value, and `<=` would land on the last of them and
    /// skip every sample that shares the target time. Starting one checkpoint
    /// early is always safe — the walk that follows finds the right run — and
    /// costs at most one extra stride.
    ///
    /// Correct only where the value sequence is non-decreasing, which holds for
    /// `stts` (durations are unsigned) and for cumulative byte sizes.
    #[must_use]
    pub fn checkpoint_for_value(&self, v: i64) -> Checkpoint {
        let at = self.points.partition_point(|c| c.value < v);
        self.points
            .get(at.saturating_sub(1))
            .copied()
            .unwrap_or(Checkpoint {
                run: 0,
                samples: 0,
                value: 0,
            })
    }
}

/// Runs per checkpoint for a table of `runs` entries.
///
/// Chosen so the summary never exceeds [`MAX_CHECKPOINTS`] entries: a
/// well-behaved file (one `stts` run) gets stride 1 and an exact index, and a
/// pathological one (a run per sample) gets a coarse index and a bounded linear
/// tail instead of a hundred megabytes of prefix sums.
#[must_use]
pub fn decimation_stride(runs: u32) -> u32 {
    let cap = MAX_CHECKPOINTS as u64;
    let runs = u64::from(runs);
    if runs <= cap {
        return 1;
    }
    // ceil(runs / cap), without a division by a possibly-zero value.
    #[allow(
        clippy::integer_division,
        reason = "cap is the non-zero constant MAX_CHECKPOINTS"
    )]
    let s = runs.div_ceil(cap);
    u32::try_from(s).unwrap_or(u32::MAX).max(1)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code"
)]
mod tests {
    use super::*;

    #[test]
    fn a_declared_count_is_clamped_to_the_bytes_present() {
        // The classic amplification: a 16-byte payload claiming four billion
        // entries.
        let data = [0u8; 16];
        let t = EntryTable::new(&data, 8, u32::MAX);
        assert_eq!(t.len(), 2);
        assert!(t.entry(2).is_none());
    }

    #[test]
    fn a_zero_stride_yields_an_empty_table_rather_than_dividing() {
        let data = [1u8, 2, 3];
        let t = EntryTable::new(&data, 0, 99);
        assert!(t.is_empty());
        assert!(t.entry(0).is_none());
    }

    #[test]
    fn entries_read_big_endian_at_an_offset() {
        let data = [0, 0, 0, 5, 0, 0, 0, 9, 0, 0, 0, 7, 0, 0, 0, 1];
        let t = EntryTable::new(&data, 8, 2);
        assert_eq!(t.get_u32(0, 0), Some(5));
        assert_eq!(t.get_u32(0, 4), Some(9));
        assert_eq!(t.get_u32(1, 0), Some(7));
        assert_eq!(t.get_u32(1, 4), Some(1));
        assert_eq!(t.get_u32(1, 5), None);
        assert_eq!(t.get_u32(2, 0), None);
        assert_eq!(t.as_bytes().len(), 16);
    }

    #[test]
    fn a_short_declared_count_wins_over_the_payload() {
        let data = [0u8; 64];
        let t = EntryTable::new(&data, 8, 3);
        assert_eq!(t.len(), 3);
    }

    #[test]
    fn a_summary_of_one_run_is_exact() {
        let idx = RunIndex::build(1, |_| (50, 512));
        assert_eq!(idx.total_samples(), 50);
        assert_eq!(idx.total_value(), 25_600);
        assert_eq!(idx.stride(), 1);
        assert_eq!(idx.checkpoints(), 1);
    }

    #[test]
    fn a_summary_never_exceeds_its_checkpoint_cap() {
        let runs = 1_000_000u32;
        let idx = RunIndex::build(runs, |_| (1, 1));
        assert!(
            idx.checkpoints() <= MAX_CHECKPOINTS,
            "{}",
            idx.checkpoints()
        );
        assert_eq!(idx.total_samples(), u64::from(runs));
        assert!(idx.stride() > 1);
    }

    #[test]
    fn value_lookup_does_not_skip_past_a_tie() {
        // Three zero-valued runs then a real one: looking up value 0 must land
        // on the first run, not the last one that also reads zero.
        let idx = RunIndex::build(4, |i| if i < 3 { (3, 0) } else { (2, 100) });
        let c = idx.checkpoint_for_value(0);
        assert_eq!(c.run, 0);
        assert_eq!(c.samples, 0);
        // And a value inside the last run still starts at or before it.
        let c = idx.checkpoint_for_value(150);
        assert!(c.value <= 150);
    }

    #[test]
    fn checkpoint_lookup_lands_at_or_before_the_target() {
        let idx = RunIndex::build(10, |i| (10, i64::from(i) + 1));
        let c = idx.checkpoint_for_sample(35);
        assert!(c.samples <= 35);
        assert_eq!(c.run, 3);
        let c0 = idx.checkpoint_for_sample(0);
        assert_eq!(c0.samples, 0);
        assert_eq!(c0.run, 0);
        // Past the end clamps to the last checkpoint rather than panicking.
        let last = idx.checkpoint_for_sample(u64::MAX);
        assert_eq!(last.run, 9);
    }

    #[test]
    fn accumulation_saturates_rather_than_overflowing() {
        let idx = RunIndex::build(4, |_| (u32::MAX, i64::MAX));
        assert_eq!(idx.total_value(), i64::MAX);
        assert!(idx.total_samples() > 0);
    }

    #[test]
    fn decimation_stride_is_monotone_and_never_zero() {
        assert_eq!(decimation_stride(0), 1);
        assert_eq!(decimation_stride(MAX_CHECKPOINTS as u32), 1);
        assert_eq!(decimation_stride(MAX_CHECKPOINTS as u32 + 1), 2);
        assert!(decimation_stride(u32::MAX) >= 1);
        assert!(
            u64::from(decimation_stride(u32::MAX)).saturating_mul(MAX_CHECKPOINTS as u64)
                >= u64::from(u32::MAX)
        );
    }
}
