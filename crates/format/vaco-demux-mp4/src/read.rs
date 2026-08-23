//! Per-track sample readers, and the rule that decides which packet is next.
//!
//! # Why samples are produced in batches
//!
//! `vaco-format-isom`'s tables **borrow** the `moov` bytes: `SampleTable<'a>`
//! is a set of `&'a [u8]` slices plus decimated summaries, which is what makes
//! it allocate nothing proportional to the sample count. A `Box<dyn Demuxer>`
//! is `'static`, so the demuxer owns those bytes and cannot also hold a
//! structure that borrows them — safe Rust has no way to express that without
//! a self-referential helper crate (none is in `[workspace.dependencies]`) or
//! leaking the buffer.
//!
//! The resolution is to re-parse the one track's `stbl` per **refill** and take
//! a batch of samples into an owned queue. Parsing is O(samples) — measured at
//! 1.3 ns per sample by that crate's own benchmark — so a fixed batch would be
//! quadratic in the sample count. The batch therefore grows geometrically from
//! [`BATCH_MIN`] to [`BATCH_MAX`], which bounds the number of refills at
//! `log2(BATCH_MAX / BATCH_MIN) + count / BATCH_MAX` and the total re-parse
//! work at a small multiple of one parse.
//!
//! Reported rather than worked around: the natural fix is for
//! `vaco-format-isom` to expose an owned, resumable cursor state (`(index, dts,
//! chunk, within)` are all it needs), which would remove the re-parse
//! completely. See the crate's doc file.

use std::collections::VecDeque;

use vaco_core::Rational;
use vaco_format_isom::frag::{TrackExtends, TrackFragment};
use vaco_format_isom::stbl::{Sample, SampleTable};

/// Smallest refill batch, in samples.
pub(crate) const BATCH_MIN: u32 = 4096;
/// Largest refill batch, in samples. 128 Ki × 48 B ≈ 6 MiB per track.
pub(crate) const BATCH_MAX: u32 = 128 * 1024;
/// Samples taken from one track fragment per refill.
pub(crate) const FRAG_BATCH: u32 = 16 * 1024;

/// Hard ceiling on the samples one track will ever be walked for.
///
/// The real bound is the file size — see [`sample_limit`] — and this is the
/// backstop for a source that cannot state one.
pub(crate) const MAX_SAMPLES_PER_TRACK: u32 = 1 << 24;

/// Hard ceiling on the samples one `traf` will ever be walked for.
pub(crate) const MAX_SAMPLES_PER_FRAGMENT: u32 = 1 << 20;

/// How many samples of a track are worth walking.
///
/// **This is the bound `vaco-format-isom` deliberately left to the demuxer.** A
/// uniform `stsz` is the one declared count in the format with no payload to
/// clamp it against: twelve bytes can legally say `sample_count =
/// 0xFFFF_FFFF`. Nothing allocates for it, but iterating it is a denial of
/// service on a seventy-byte file.
///
/// The bound that is *correct* rather than arbitrary is the source's own size.
/// Distinct samples of one track occupy disjoint byte ranges, and a sample
/// occupies at least one byte, so a file of `n` bytes holds at most `n`
/// samples. A variable `stsz` is already clamped by its own payload; this
/// closes the uniform case with the same kind of argument rather than with a
/// magic number.
#[must_use]
pub(crate) fn sample_limit(declared: u32, source_size: Option<u64>) -> u32 {
    let by_size = source_size
        .and_then(|n| u32::try_from(n.saturating_add(1)).ok())
        .unwrap_or(u32::MAX);
    declared.min(by_size).min(MAX_SAMPLES_PER_TRACK)
}

/// One resolved sample, waiting to be turned into a packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pending {
    pub offset: u64,
    pub size: u32,
    pub dts: i64,
    pub pts: i64,
    pub duration: u32,
    pub key: bool,
    pub discard: bool,
    /// Leading samples to trim, in time-base ticks — which for an MP4 audio
    /// track are samples, because its time base is `1 / sample_rate`.
    pub skip: u32,
}

/// One track fragment belonging to one track, located but not yet resolved.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FragEntry {
    /// Index into the demuxer's fragment list.
    pub fragment: usize,
    /// Index of the `traf` inside that `moof`.
    pub traf: usize,
    /// Decode time of its first sample.
    pub start_dts: i64,
    /// Samples it declares, already bounded.
    pub samples: u32,
    /// Offset of its first sample, for the tie-break rule and for seeking.
    pub first_offset: u64,
}

/// Where a track's samples come from.
#[derive(Debug)]
pub(crate) enum Source {
    /// The movie's own sample table.
    Table {
        /// Index into `Movie::tracks`, stable because `Movie::parse` skips a
        /// broken `trak` deterministically.
        slot: usize,
        next: u32,
        limit: u32,
    },
    /// Movie fragments.
    Fragments { entry: usize, next_in_entry: u32 },
    /// A single `covr` image, which has no timeline at all.
    AttachedPic {
        offset: u64,
        size: u32,
        emitted: bool,
    },
}

