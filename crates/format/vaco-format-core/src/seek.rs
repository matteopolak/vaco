//! Seeking: the target model, the index, and the two generic strategies.
//!
//! Three container families cover the design space and all three have to be
//! expressible here without contortions:
//!
//! | Family | What it has | Path it takes |
//! |---|---|---|
//! | MP4 | a complete sample table built at `read_header` | [`PacketIndex`], populated once, [`PacketIndex::search`] per seek |
//! | Matroska | `Cues` — sparse, keyframe-only, sometimes absent | [`PacketIndex`] when present, [`binary_search`] when not |
//! | MPEG-TS | nothing at all; timestamps that legitimately jump | [`binary_search`] where the stream is continuous, byte seek plus resync where it is not |
//!
//! The full analysis, including what each of the three would actually write, is
//! in `docs/format/vaco-format-core.md`. It is the justification for this
//! module's shape and it is worth reading before changing anything here.
//!
//! # Dispatch
//!
//! [`Demuxer::seek`](crate::Demuxer::seek) owns the whole operation: the frozen
//! trait has no `read_timestamp` hook, so the core cannot drive a bisection on
//! a demuxer's behalf the way `planning/18-formats.md` §1.8.2 assumed. What it
//! can do — and what this module provides — is hand the demuxer the two generic
//! strategies as *callable functions* it drives itself. [`SeekStrategy::choose`]
//! encodes the same S1 decision table; the demuxer calls it and then calls
//! whichever helper it names.
//!
//! That inversion is the one substantive consequence of the frozen trait, and
//! it is not obviously worse: the demuxer already owns its I/O context, so a
//! core-driven bisection would have had to reach back through a callback
//! anyway.

use vaco_core::{Error, Rational, Result, TimeBase, Timestamp};

use crate::flags::FormatFlags;
use crate::options::{FFlags, FormatOptions};

/// Where to seek to.
#[derive(Debug, Clone, Copy)]
pub enum SeekTarget {
    /// To a timestamp on a specific stream, in that stream's time base.
    Timestamp { stream_index: u32, ts: Timestamp },
    /// To a byte offset. Used for formats with no index, and by `-bytes`.
    Byte(u64),
    /// To a frame number, where the format can count frames.
    Frame { stream_index: u32, frame: u64 },
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct SeekFlags: u8 {
        /// Land at or before the target rather than at or after it.
        const BACKWARD = 1 << 0;
        /// Allow landing on a non-keyframe; the caller will decode and discard.
        const ANY      = 1 << 1;
        /// Target is a byte position even for a timestamp-capable format.
        const BYTE     = 1 << 2;
    }
}

impl SeekTarget {
    /// The stream the target is expressed against, if any.
    #[must_use]
    pub const fn stream_index(self) -> Option<u32> {
        match self {
            Self::Timestamp { stream_index, .. } | Self::Frame { stream_index, .. } => {
                Some(stream_index)
            }
            Self::Byte(_) => None,
        }
    }

    /// Convert a [`SeekTarget::Frame`] into a timestamp using `frame_rate` and
    /// the stream's `time_base` (S7).
    ///
    /// Everything else passes through unchanged, so this is safe to call
    /// unconditionally at the top of a `seek` implementation.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] when the frame rate is unknown or unusable — a
    /// frame number means nothing without one, and guessing would silently land
    /// somewhere else.
    pub fn resolve_frames(self, frame_rate: Rational, time_base: TimeBase) -> Result<Self> {
        let Self::Frame {
            stream_index,
            frame,
        } = self
        else {
            return Ok(self);
        };
        if !frame_rate.is_defined() || frame_rate.is_zero() || frame_rate.is_infinite() {
            return Err(Error::Unsupported(
                "frame-number seek needs a known frame rate",
            ));
        }
        let n = i64::try_from(frame).map_err(|_| Error::Unsupported("frame number too large"))?;
        // frame / rate seconds, expressed in time_base ticks.
        let ticks = Timestamp::new(n)
            .checked_rescale(frame_rate.inverse(), time_base, vaco_core::Rounding::Zero)
            .ok_or(Error::Unsupported(
                "frame number does not fit the time base",
            ))?;
        Ok(Self::Timestamp {
            stream_index,
            ts: ticks,
        })
    }
}

