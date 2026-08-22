//! Edit lists: `elst`, and the presentation ↔ media time mapping it defines.
//!
//! ISO/IEC 14496-12 §8.6.6. This is where A/V sync quietly dies, for one
//! structural reason: **an `elst` entry mixes two time bases in one record**.
//! `segment_duration` counts in the *movie* timescale from `mvhd`;
//! `media_time` counts in the *media* timescale from `mdhd`. A parser that uses
//! one for the other produces a track that is out by the ratio of the two —
//! typically 12.8× for video and 44.1× for audio, which is why the symptom is
//! "the audio drifts" rather than "the file is broken".
//!
//! Nothing in this module accepts a timescale implicitly. [`EditList::resolve`]
//! takes both and returns a [`Timeline`] in media ticks, and the raw entries
//! keep their units in their field names.
//!
//! # What the reference does, measured
//!
//! `ffmpeg`/`ffprobe` 8.1, on files this crate's fixtures reproduce:
//!
//! | File | `elst` | `start_pts` | first packet |
//! |---|---|---|---|
//! | `prog.mp4` video | `[(2000 movie, 1024 media, 1.0)]` | 0 | `pts=0 dts=-1024` |
//! | `prog.mp4` audio | `[(2000, 1024, 1.0)]` | 0 | `pts=-1024`, `skip_samples=1024`, discard |
//! | `delay.mp4` video | `[(520, -1), (2000, 0)]` | 6656 | `pts=6656` |
//!
//! So a non-empty first edit with `media_time = M` shifts **both** PTS and DTS
//! by `-M`, and a leading empty edit shifts both by `+segment_duration`
//! rescaled into the media timescale. [`EditList::simple_shift`] is exactly
//! that sum, and it reproduces all three rows.
//!
//! Audio samples that fall before the edit start are **not dropped**: they are
//! emitted with a `skip_samples` trim and a discard flag. That is the
//! demuxer's business; what this module owes it is
//! [`Timeline::media_to_presentation`] returning `None` for exactly those
//! samples.

use vaco_core::{Rational, Rounding, rescale_rnd};

use crate::boxes::FullBox;

/// Largest number of `elst` entries kept.
///
/// A genuine edit decision list is a handful of entries; the largest seen in
/// the wild is in the low hundreds. The cap exists because `entry_count` is a
/// 32-bit file field and [`EditList`] is the one structure here that does
/// allocate per entry — sixteen bytes each, so 64 Ki entries is a 1 MiB ceiling
/// per track.
pub const MAX_EDIT_ENTRIES: u32 = 65_536;

/// `media_time` for an empty edit.
pub const EMPTY_EDIT: i64 = -1;

/// One `elst` entry, in the units the file states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditEntry {
    /// Length of this segment **in movie timescale ticks**.
    ///
    /// Zero means "to the end of the media" (§8.6.6.1).
    pub segment_duration: u64,
    /// Start time within the media **in media timescale ticks**, or
    /// [`EMPTY_EDIT`] for an empty edit.
    pub media_time: i64,
    /// `media_rate_integer` — the 16 integer bits of the rate.
    pub rate_integer: i16,
    /// `media_rate_fraction` — the 16 fractional bits.
    pub rate_fraction: u16,
}

impl EditEntry {
    /// Whether this is an empty edit, i.e. inserted blank time.
    #[must_use]
    pub const fn is_empty_edit(&self) -> bool {
        self.media_time < 0
    }

    /// The playback rate as an exact rational.
    #[must_use]
    pub fn rate(&self) -> Rational {
        let raw = (i32::from(self.rate_integer) << 16) | i32::from(self.rate_fraction);
        Rational::new(raw, 1 << 16)
    }

    /// Whether the rate is exactly 1.0, the only rate anything reproduces
    /// faithfully.
    #[must_use]
    pub const fn is_normal_rate(&self) -> bool {
        self.rate_integer == 1 && self.rate_fraction == 0
    }
}

