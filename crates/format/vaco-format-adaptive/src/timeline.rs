//! The segment-timeline model both formats reduce to.
//!
//! DASH states a timeline directly, as `SegmentTimeline`'s run-length-encoded
//! `<S t="…" d="…" r="…"/>` elements (ISO/IEC 23009-1 §5.3.9.6). HLS states one
//! implicitly: each `#EXTINF:<duration>` line is one more entry whose start is
//! the running sum of everything before it, with an `#EXT-X-DISCONTINUITY`
//! marking a break the way DASH marks one with a new `Period` or a timeline
//! entry whose stated `@t` does not follow the previous one's `@t + @d`.
//!
//! [`expand`] turns the DASH form into the common [`SegmentTiming`] sequence.
//! The HLS demuxer builds the same sequence directly, one `EXTINF` at a time,
//! since there is no run-length encoding to expand — see
//! `vaco_demux_hls::playlist::media::timing`.
//!
//! # The part that is genuinely fiddly
//!
//! `@r` is a **repeat count**: `<S t="0" d="1000" r="4"/>` is five segments
//! (the stated one plus four repeats), each `d` apart. `@r="0"` (or omitted) is
//! one segment. `@r="-1"` means "repeat until the *next* `S` element's `@t`, or
//! until the period ends if this is the last `S`" — a count that is not stated
//! anywhere in this element and depends on information the caller has to
//! supply. Getting this wrong produces a stream that plays and drifts out of
//! sync a few segments in, which is a worse failure than refusing to parse.
//!
//! `@t` is also optional on every entry after the first: an omitted `@t` means
//! "immediately after the previous entry's last segment", i.e.
//! `previous.start + previous.duration`. The very first entry's `@t` defaults
//! to 0 when omitted.

use vaco_core::{Error, Result};
use vaco_limits::Budget;

/// One `<S>` element, before expansion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineEntry {
    /// `@t`. `None` when the attribute was omitted.
    pub t: Option<u64>,
    /// `@d`. Always stated; zero is nonsensical but not this layer's business
    /// to reject (the caller may want to report the manifest as invalid with
    /// more context than this function has).
    pub d: u64,
    /// `@r`. `None`/absent and `Some(0)` both mean "no repeats"; `Some(-1)`
    /// means "repeat until the next entry's `@t`, or until `period_end`".
    pub r: Option<i64>,
}

/// One expanded segment: an absolute start and a duration, both in the
/// timeline's own timescale ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentTiming {
    pub start: u64,
    pub duration: u64,
}

/// Upper bound on segments a single [`expand`] call will produce.
///
/// The `DoS` this guards: `<S t="0" d="1" r="18446744073709551615"/>` is under
/// 40 bytes of XML and states 2^64 one-tick segments. Chosen generously above
/// any real manifest — a day of one-second segments is 86,400 — while still
/// being small enough that building the `Vec` costs microseconds, not an
/// unbounded amount of caller time.
pub const MAX_SEGMENTS: usize = 1 << 20;

/// Expand a `SegmentTimeline`'s `<S>` run into absolute `(start, duration)`
/// pairs.
///
/// `period_end`, in the same timescale ticks, resolves a final entry's
/// `r="-1"`; pass `None` for a live (dynamic) manifest whose period has no
/// stated end, in which case a trailing open repeat produces zero additional
/// segments — there is nothing to bound it to, and inventing segments a live
/// manifest has not announced yet would be worse than reporting what is
/// certain.
///
/// # Errors
/// [`Error::LimitExceeded`] when expansion would exceed [`MAX_SEGMENTS`] or the
/// caller's own `budget`, whichever is smaller — this is the bound the
/// module-level docs describe.
pub fn expand(
    entries: &[TimelineEntry],
    period_end: Option<u64>,
    budget: &mut Budget,
) -> Result<Vec<SegmentTiming>> {
    let cap = budget.available().min(MAX_SEGMENTS as u64);
    let mut out: Vec<SegmentTiming> = Vec::new();
    let mut cursor: u64 = 0;
    for (i, entry) in entries.iter().enumerate() {
        if entry.d == 0 {
            // A zero-duration run cannot be expanded (nothing advances the
            // cursor) and is never valid DASH; refuse rather than loop.
            return Err(Error::InvalidData("SegmentTimeline <S> has zero duration"));
        }
        let start0 = entry.t.unwrap_or(cursor);
        let repeat = entry.r.unwrap_or(0);
        let count: u64 = if repeat >= 0 {
            // The stated segment plus `repeat` more.
            u64::try_from(repeat).unwrap_or(u64::MAX).saturating_add(1)
        } else {
            // r == -1 (any negative value is treated as -1, per the spec's own
            // "less than zero" reading rather than requiring the literal -1):
            // repeat until the next entry's stated `@t`, or until `period_end`.
            let bound = entries.get(i + 1).and_then(|next| next.t).or(period_end);
            match bound {
                Some(end) if end > start0 => {
                    let span = end - start0;
                    // Ceiling division: a final short segment is legal and the
                    // reference authors it rather than dropping the remainder.
                    span.div_ceil(entry.d)
                }
                _ => 0,
            }
        };
        budget.consume_fuel(count)?;
        if out.len().saturating_add(count as usize) as u64 > cap {
            return Err(Error::LimitExceeded {
                limit: "adaptive_timeline_segments",
                requested: out.len() as u64 + count,
                cap,
            });
        }
        let mut start = start0;
        for _ in 0..count {
            out.push(SegmentTiming {
                start,
                duration: entry.d,
            });
            start = start.saturating_add(entry.d);
        }
        cursor = start;
    }
    Ok(out)
}