/// Which generic strategy applies, given what the container declares (S1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekStrategy {
    /// Use the index: either the container's own or one built as packets went
    /// past.
    Index,
    /// Bisect over byte positions, probing timestamps (S5).
    BinarySearch,
    /// Seek to a byte offset and resynchronise (S6).
    Byte,
    /// Nothing applies. Report [`Error::NotSeekable`].
    Unsupported,
}

impl SeekStrategy {
    /// Decide, in the fixed order the model specifies.
    ///
    /// `has_index` is whether the caller holds a usable index *now* — after
    /// `fflags +ignidx` has been honoured, which is the caller's job because
    /// only it knows whether its index came from the container or from its own
    /// observation.
    #[must_use]
    pub fn choose(
        target: SeekTarget,
        flags: SeekFlags,
        format: FormatFlags,
        has_index: bool,
        seekable: bool,
    ) -> Self {
        if !seekable {
            return Self::Unsupported;
        }
        let byte_target = flags.contains(SeekFlags::BYTE) || matches!(target, SeekTarget::Byte(_));
        if byte_target {
            return if format.allows_byte_seek() {
                Self::Byte
            } else {
                Self::Unsupported
            };
        }
        if has_index && format.allows_index_seek() {
            return Self::Index;
        }
        if format.allows_binary_search() {
            return Self::BinarySearch;
        }
        if format.allows_byte_seek() {
            return Self::Byte;
        }
        Self::Unsupported
    }
}

bitflags::bitflags! {
    /// What an index entry says about the packet at that position.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
    pub struct IndexFlags: u8 {
        /// Decoding may start here.
        const KEYFRAME = 1 << 0;
        /// Present for structure only; the packet must be dropped.
        const DISCARD  = 1 << 1;
    }
}

/// One seek point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexEntry {
    pub pos: u64,
    pub timestamp: Timestamp,
    pub flags: IndexFlags,
    pub size: u32,
    /// Bytes back to the previous keyframe; zero means unknown.
    pub min_distance: u32,
}

impl IndexEntry {
    /// A keyframe entry at `pos` with timestamp `ts`.
    #[must_use]
    pub const fn keyframe(pos: u64, ts: Timestamp) -> Self {
        Self {
            pos,
            timestamp: ts,
            flags: IndexFlags::KEYFRAME,
            size: 0,
            min_distance: 0,
        }
    }

    /// A non-keyframe entry.
    #[must_use]
    pub const fn frame(pos: u64, ts: Timestamp) -> Self {
        Self {
            pos,
            timestamp: ts,
            flags: IndexFlags::empty(),
            size: 0,
            min_distance: 0,
        }
    }

    /// Whether decoding may start here.
    #[must_use]
    pub const fn is_key(&self) -> bool {
        self.flags.contains(IndexFlags::KEYFRAME)
    }
}

/// One stream's seek points, sorted strictly by timestamp.
///
/// Built three ways: from a container-native index at `read_header` (MP4's
/// `stss`+`stts`+`stco`, Matroska's `Cues`, AVI's `idx1`), incrementally as
/// packets are read for a [`FormatFlags::GENERIC_INDEX`] format, or as a
/// by-product of [`binary_search`], which populates it for free.
#[derive(Debug, Clone, Default)]
pub struct PacketIndex {
    entries: Vec<IndexEntry>,
    max_entries: usize,
    decimations: u32,
}

/// Entries a default 1 MiB `indexmem` buys.
#[allow(
    clippy::integer_division,
    reason = "size_of is a non-zero constant; this converts a byte cap into an entry cap"
)]
const DEFAULT_MAX_ENTRIES: usize = (1 << 20) / core::mem::size_of::<IndexEntry>();