/// One track's read state.
#[derive(Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each names an independent axis a real file can combine — audio-ness, exhaustion, \
              permanent refusal, encryption — not a state machine with excluded combinations"
)]
pub(crate) struct Reader {
    pub stream_index: u32,
    pub time_base: Rational,
    pub audio: bool,
    /// `min(0, min(ctts))`, or `cslg`. Applied to DTS only — a D17 deviation
    /// reproduced from the reference; see `vaco-format-isom`'s doc file.
    pub dts_shift: i64,
    /// The edit list's single shift, applied to both PTS and DTS.
    pub edit_shift: i64,
    /// Where the presented timeline starts; samples before it are trimmed.
    pub trim_point: i64,
    pub source: Source,
    pub entries: Vec<FragEntry>,
    pub queue: VecDeque<Pending>,
    pub batch: u32,
    pub finished: bool,
    /// Permanently unreadable, as opposed to merely exhausted.
    ///
    /// A track whose `dref` points at another file is refused (plan 18
    /// §3.1.10), and a seek must not resurrect it. Keeping the two states in
    /// one `finished` flag did exactly that: `place` clears `finished` because
    /// a seek moves an exhausted reader back into the file, and it cleared the
    /// refusal with it. Found by the `dem_mp4` fuzz target, as "a seek produced
    /// a packet a straight read never did".
    pub blocked: bool,
    /// `sinf ▸ schm`/`sinf ▸ schi ▸ tenc` named a Common Encryption scheme.
    ///
    /// Deliberately **not** folded into `blocked`: a blocked track silently
    /// produces no packets forever, which is right for an unreachable `dref`
    /// but wrong here — decryption is out of scope by design (see the crate's
    /// doc file), and a caller who asks for packets from an encrypted track
    /// should be told why it cannot have any, not handed an empty stream that
    /// looks the same as a track with nothing in it. `ensure_head` turns this
    /// into an [`vaco_core::Error::Unsupported`] the first time any packet is
    /// requested, once — for every track, so a mixed encrypted/clear file
    /// fails predictably rather than only once the encrypted track's turn in
    /// the interleave happens to come up.
    pub encrypted: bool,
}

impl Reader {
    /// The next sample, without consuming it.
    pub(crate) fn head(&self) -> Option<&Pending> {
        self.queue.front()
    }

    /// Grow the batch for the next refill, bounded.
    fn grow(&mut self) {
        self.batch = self.batch.saturating_mul(2).min(BATCH_MAX);
    }

    /// Turn a media-timescale sample into a queue entry.
    fn push(&mut self, offset: u64, size: u32, dts: i64, cts: i32, duration: u32, key: bool) {
        let dts_out = dts
            .saturating_add(self.dts_shift)
            .saturating_add(self.edit_shift);
        let pts_out = dts
            .saturating_add(i64::from(cts))
            .saturating_add(self.edit_shift);
        let end = pts_out.saturating_add(i64::from(duration));
        let discard = end <= self.trim_point && duration > 0;
        let skip = if self.audio {
            self.trim_point
                .saturating_sub(pts_out)
                .clamp(0, i64::from(duration)) as u32
        } else {
            0
        };
        self.queue.push_back(Pending {
            offset,
            size,
            dts: dts_out,
            pts: pts_out,
            duration,
            key,
            discard,
            skip,
        });
    }
}

/// Fill `reader`'s queue from the movie's sample table.
///
/// Returns whether the reader **advanced** — which is not the same as whether
/// it produced a packet. A batch whose samples all lie outside the file yields
/// nothing and is still progress, because the cursor moved; reporting it as a
/// stall is how a truncated file turns into a spurious "no progress" error
/// several thousand samples before the end of its table. Found by the
/// `dem_mp4` fuzz target on a truncated fragmented file, which read zero
/// packets straight through and a dozen after a seek.
///
/// Borrows are split by the caller:
/// `table` is derived from the demuxer's `moov` field while `reader` comes from
/// its `readers` field, and the two are disjoint.
pub(crate) fn refill_table(
    reader: &mut Reader,
    table: &SampleTable<'_>,
    source_size: Option<u64>,
) -> bool {
    let Source::Table { next, limit, .. } = &mut reader.source else {
        return false;
    };
    let (start, limit) = (*next, *limit);
    if start >= limit {
        reader.finished = true;
        return false;
    }
    let want = reader.batch.min(limit.saturating_sub(start));
    let mut last = start;
    let samples: Vec<Sample> = table
        .cursor_at(start)
        .take_while(|s| s.index < limit)
        .take(want as usize)
        .collect();
    let got = samples.len();
    for s in &samples {
        let s = *s;
        last = s.index;
        // A sample that does not fit inside the source is not readable. Plan 18
        // §3.1.10: they are dropped, and `nb_frames` still reports the table's
        // count.
        let fits = source_size.is_none_or(|n| s.offset.saturating_add(u64::from(s.size)) <= n);
        if fits {
            reader.push(s.offset, s.size, s.dts, s.cts_offset, s.duration, s.is_sync);
        }
    }
    // A cursor that yielded less than a full batch has run out: the table
    // cannot resolve any later sample either, so this is the end rather than a
    // gap to walk over one index at a time.
    let exhausted = got < want as usize;
    let advanced = last
        .saturating_add(1)
        .max(start.saturating_add(want))
        .min(limit);
    if let Source::Table { next, .. } = &mut reader.source {
        *next = advanced;
        if exhausted || advanced >= limit {
            reader.finished = true;
        }
    }
    reader.grow();
    true
}