/// The parsed `elst` box.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditList {
    /// The entries, in file order. Order is meaningful: it is the presentation
    /// order of the segments.
    pub entries: Vec<EditEntry>,
    /// Whether the declared entry count exceeded what the payload could hold or
    /// [`MAX_EDIT_ENTRIES`], so the list is a prefix of what the file claimed.
    pub truncated: bool,
}

impl EditList {
    /// Parse an `elst` full box.
    ///
    /// Never fails: an `elst` that runs out of bytes yields the entries that
    /// were complete and sets [`EditList::truncated`]. Refusing the whole track
    /// because its edit list is short would be a worse outcome than playing it
    /// unedited, and the flag lets the caller decide.
    #[must_use]
    pub fn parse(full: &FullBox<'_>) -> Self {
        let mut r = full.reader();
        let declared = r.be32();
        let stride = if full.version == 1 { 20 } else { 12 };
        #[allow(
            clippy::integer_division,
            reason = "stride is the constant 12 or 20; this is the clamp that makes the declared count trustworthy"
        )]
        let available =
            u32::try_from(full.body.len().saturating_sub(4) / stride).unwrap_or(u32::MAX);
        let n = declared.min(available).min(MAX_EDIT_ENTRIES);
        let mut entries = Vec::new();
        for _ in 0..n {
            let (segment_duration, media_time) = if full.version == 1 {
                (r.be64(), r.be64().cast_signed())
            } else {
                (u64::from(r.be32()), i64::from(r.be32().cast_signed()))
            };
            let rate_integer = r.be16().cast_signed();
            let rate_fraction = r.be16();
            if r.overrun() {
                break;
            }
            entries.push(EditEntry {
                segment_duration,
                media_time,
                rate_integer,
                rate_fraction,
            });
        }
        Self {
            truncated: declared > n,
            entries,
        }
    }

    /// Whether the list says nothing, i.e. play the media as stored.
    #[must_use]
    pub fn is_trivial(&self) -> bool {
        self.entries.is_empty()
            || (self.entries.len() == 1
                && self
                    .entries
                    .first()
                    .is_some_and(|e| e.media_time == 0 && e.is_normal_rate()))
    }

    /// Whether any entry asks for a rate other than 1.0.
    ///
    /// # Known divergence
    ///
    /// Rate-1 edits are implemented exactly. Any other rate is a speed change
    /// which this crate reports and does not apply — the [`Timeline`] treats it
    /// as rate 1. `planning/18-formats.md` §3.1.5 E4 records that the reference
    /// is also approximate here; the divergence is flagged rather than hidden
    /// so a caller can warn.
    #[must_use]
    pub fn has_unsupported_rate(&self) -> bool {
        self.entries
            .iter()
            .any(|e| !e.is_empty_edit() && !e.is_normal_rate())
    }

    /// Leading empty-edit duration, in **movie** ticks.
    #[must_use]
    pub fn empty_offset_movie(&self) -> u64 {
        let mut total = 0u64;
        for e in &self.entries {
            if e.is_empty_edit() {
                total = total.saturating_add(e.segment_duration);
            } else {
                break;
            }
        }
        total
    }

    /// `media_time` of the first non-empty edit, or zero when there is none.
    #[must_use]
    pub fn initial_media_time(&self) -> i64 {
        self.entries
            .iter()
            .find(|e| !e.is_empty_edit())
            .map_or(0, |e| e.media_time)
    }

    /// Sum of every segment's duration, in **movie** ticks.
    #[must_use]
    pub fn total_duration_movie(&self) -> u64 {
        self.entries
            .iter()
            .fold(0u64, |a, e| a.saturating_add(e.segment_duration))
    }

    /// Sum of the **non-empty** segments' durations, in movie ticks.
    ///
    /// Measured: this is what `ffprobe 8.1` reports as `duration_ts` once
    /// rescaled into the media timescale. On `delay.mp4`, whose video `elst` is
    /// `[(520, -1), (2000, 0)]`, `duration_ts` is 25 600 — that is 2000 movie
    /// ticks at 12 800/1000, and *not* 2520, so the empty edit is excluded.
    #[must_use]
    pub fn played_duration_movie(&self) -> u64 {
        self.entries
            .iter()
            .filter(|e| !e.is_empty_edit())
            .fold(0u64, |a, e| a.saturating_add(e.segment_duration))
    }

    /// The single shift a demuxer applies to every PTS and DTS on the track,
    /// in **media** ticks.
    ///
    /// `empty_offset - initial_media_time`, which reproduces every case the
    /// reference was measured on. Correct whenever the list is a leading run of
    /// empty edits followed by one non-empty edit — which is every file any
    /// mainstream tool writes. For a genuine multi-segment edit decision list
    /// use [`EditList::resolve`] instead; [`EditList::is_simple`] says which
    /// applies.
    #[must_use]
    pub fn simple_shift(&self, movie_timescale: u32, media_timescale: u32) -> i64 {
        let empty = rescale_movie_to_media(
            i64::try_from(self.empty_offset_movie()).unwrap_or(i64::MAX),
            movie_timescale,
            media_timescale,
        );
        empty.saturating_sub(self.initial_media_time())
    }

    /// Whether [`EditList::simple_shift`] is sufficient: at most one non-empty
    /// segment, all at rate 1.
    #[must_use]
    pub fn is_simple(&self) -> bool {
        !self.has_unsupported_rate()
            && self.entries.iter().filter(|e| !e.is_empty_edit()).count() <= 1
    }

    /// Resolve the list into media-timescale segments.
    ///
    /// `media_duration` is `mdhd.duration`, needed because a
    /// `segment_duration` of zero means "to the end of the media" and nothing
    /// in the `elst` says where that is.
    #[must_use]
    pub fn resolve(
        &self,
        movie_timescale: u32,
        media_timescale: u32,
        media_duration: i64,
    ) -> Timeline {
        let mut segments = Vec::new();
        let mut presentation = 0i64;
        for e in &self.entries {
            let stated = rescale_movie_to_media(
                i64::try_from(e.segment_duration).unwrap_or(i64::MAX),
                movie_timescale,
                media_timescale,
            );
            let media_start = if e.is_empty_edit() {
                None
            } else {
                Some(e.media_time)
            };
            let duration = if e.segment_duration == 0 {
                // "To the end of the media", measured from this segment's own
                // start rather than from zero.
                media_duration
                    .saturating_sub(media_start.unwrap_or(0))
                    .max(0)
            } else {
                stated
            };
            segments.push(Segment {
                presentation_start: presentation,
                duration,
                media_start,
                rate: e.rate(),
            });
            presentation = presentation.saturating_add(duration);
        }
        Timeline {
            segments,
            total: presentation,
        }
    }
}