impl PacketIndex {
    /// An empty index with the default memory cap.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
            max_entries: DEFAULT_MAX_ENTRIES,
            decimations: 0,
        }
    }

    /// An empty index sized from `indexmem`.
    #[must_use]
    pub fn with_options(opts: &FormatOptions) -> Self {
        let bytes = usize::try_from(opts.indexmem).unwrap_or(1 << 20);
        #[allow(
            clippy::integer_division,
            reason = "size_of is a non-zero constant; this converts a byte cap into an entry cap"
        )]
        let max = (bytes / core::mem::size_of::<IndexEntry>()).max(2);
        Self {
            entries: Vec::new(),
            max_entries: max,
            decimations: 0,
        }
    }

    /// How many entries the index holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the index is empty — the signal that the index seek path is
    /// unavailable.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entries, in timestamp order.
    #[must_use]
    pub fn entries(&self) -> &[IndexEntry] {
        &self.entries
    }

    /// How many times the index has been decimated to stay under its cap.
    /// Non-zero means seeks are coarser than the container's own index would
    /// have been.
    #[must_use]
    pub const fn decimations(&self) -> u32 {
        self.decimations
    }

    /// Drop every entry, keeping the cap. Used by `fflags +ignidx` and by a
    /// demuxer that discovers its container's index was lying.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.decimations = 0;
    }

    /// Insert or refresh one entry (I1).
    ///
    /// An entry with the same timestamp is *updated*, not duplicated: the same
    /// packet can be seen twice — once from a bisection probe, once from a
    /// linear read — and two entries for one packet make the index a liar about
    /// its own density.
    pub fn add(&mut self, entry: IndexEntry) {
        let Some(ts) = entry.timestamp.ticks() else {
            // An entry with no timestamp cannot be searched for and would break
            // the sort invariant. Dropping it is the only coherent option.
            return;
        };
        // Decimate *before* choosing the insertion point, not after.
        //
        // The obvious order — search, then decimate if full, then insert at the
        // position the search returned — is wrong: decimation shortens the
        // vector, so the position is stale and the entry lands in the wrong
        // slot, silently unsorting the index. Clamping it to the new length
        // hides the symptom for an ascending insertion order and not for any
        // other. Found by the `the_index_stays_well_formed` property test,
        // which shrank it to four out-of-order entries and a small `indexmem`.
        if self.entries.len() >= self.max_entries && self.search_raw(ts).is_err() {
            self.decimate();
        }
        match self.search_raw(ts) {
            Ok(at) => {
                if let Some(slot) = self.entries.get_mut(at) {
                    *slot = entry;
                }
            }
            Err(at) => {
                let at = at.min(self.entries.len());
                self.entries.insert(at, entry);
            }
        }
    }

    /// Halve the index, deterministically and preserving the endpoints (I2).
    ///
    /// Non-keyframes go first, because they are the entries a seek cannot use
    /// unless `ANY` is set. If that is not enough, every second keyframe goes
    /// too. The reference's own eviction policy is unknown to us and is not
    /// observable through any output field, so this is a free choice — recorded
    /// as a choice rather than presented as a reproduction.
    fn decimate(&mut self) {
        let last = self.entries.len().saturating_sub(1);
        let mut seen_non_key = 0usize;
        let mut kept: Vec<IndexEntry> = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            let endpoint = i == 0 || i == last;
            let drop = if e.is_key() {
                false
            } else {
                seen_non_key += 1;
                !endpoint && seen_non_key.is_multiple_of(2)
            };
            if !drop {
                kept.push(*e);
            }
        }
        if kept.len() >= self.entries.len() {
            // Everything was a keyframe. Thin those instead.
            kept.clear();
            for (i, e) in self.entries.iter().enumerate() {
                if i == 0 || i == last || i % 2 == 1 {
                    kept.push(*e);
                }
            }
        }
        self.entries = kept;
        self.decimations = self.decimations.saturating_add(1);
    }

    /// Binary search by raw tick value.
    fn search_raw(&self, ts: i64) -> core::result::Result<usize, usize> {
        self.entries
            .binary_search_by(|e| match e.timestamp.ticks() {
                Some(v) => v.cmp(&ts),
                None => core::cmp::Ordering::Less,
            })
    }

    /// The entry a seek to `ts` should land on.
    ///
    /// With [`SeekFlags::BACKWARD`], the greatest entry at or before `ts`;
    /// without it, the least entry at or after. Without [`SeekFlags::ANY`],
    /// only keyframes qualify.
    ///
    /// `None` means the index has nothing usable in that direction — which is a
    /// fact the caller acts on (fall through to a bisection, or refuse), not an
    /// error.
    #[must_use]
    pub fn search(&self, ts: Timestamp, flags: SeekFlags) -> Option<IndexEntry> {
        let want = ts.ticks()?;
        let backward = flags.contains(SeekFlags::BACKWARD);
        let any = flags.contains(SeekFlags::ANY);
        let usable =
            |e: &&IndexEntry| (any || e.is_key()) && !e.flags.contains(IndexFlags::DISCARD);
        let at = match self.search_raw(want) {
            Ok(i) | Err(i) => i,
        };
        if backward {
            // Everything at or before `want`, closest first.
            let end = match self.search_raw(want) {
                Ok(i) => i.saturating_add(1),
                Err(i) => i,
            };
            self.entries.get(..end)?.iter().rev().find(usable).copied()
        } else {
            self.entries.get(at..)?.iter().find(usable).copied()
        }
    }

    /// The last keyframe entry, for `-sseof`-style resolution.
    #[must_use]
    pub fn last_keyframe(&self) -> Option<IndexEntry> {
        self.entries.iter().rev().find(|e| e.is_key()).copied()
    }

    /// Whether the index is sorted and duplicate-free. Called by tests and by
    /// the fuzz target; cheap enough to call from a debug assertion too.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.entries.windows(2).all(|w| {
            match (
                w.first().and_then(|e| e.timestamp.ticks()),
                w.get(1).and_then(|e| e.timestamp.ticks()),
            ) {
                (Some(a), Some(b)) => a < b,
                _ => false,
            }
        })
    }
}