/// Fold a run of `SegmentTiming` back into the minimal `TimelineEntry` run
/// that [`expand`] would reproduce, collapsing equal-duration consecutive runs
/// into one `@r`-repeated entry.
///
/// This is [`expand`]'s inverse for the proptest below, and it is also
/// realistic: it is what a DASH muxer does to write a compact
/// `SegmentTimeline` instead of one `<S>` per segment.
#[must_use]
pub fn compact(timings: &[SegmentTiming]) -> Vec<TimelineEntry> {
    let mut out: Vec<TimelineEntry> = Vec::new();
    for &SegmentTiming { start, duration } in timings {
        if let Some(last) = out.last_mut() {
            // Safe to unwrap-free re-derive: every entry this function itself
            // produced carries an explicit `t` and a `d`.
            if let (Some(t), Some(r)) = (last.t, last.r) {
                let r = r.max(0);
                let prev_start = t + (r as u64) * last.d;
                if last.d == duration && prev_start.saturating_add(last.d) == start {
                    last.r = Some(r + 1);
                    continue;
                }
            }
        }
        out.push(TimelineEntry {
            t: Some(start),
            d: duration,
            r: Some(0),
        });
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn budget() -> Budget {
        Budget::new(vaco_limits::Limits::permissive())
    }

    #[test]
    fn no_repeat_is_one_segment() {
        let entries = [TimelineEntry {
            t: Some(0),
            d: 1000,
            r: None,
        }];
        let out = expand(&entries, None, &mut budget()).unwrap();
        assert_eq!(
            out,
            vec![SegmentTiming {
                start: 0,
                duration: 1000
            }]
        );
    }

    #[test]
    fn positive_repeat_counts_the_stated_segment_once() {
        let entries = [TimelineEntry {
            t: Some(0),
            d: 1000,
            r: Some(4),
        }];
        let out = expand(&entries, None, &mut budget()).unwrap();
        assert_eq!(out.len(), 5);
        assert_eq!(
            out[4],
            SegmentTiming {
                start: 4000,
                duration: 1000
            }
        );
    }

    #[test]
    fn omitted_t_continues_from_the_previous_entry() {
        let entries = [
            TimelineEntry {
                t: Some(0),
                d: 1000,
                r: Some(1),
            },
            TimelineEntry {
                t: None,
                d: 500,
                r: None,
            },
        ];
        let out = expand(&entries, None, &mut budget()).unwrap();
        // Two 1000-tick segments (t=0, t=1000), then one 500-tick segment
        // continuing at 2000.
        assert_eq!(
            out,
            vec![
                SegmentTiming {
                    start: 0,
                    duration: 1000
                },
                SegmentTiming {
                    start: 1000,
                    duration: 1000
                },
                SegmentTiming {
                    start: 2000,
                    duration: 500
                },
            ]
        );
    }

    /// The fiddly case the module docs call out: `r="-1"` bounded by the next
    /// entry's `@t`, including a final short segment from the ceiling
    /// division.
    #[test]
    fn negative_repeat_is_bounded_by_the_next_entrys_t() {
        let entries = [
            TimelineEntry {
                t: Some(0),
                d: 1000,
                r: Some(-1),
            },
            TimelineEntry {
                t: Some(3500),
                d: 500,
                r: None,
            },
        ];
        let out = expand(&entries, None, &mut budget()).unwrap();
        // 0..3500 at 1000/segment = 3 full segments (0,1000,2000) plus one
        // short segment (3000..3500, duration 1000 as *stated*, but bounded by
        // count via ceiling division): span=3500, d=1000 -> ceil = 4 segments.
        assert_eq!(out.len(), 4 + 1); // four from the -1 run, one explicit
        assert_eq!(
            out[3],
            SegmentTiming {
                start: 3000,
                duration: 1000
            }
        );
        assert_eq!(
            out[4],
            SegmentTiming {
                start: 3500,
                duration: 500
            }
        );
    }

    /// `r="-1"` on the *last* entry is bounded by the period end, per the
    /// module doc's DASH-IF reading — this is the "until the period ends"
    /// half of the fiddly case, and the one a live manifest (no period end)
    /// must not invent segments for.
    #[test]
    fn negative_repeat_on_the_last_entry_uses_period_end() {
        let entries = [TimelineEntry {
            t: Some(0),
            d: 1000,
            r: Some(-1),
        }];
        let out = expand(&entries, Some(5000), &mut budget()).unwrap();
        assert_eq!(out.len(), 5);
        assert_eq!(
            out.last().copied(),
            Some(SegmentTiming {
                start: 4000,
                duration: 1000
            })
        );

        // No period end at all: an open `-1` produces nothing further, not an
        // error and not a guess.
        let out_live = expand(&entries, None, &mut budget()).unwrap();
        assert!(out_live.is_empty());
    }

    #[test]
    fn zero_duration_is_rejected_rather_than_looping() {
        let entries = [TimelineEntry {
            t: Some(0),
            d: 0,
            r: Some(-1),
        }];
        assert!(expand(&entries, Some(1_000_000), &mut budget()).is_err());
    }

    #[test]
    fn a_huge_repeat_count_is_bounded_not_materialised() {
        let entries = [TimelineEntry {
            t: Some(0),
            d: 1,
            r: Some(i64::MAX),
        }];
        let err = expand(&entries, None, &mut budget()).unwrap_err();
        assert!(matches!(err, Error::LimitExceeded { .. }));
    }

    #[test]
    fn a_huge_open_repeat_is_also_bounded() {
        let entries = [TimelineEntry {
            t: Some(0),
            d: 1,
            r: Some(-1),
        }];
        let err = expand(&entries, Some(u64::MAX), &mut budget()).unwrap_err();
        assert!(matches!(err, Error::LimitExceeded { .. }));
    }

    proptest::proptest! {
        /// `compact` after `expand` is lossless for any well-formed,
        /// explicitly-repeated (non-`-1`) timeline: the two functions describe
        /// the same segment sequence. This is the round-trip the brief calls
        /// out as the one likeliest to find a real bug.
        #[test]
        fn compact_of_expand_reproduces_the_same_segments(
            starts_from_zero in proptest::bool::ANY,
            runs in proptest::collection::vec((1u64..=10_000, 0u64..=20), 1..12),
        ) {
            let mut entries = Vec::new();
            let mut cursor = 0u64;
            for (i, &(d, r)) in runs.iter().enumerate() {
                let t = if i == 0 && starts_from_zero { Some(0) } else { Some(cursor) };
                entries.push(TimelineEntry { t, d, r: Some(r.cast_signed()) });
                cursor += d * (r + 1);
            }
            let mut b = budget();
            let expanded = expand(&entries, None, &mut b).unwrap();
            let recompacted = compact(&expanded);
            let mut b2 = budget();
            let reexpanded = expand(&recompacted, None, &mut b2).unwrap();
            proptest::prop_assert_eq!(expanded, reexpanded);
        }

        /// Every expansion is non-decreasing and gap-free within one `<S>`
        /// run's contribution: `start[i+1] == start[i] + duration[i]` whenever
        /// both come from the same entry. This is the invariant a subtly
        /// wrong `@r`/`@t` interaction breaks silently (drift, not a crash).
        #[test]
        fn expansion_is_contiguous_within_one_entry(
            t in 0u64..1_000_000,
            d in 1u64..100_000,
            r in 0i64..50,
        ) {
            let entries = [TimelineEntry { t: Some(t), d, r: Some(r) }];
            let mut b = budget();
            let out = expand(&entries, None, &mut b).unwrap();
            for w in out.windows(2) {
                proptest::prop_assert_eq!(w[1].start, w[0].start + w[0].duration);
            }
            proptest::prop_assert_eq!(out.first().map(|s| s.start), Some(t));
        }
    }
}