/// One resolved segment, entirely in media-timescale ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    /// Where the segment starts on the presented timeline.
    pub presentation_start: i64,
    /// How long it lasts on the presented timeline.
    pub duration: i64,
    /// Where it starts in the media, or `None` for an empty edit.
    pub media_start: Option<i64>,
    /// The requested playback rate; only 1.0 is applied.
    pub rate: Rational,
}

impl Segment {
    /// One past the last presented tick.
    #[must_use]
    pub const fn presentation_end(&self) -> i64 {
        self.presentation_start.saturating_add(self.duration)
    }

    /// One past the last media tick this segment draws from, when it draws from
    /// the media at all.
    #[must_use]
    pub fn media_end(&self) -> Option<i64> {
        Some(self.media_start?.saturating_add(self.duration))
    }
}

/// A resolved edit list: the presented timeline, in media ticks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Timeline {
    segments: Vec<Segment>,
    total: i64,
}

impl Timeline {
    /// A timeline that presents the media unchanged, for a track with no
    /// `elst`.
    #[must_use]
    pub fn identity(media_duration: i64) -> Self {
        Self {
            segments: vec![Segment {
                presentation_start: 0,
                duration: media_duration,
                media_start: Some(0),
                rate: Rational::new(1, 1),
            }],
            total: media_duration,
        }
    }