/// Smallest byte span [`binary_search`] will bisect down to.
///
/// Below this the linear scan from the last probe is cheaper than another round
/// trip, and it also bounds the iteration count.
pub const MIN_SEEK_STEP: u64 = 64 * 1024;

/// Hard iteration cap, on top of the `log2` bound.
///
/// A pathological `probe` that never narrows the interval would otherwise spin,
/// which is a real fuzzing concern rather than a theoretical one.
const MAX_SEEK_ITERATIONS: u32 = 80;

/// The outcome of a generic seek.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeekLanding {
    /// Byte offset to resume reading from.
    pub pos: u64,
    /// The timestamp actually reached, when one is known.
    ///
    /// The CLI needs this: `-ss` on a stream copy starts early, and
    /// `-read_intervals`' "+duration is measured from the *found* position"
    /// rule needs something exact to measure from.
    pub timestamp: Timestamp,
}

/// Bisect byte positions to find the last sync point at or before `target` (S5).
///
/// `probe` is asked for the first sync point at or after a byte position,
/// bounded above by a limit: it returns the position actually found and that
/// packet's DTS, or `None` when there is no sync point in range. It is the one
/// piece only the container knows how to do, which is why it is a parameter.
///
/// The search populates an index as it goes, so the *second* seek into a file
/// is cheap even for a container that ships no index. That is not an
/// optimisation bolted on afterwards — it is why the bisection returns entries
/// at all.
///
/// # Errors
///
/// Whatever `probe` reports. The loop itself cannot fail and cannot hang: it is
/// bounded by `log2(size / MIN_SEEK_STEP) + 4` iterations and again by
/// a hard iteration cap.
pub fn binary_search<P>(
    target: Timestamp,
    lo_pos: u64,
    hi_pos: u64,
    index: &mut PacketIndex,
    mut probe: P,
) -> Result<Option<SeekLanding>>
where
    P: FnMut(u64, u64) -> Result<Option<(u64, Timestamp)>>,
{
    let Some(want) = target.ticks() else {
        return Err(Error::InvalidData("binary seek needs a target timestamp"));
    };
    let (mut lo, mut hi) = (lo_pos, hi_pos.max(lo_pos));
    let mut best: Option<SeekLanding> = None;
    let mut iterations = 0u32;
    while hi.saturating_sub(lo) > MIN_SEEK_STEP && iterations < MAX_SEEK_ITERATIONS {
        iterations = iterations.saturating_add(1);
        #[allow(
            clippy::integer_division,
            reason = "bisection midpoint; the divisor is the literal 2"
        )]
        let mid = lo.saturating_add((hi.saturating_sub(lo)) / 2);
        match probe(mid, hi)? {
            None => hi = mid,
            Some((found, ts)) => {
                if let Some(v) = ts.ticks() {
                    index.add(IndexEntry::keyframe(found, ts));
                    if v <= want {
                        best = Some(SeekLanding {
                            pos: found,
                            timestamp: ts,
                        });
                        // `found` may be at or past `mid`; advancing to it is
                        // what guarantees the interval shrinks even when the
                        // probe had to scan forward a long way.
                        let next = found.max(mid).saturating_add(1);
                        if next >= hi {
                            break;
                        }
                        lo = next;
                    } else {
                        hi = mid;
                    }
                } else {
                    hi = mid;
                }
            }
        }
    }
    if best.is_none() {
        // Nothing at or before the target: report the first sync point in the
        // whole range, which is the best a forward seek can do.
        if let Some((found, ts)) = probe(lo_pos, hi_pos)? {
            index.add(IndexEntry::keyframe(found, ts));
            best = Some(SeekLanding {
                pos: found,
                timestamp: ts,
            });
        }
    }
    Ok(best)
}