/// Fill `reader`'s queue from one track fragment.
pub(crate) fn refill_fragment(
    reader: &mut Reader,
    traf: &TrackFragment<'_>,
    base: u64,
    defaults: &TrackExtends,
    source_size: Option<u64>,
) -> bool {
    let Source::Fragments {
        entry,
        next_in_entry,
    } = reader.source
    else {
        return false;
    };
    let Some(e) = reader.entries.get(entry).copied() else {
        return false;
    };
    if next_in_entry >= e.samples {
        return false;
    }
    let want = FRAG_BATCH.min(e.samples.saturating_sub(next_in_entry));
    let resolved: Vec<_> = traf
        .samples(base, e.start_dts, defaults)
        .skip(next_in_entry as usize)
        .take(want as usize)
        .collect();
    for s in resolved {
        let fits = source_size.is_none_or(|n| s.offset.saturating_add(u64::from(s.size)) <= n);
        if fits {
            reader.push(
                s.offset,
                s.size,
                s.dts,
                s.cts_offset,
                s.duration,
                s.is_sync(),
            );
        }
    }
    if let Some(Source::Fragments { next_in_entry, .. }) = Some(&mut reader.source) {
        *next_in_entry = next_in_entry.saturating_add(want);
    }
    true
}

/// Advance to the next track fragment, if this track has one.
pub(crate) fn advance_fragment(reader: &mut Reader) -> bool {
    let Source::Fragments {
        entry,
        next_in_entry,
    } = &mut reader.source
    else {
        return false;
    };
    let done = reader
        .entries
        .get(*entry)
        .is_none_or(|e| *next_in_entry >= e.samples);
    if !done {
        return true;
    }
    if entry.saturating_add(1) < reader.entries.len() {
        *entry = entry.saturating_add(1);
        *next_in_entry = 0;
        true
    } else {
        false
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn a_uniform_stsz_is_bounded_by_the_source_size() {
        // Twelve bytes can declare four billion samples; a seventy-eight-byte
        // file holds at most seventy-nine.
        assert_eq!(sample_limit(u32::MAX, Some(78)), 79);
        assert_eq!(sample_limit(4, Some(78)), 4);
        assert_eq!(sample_limit(u32::MAX, None), MAX_SAMPLES_PER_TRACK);
        assert_eq!(
            sample_limit(u32::MAX, Some(u64::MAX)),
            MAX_SAMPLES_PER_TRACK
        );
    }

    fn reader() -> Reader {
        Reader {
            stream_index: 0,
            time_base: Rational::new(1, 1000),
            audio: true,
            dts_shift: 0,
            edit_shift: -1024,
            trim_point: 0,
            source: Source::Table {
                slot: 0,
                next: 0,
                limit: 8,
            },
            entries: Vec::new(),
            queue: VecDeque::new(),
            batch: BATCH_MIN,
            finished: false,
            blocked: false,
            encrypted: false,
        }
    }

    #[test]
    fn a_sample_entirely_before_the_edit_is_discarded_and_trimmed() {
        let mut r = reader();
        r.push(0, 4, 0, 0, 1024, true);
        let p = r.queue.front().copied().unwrap();
        assert_eq!(p.pts, -1024);
        assert_eq!(p.dts, -1024);
        assert!(p.discard);
        assert_eq!(p.skip, 1024);
    }

    #[test]
    fn a_sample_straddling_the_edit_is_trimmed_but_kept() {
        let mut r = reader();
        r.edit_shift = -512;
        r.push(0, 4, 0, 0, 1024, true);
        let p = r.queue.front().copied().unwrap();
        assert_eq!(p.pts, -512);
        assert!(!p.discard);
        assert_eq!(p.skip, 512);
    }

    #[test]
    fn a_video_sample_is_never_given_a_sample_trim() {
        let mut r = reader();
        r.audio = false;
        r.push(0, 4, 0, 0, 1024, true);
        let p = r.queue.front().copied().unwrap();
        assert!(p.discard);
        assert_eq!(p.skip, 0);
    }

    #[test]
    fn the_composition_offset_moves_pts_but_not_dts() {
        let mut r = reader();
        r.edit_shift = 0;
        r.dts_shift = -512;
        r.push(0, 4, 1024, 256, 512, false);
        let p = r.queue.front().copied().unwrap();
        assert_eq!(p.dts, 512, "dts carries the ctts-derived shift");
        assert_eq!(p.pts, 1280, "pts carries the composition offset");
    }

    #[test]
    fn the_batch_grows_geometrically_and_stops() {
        let mut r = reader();
        for _ in 0..64 {
            r.grow();
        }
        assert_eq!(r.batch, BATCH_MAX);
    }
}