    /// The segments, in presentation order.
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Total presented duration, in media ticks.
    #[must_use]
    pub const fn duration(&self) -> i64 {
        self.total
    }

    /// Where the presented timeline starts, i.e. the leading empty-edit run.
    #[must_use]
    pub fn start_offset(&self) -> i64 {
        self.segments
            .iter()
            .take_while(|s| s.media_start.is_none())
            .fold(0i64, |a, s| a.saturating_add(s.duration))
    }

    /// Map a media time onto the presented timeline.
    ///
    /// `None` when no segment covers it, which is exactly the "trimmed away"
    /// case — the samples an audio track presents as `skip_samples` and a video
    /// track drops.
    ///
    /// Where several segments cover the same media time (a legal edit list may
    /// repeat a region), the **first** in presentation order wins.
    #[must_use]
    pub fn media_to_presentation(&self, media: i64) -> Option<i64> {
        for s in &self.segments {
            let Some(start) = s.media_start else { continue };
            if media >= start && media < start.saturating_add(s.duration) {
                return Some(
                    s.presentation_start
                        .saturating_add(media.saturating_sub(start)),
                );
            }
        }
        None
    }

    /// Map a presented time back into the media.
    ///
    /// `None` inside an empty edit — there is no media there — and past the
    /// end of the timeline.
    #[must_use]
    pub fn presentation_to_media(&self, presented: i64) -> Option<i64> {
        for s in &self.segments {
            if presented >= s.presentation_start && presented < s.presentation_end() {
                let start = s.media_start?;
                return Some(start.saturating_add(presented.saturating_sub(s.presentation_start)));
            }
        }
        None
    }

    /// The first media time the timeline presents.
    #[must_use]
    pub fn first_media_time(&self) -> Option<i64> {
        self.segments.iter().find_map(|s| s.media_start)
    }
}

/// Rescale a value from the movie timescale into the media timescale.
///
/// Exact through `i128`, saturating on overflow. Round-to-nearest matches every
/// case measured against the reference; all of those were exact, so the mode is
/// a choice rather than a reproduction and is recorded as one.
#[must_use]
pub fn rescale_movie_to_media(v: i64, movie_timescale: u32, media_timescale: u32) -> i64 {
    if movie_timescale == 0 {
        return 0;
    }
    rescale_rnd(
        v,
        i64::from(media_timescale),
        i64::from(movie_timescale),
        Rounding::NearestAwayFromZero,
    )
    .unwrap_or(if v < 0 { i64::MIN } else { i64::MAX })
}

/// The inverse of [`rescale_movie_to_media`].
#[must_use]
pub fn rescale_media_to_movie(v: i64, media_timescale: u32, movie_timescale: u32) -> i64 {
    rescale_movie_to_media(v, media_timescale, movie_timescale)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]
mod tests {
    use super::*;
    use crate::testutil::{first_box, fullbx};