/// Whether a demuxer may land on a non-keyframe for this request (S8).
///
/// `seek2any` is a context option and is deliberately separate from
/// [`SeekFlags::ANY`]: some formats can honour one and not the other, and a
/// user who set the option once should not have to repeat it per call.
#[must_use]
pub const fn allows_non_keyframe(flags: SeekFlags, opts: &FormatOptions) -> bool {
    flags.contains(SeekFlags::ANY) || opts.seek2any
}

/// Whether the demuxer may take a cheaper, less accurate path (S9).
///
/// Never changes the correctness of the timestamps *reported*, only which
/// packet you land on.
#[must_use]
pub fn fast_seek(opts: &FormatOptions) -> bool {
    opts.fflags.contains(FFlags::FASTSEEK)
}

/// Whether the container's own index should be used at all (I4).
///
/// `fflags +ignidx` is the escape hatch for files with lying indexes. It
/// changes seek results, which is exactly why it is in the conformance matrix.
#[must_use]
pub fn use_container_index(opts: &FormatOptions) -> bool {
    !opts.fflags.contains(FFlags::IGNIDX)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::field_reassign_with_default,
    clippy::cast_possible_wrap,
    clippy::unnecessary_wraps,
    clippy::integer_division,
    reason = "test code"
)]
mod tests {
    use super::*;

    fn idx(points: &[(u64, i64, bool)]) -> PacketIndex {
        let mut ix = PacketIndex::new();
        for &(pos, ts, key) in points {
            ix.add(if key {
                IndexEntry::keyframe(pos, Timestamp::new(ts))
            } else {
                IndexEntry::frame(pos, Timestamp::new(ts))
            });
        }
        ix
    }

    #[test]
    fn index_stays_sorted_under_arbitrary_insertion_order() {
        let ix = idx(&[(30, 3, true), (10, 1, true), (20, 2, true), (0, 0, true)]);
        assert!(ix.is_well_formed());
        assert_eq!(ix.len(), 4);
        assert_eq!(ix.entries().first().unwrap().pos, 0);
    }

    #[test]
    fn duplicate_timestamps_update_rather_than_duplicate() {
        let mut ix = idx(&[(10, 1, true)]);
        ix.add(IndexEntry::keyframe(99, Timestamp::new(1)));
        assert_eq!(ix.len(), 1);
        assert_eq!(ix.entries().first().unwrap().pos, 99);
    }

    #[test]
    fn entries_without_a_timestamp_are_refused() {
        let mut ix = PacketIndex::new();
        ix.add(IndexEntry::keyframe(1, Timestamp::NONE));
        assert!(ix.is_empty());
    }

    #[test]
    fn search_honours_direction_and_keyframes() {
        let ix = idx(&[
            (0, 0, true),
            (10, 10, false),
            (20, 20, true),
            (30, 30, false),
        ]);
        // Forward to the next keyframe.
        let e = ix.search(Timestamp::new(11), SeekFlags::empty()).unwrap();
        assert_eq!(e.pos, 20);
        // Backward to the previous keyframe.
        let e = ix.search(Timestamp::new(19), SeekFlags::BACKWARD).unwrap();
        assert_eq!(e.pos, 0);
        // ANY takes the nearest entry regardless.
        let e = ix
            .search(Timestamp::new(19), SeekFlags::BACKWARD | SeekFlags::ANY)
            .unwrap();
        assert_eq!(e.pos, 10);
        // An exact hit lands on itself in both directions.
        assert_eq!(
            ix.search(Timestamp::new(20), SeekFlags::empty())
                .unwrap()
                .pos,
            20
        );
        assert_eq!(
            ix.search(Timestamp::new(20), SeekFlags::BACKWARD)
                .unwrap()
                .pos,
            20
        );
        // Past the end, backward finds the last keyframe; forward finds nothing.
        assert_eq!(
            ix.search(Timestamp::new(999), SeekFlags::BACKWARD)
                .unwrap()
                .pos,
            20
        );
        assert!(ix.search(Timestamp::new(999), SeekFlags::empty()).is_none());
        // Absent target is never a match.
        assert!(ix.search(Timestamp::NONE, SeekFlags::empty()).is_none());
    }

    #[test]
    fn decimation_does_not_unsort_the_entry_being_inserted() {
        // The shrunk counterexample: entries arriving out of order, with a cap
        // small enough that decimation fires mid-insertion.
        let mut opts = FormatOptions::default();
        opts.indexmem = (core::mem::size_of::<IndexEntry>() * 2) as i32;
        let mut ix = PacketIndex::with_options(&opts);
        for ts in [0i64, 1, -1, 2, -2, 3] {
            ix.add(IndexEntry::frame(0, Timestamp::new(ts)));
            assert!(
                ix.is_well_formed(),
                "unsorted after inserting {ts}: {:?}",
                ix.entries()
            );
        }
    }

    #[test]
    fn decimation_bounds_memory_and_keeps_the_endpoints() {
        let mut opts = FormatOptions::default();
        // Room for a handful of entries.
        opts.indexmem = (core::mem::size_of::<IndexEntry>() * 8) as i32;
        let mut ix = PacketIndex::with_options(&opts);
        for i in 0..200i64 {
            ix.add(IndexEntry::frame(i as u64 * 100, Timestamp::new(i)));
        }
        assert!(ix.len() <= 8, "index grew to {}", ix.len());
        assert!(ix.is_well_formed());
        assert!(ix.decimations() > 0);
        assert_eq!(
            ix.entries().first().unwrap().timestamp.ticks(),
            Some(0),
            "the first entry must survive decimation"
        );
    }

    #[test]
    fn all_keyframe_index_also_decimates() {
        let mut opts = FormatOptions::default();
        opts.indexmem = (core::mem::size_of::<IndexEntry>() * 8) as i32;
        let mut ix = PacketIndex::with_options(&opts);
        for i in 0..200i64 {
            ix.add(IndexEntry::keyframe(i as u64 * 100, Timestamp::new(i)));
        }
        assert!(ix.len() <= 8);
        assert!(ix.is_well_formed());
    }

    #[test]
    fn strategy_follows_the_declared_capabilities() {
        let t = SeekTarget::Timestamp {
            stream_index: 0,
            ts: Timestamp::new(1),
        };
        let none = SeekFlags::empty();
        // Unseekable input short-circuits everything.
        assert_eq!(
            SeekStrategy::choose(t, none, FormatFlags::empty(), true, false),
            SeekStrategy::Unsupported
        );
        // An index wins when one exists.
        assert_eq!(
            SeekStrategy::choose(t, none, FormatFlags::empty(), true, true),
            SeekStrategy::Index
        );
        // Without one, bisection.
        assert_eq!(
            SeekStrategy::choose(t, none, FormatFlags::empty(), false, true),
            SeekStrategy::BinarySearch
        );
        // A discontinuous format cannot be bisected: byte seek and resync.
        assert_eq!(
            SeekStrategy::choose(t, none, FormatFlags::TS_DISCONT, false, true),
            SeekStrategy::Byte
        );
        // A byte target goes straight there.
        assert_eq!(
            SeekStrategy::choose(
                SeekTarget::Byte(4096),
                none,
                FormatFlags::empty(),
                true,
                true
            ),
            SeekStrategy::Byte
        );
        assert_eq!(
            SeekStrategy::choose(
                SeekTarget::Byte(4096),
                none,
                FormatFlags::NO_BYTE_SEEK,
                true,
                true
            ),
            SeekStrategy::Unsupported
        );
        // Nothing left at all.
        assert_eq!(
            SeekStrategy::choose(
                t,
                none,
                FormatFlags::TS_DISCONT | FormatFlags::NO_BYTE_SEEK,
                false,
                true
            ),
            SeekStrategy::Unsupported
        );
    }