    fn elst_v0(entries: &[(u32, i32, i16, u16)]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&u32::try_from(entries.len()).unwrap().to_be_bytes());
        for (d, m, ri, rf) in entries {
            b.extend_from_slice(&d.to_be_bytes());
            b.extend_from_slice(&m.to_be_bytes());
            b.extend_from_slice(&ri.to_be_bytes());
            b.extend_from_slice(&rf.to_be_bytes());
        }
        fullbx(b"elst", 0, 0, &b)
    }

    fn parse(raw: &[u8]) -> EditList {
        EditList::parse(&first_box(raw).full().unwrap())
    }

    #[test]
    fn the_measured_progressive_case_shifts_by_minus_media_time() {
        // prog.mp4: elst [(2000 movie, 1024 media, 1.0)], mvhd ts 1000,
        // mdhd ts 12800. ffprobe: start_pts=0, first packet pts=0 dts=-1024.
        let el = parse(&elst_v0(&[(2000, 1024, 1, 0)]));
        assert_eq!(el.entries.len(), 1);
        assert_eq!(el.simple_shift(1000, 12_800), -1024);
        assert!(el.is_simple());
        assert!(!el.has_unsupported_rate());
        // duration_ts: 2000 movie ticks at 12800/1000 = 25600.
        assert_eq!(
            rescale_movie_to_media(el.played_duration_movie().cast_signed(), 1000, 12_800),
            25_600
        );
    }

    #[test]
    fn the_measured_delayed_case_shifts_forward_by_the_empty_edit() {
        // delay.mp4 video: elst [(520, -1), (2000, 0)]. ffprobe start_pts=6656.
        let el = parse(&elst_v0(&[(520, -1, 1, 0), (2000, 0, 1, 0)]));
        assert_eq!(el.empty_offset_movie(), 520);
        assert_eq!(el.initial_media_time(), 0);
        assert_eq!(el.simple_shift(1000, 12_800), 6656);
        assert!(el.is_simple());
        // And the played duration excludes the empty edit: 25600, not 32256.
        assert_eq!(
            rescale_movie_to_media(el.played_duration_movie().cast_signed(), 1000, 12_800),
            25_600
        );
        assert_eq!(el.total_duration_movie(), 2520);
    }

    #[test]
    fn the_measured_audio_case_puts_trimmed_samples_outside_the_timeline() {
        // prog.mp4 audio: elst [(2000, 1024, 1.0)], mdhd ts 44100.
        let el = parse(&elst_v0(&[(2000, 1024, 1, 0)]));
        let tl = el.resolve(1000, 44_100, 89_224);
        // Sample 0 sits at media time 0, before the edit: not presented.
        assert_eq!(tl.media_to_presentation(0), None);
        // Sample 1 sits at 1024, the first presented sample.
        assert_eq!(tl.media_to_presentation(1024), Some(0));
        assert_eq!(tl.media_to_presentation(2048), Some(1024));
        assert_eq!(tl.first_media_time(), Some(1024));
        assert_eq!(tl.start_offset(), 0);
        assert_eq!(tl.duration(), 88_200);
    }

    #[test]
    fn timescale_confusion_is_caught_by_the_two_arguments() {
        // The bug this module exists to prevent: 520 is a movie-ticks value.
        // Reading it as media ticks gives 520, not 6656.
        assert_eq!(rescale_movie_to_media(520, 1000, 12_800), 6656);
        assert_eq!(rescale_movie_to_media(520, 1000, 44_100), 22_932);
        assert_eq!(rescale_media_to_movie(6656, 12_800, 1000), 520);
    }

    #[test]
    fn a_zero_movie_timescale_does_not_divide_by_zero() {
        assert_eq!(rescale_movie_to_media(1000, 0, 12_800), 0);
        let el = parse(&elst_v0(&[(100, -1, 1, 0)]));
        assert_eq!(el.simple_shift(0, 12_800), 0);
    }

    #[test]
    fn segment_duration_zero_runs_to_the_end_of_the_media() {
        let el = parse(&elst_v0(&[(0, 500, 1, 0)]));
        let tl = el.resolve(1000, 1000, 4000);
        assert_eq!(tl.segments()[0].duration, 3500);
        assert_eq!(tl.duration(), 3500);
        assert_eq!(tl.media_to_presentation(500), Some(0));
        assert_eq!(tl.media_to_presentation(3999), Some(3499));
        assert_eq!(tl.media_to_presentation(4000), None);
    }

    #[test]
    fn a_multi_segment_edit_decision_list_reorders_the_media() {
        // Present the second half first, then the first half.
        let el = parse(&elst_v0(&[(500, 500, 1, 0), (500, 0, 1, 0)]));
        let tl = el.resolve(1000, 1000, 1000);
        assert_eq!(tl.segments().len(), 2);
        assert_eq!(tl.media_to_presentation(500), Some(0));
        assert_eq!(tl.media_to_presentation(0), Some(500));
        assert_eq!(tl.presentation_to_media(0), Some(500));
        assert_eq!(tl.presentation_to_media(500), Some(0));
        assert!(!el.is_simple());
    }

    #[test]
    fn an_empty_edit_has_no_media_underneath_it() {
        let el = parse(&elst_v0(&[(300, -1, 1, 0), (700, 0, 1, 0)]));
        let tl = el.resolve(1000, 1000, 700);
        assert_eq!(tl.start_offset(), 300);
        assert_eq!(tl.presentation_to_media(100), None);
        assert_eq!(tl.presentation_to_media(300), Some(0));
        assert_eq!(tl.presentation_to_media(999), Some(699));
        assert_eq!(tl.presentation_to_media(1000), None);
    }

    #[test]
    fn version_one_entries_are_sixty_four_bit() {
        let mut b = Vec::new();
        b.extend_from_slice(&1u32.to_be_bytes());
        b.extend_from_slice(&0x1_0000_0000u64.to_be_bytes());
        b.extend_from_slice(&(-1i64).to_be_bytes());
        b.extend_from_slice(&1i16.to_be_bytes());
        b.extend_from_slice(&0u16.to_be_bytes());
        let raw = fullbx(b"elst", 1, 0, &b);
        let el = parse(&raw);
        assert_eq!(el.entries[0].segment_duration, 0x1_0000_0000);
        assert!(el.entries[0].is_empty_edit());
        assert!(!el.truncated);
    }

    #[test]
    fn a_declared_count_larger_than_the_payload_truncates_instead_of_allocating() {
        let mut b = Vec::new();
        b.extend_from_slice(&u32::MAX.to_be_bytes());
        b.extend_from_slice(&[0; 12]);
        let raw = fullbx(b"elst", 0, 0, &b);
        let el = parse(&raw);
        assert_eq!(el.entries.len(), 1);
        assert!(el.truncated);
    }

    #[test]
    fn an_unsupported_rate_is_reported_not_applied() {
        let el = parse(&elst_v0(&[(1000, 0, 2, 0)]));
        assert!(el.has_unsupported_rate());
        assert!(!el.is_simple());
        assert_eq!(el.entries[0].rate(), Rational::new(2 << 16, 1 << 16));
        // The timeline still maps 1:1, as documented.
        let tl = el.resolve(1000, 1000, 2000);
        assert_eq!(tl.media_to_presentation(10), Some(10));
    }

    #[test]
    fn a_trivial_list_is_recognised() {
        assert!(EditList::default().is_trivial());
        assert!(parse(&elst_v0(&[(1000, 0, 1, 0)])).is_trivial());
        assert!(!parse(&elst_v0(&[(1000, 5, 1, 0)])).is_trivial());
    }

    #[test]
    fn the_identity_timeline_presents_everything() {
        let tl = Timeline::identity(1000);
        assert_eq!(tl.media_to_presentation(0), Some(0));
        assert_eq!(tl.media_to_presentation(999), Some(999));
        assert_eq!(tl.media_to_presentation(1000), None);
        assert_eq!(tl.start_offset(), 0);
    }

    #[test]
    fn saturating_durations_do_not_overflow_the_timeline() {
        let el = parse(&elst_v0(&[(u32::MAX, 0, 1, 0), (u32::MAX, 0, 1, 0)]));
        let tl = el.resolve(1, i32::MAX as u32, i64::MAX);
        assert!(tl.duration() > 0);
        assert_eq!(el.total_duration_movie(), u64::from(u32::MAX) * 2);
    }
}