    #[test]
    fn frame_targets_resolve_through_the_frame_rate() {
        let t = SeekTarget::Frame {
            stream_index: 0,
            frame: 100,
        };
        // 25 fps, 1/1000 s time base: frame 100 is 4.000 s = 4000 ticks.
        let r = t
            .resolve_frames(Rational::new(25, 1), Rational::new(1, 1000))
            .unwrap();
        match r {
            SeekTarget::Timestamp { ts, .. } => assert_eq!(ts.ticks(), Some(4000)),
            _ => unreachable!("must become a timestamp"),
        }
        assert!(
            t.resolve_frames(Rational::ZERO, Rational::new(1, 1000))
                .is_err()
        );
        // Non-frame targets pass straight through.
        assert!(matches!(
            SeekTarget::Byte(7)
                .resolve_frames(Rational::ZERO, Rational::ONE)
                .unwrap(),
            SeekTarget::Byte(7)
        ));
    }

    /// A synthetic file: one sync point every 1000 bytes, timestamp = index.
    fn synthetic_probe(pos: u64, limit: u64) -> Result<Option<(u64, Timestamp)>> {
        #[allow(clippy::integer_division, reason = "test fixture arithmetic")]
        let next = pos.div_ceil(1000) * 1000;
        if next > limit || next > 100_000 {
            return Ok(None);
        }
        #[allow(clippy::integer_division, reason = "test fixture arithmetic")]
        let ts = (next / 1000) as i64;
        Ok(Some((next, Timestamp::new(ts))))
    }

    #[test]
    fn binary_search_lands_at_or_before_the_target() {
        for want in [0i64, 1, 37, 99, 100] {
            let mut ix = PacketIndex::new();
            let got = binary_search(Timestamp::new(want), 0, 100_000, &mut ix, synthetic_probe)
                .unwrap()
                .unwrap();
            let landed = got.timestamp.ticks().unwrap();
            assert!(landed <= want, "want {want}, landed {landed}");
            // Within one bisection step of the target.
            assert!(
                want - landed <= (MIN_SEEK_STEP / 1000) as i64 + 1,
                "want {want}, landed {landed}"
            );
            assert!(!ix.is_empty(), "the bisection must populate the index");
            assert!(ix.is_well_formed());
        }
    }

    #[test]
    fn binary_search_terminates_on_a_probe_that_never_advances() {
        let mut ix = PacketIndex::new();
        // A probe that always reports the same position with a tiny timestamp
        // is the pathological case: without the forward step it would loop.
        let got = binary_search(Timestamp::new(1_000_000), 0, u64::MAX, &mut ix, |_, _| {
            Ok(Some((0, Timestamp::new(0))))
        })
        .unwrap();
        assert_eq!(got.unwrap().pos, 0);
    }

    #[test]
    fn binary_search_reports_a_forward_landing_when_nothing_precedes() {
        let mut ix = PacketIndex::new();
        // Target before the first sync point: fall back to the first one.
        let got = binary_search(Timestamp::new(-5), 0, 100_000, &mut ix, synthetic_probe)
            .unwrap()
            .unwrap();
        assert_eq!(got.timestamp.ticks(), Some(0));
    }

    #[test]
    fn binary_search_needs_a_target() {
        let mut ix = PacketIndex::new();
        assert!(binary_search(Timestamp::NONE, 0, 10, &mut ix, synthetic_probe).is_err());
    }
}
