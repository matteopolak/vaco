//! The segmentation decision, isolated from I/O so it can be unit-tested
//! against a plain sequence of `(pts, is_key)` pairs.
//!
//! Measured (`ffmpeg -h muxer=segment` / `stream_segment`, ffmpeg 8.1):
//! `-segment_time` defaults to 2 seconds, `-segment_time_delta` to 0,
//! `-break_non_keyframes` to `false` (a cut only ever happens *at* a
//! keyframe, arriving later than the nominal boundary if the next keyframe
//! is further out — unless this is set, which allows a cut at any packet).
//! `-segment_times`/`-segment_frames` give explicit split points instead of
//! a uniform interval, and are mutually exclusive with `-segment_time` in
//! this crate's model (the reference's own docs describe them as
//! alternatives; this was not independently re-probed for what happens if
//! both are given together — this crate takes explicit points over a uniform
//! interval when both are configured, rather than erroring, since "cut at
//! at least these times" is a safe reading of "also cut every N seconds").

//! This planner reads every `pts` as already being in
//! [`vaco_format_core::time::TIME_BASE_Q`] (microseconds) — the base a
//! muxer that declares no [`vaco_format_core::Muxer::stream_time_base`]
//! opinion receives packets in, which is exactly
//! [`crate::segment::SegmentMuxer`]'s situation (it has no fixed timescale
//! of its own to offer). A caller driving this planner directly, as this
//! module's own tests do, must rescale into microseconds first.

use vaco_core::{Duration, Timestamp};

/// What triggers a new segment.
#[derive(Debug, Clone, PartialEq)]
pub enum SegmentTrigger {
    /// `-segment_time`: a uniform interval.
    Interval(Duration),
    /// `-segment_times`: explicit, ascending cut points from the start of
    /// the file.
    ExplicitTimes(Vec<Duration>),
    /// `-segment_frames`: explicit, ascending reference-stream frame counts.
    ExplicitFrames(Vec<u64>),
}

/// The pure segmentation state machine.
#[derive(Debug, Clone)]
pub struct SegmentPlanner {
    trigger: SegmentTrigger,
    time_delta: Duration,
    min_seg_duration: Duration,
    break_non_keyframes: bool,
    /// The reference-stream time this segment started at, once the first
    /// packet of it has been seen.
    segment_start: Option<Duration>,
    /// Reference-stream packets seen in the current segment (for
    /// `-segment_frames`).
    frames_in_segment: u64,
    /// Index into `ExplicitTimes`/`ExplicitFrames` of the next boundary.
    next_point: usize,
}

impl SegmentPlanner {
    #[must_use]
    pub const fn new(
        trigger: SegmentTrigger,
        time_delta: Duration,
        min_seg_duration: Duration,
        break_non_keyframes: bool,
    ) -> Self {
        Self {
            trigger,
            time_delta,
            min_seg_duration,
            break_non_keyframes,
            segment_start: None,
            frames_in_segment: 0,
            next_point: 0,
        }
    }

    /// Feed one reference-stream packet. Returns `true` if a new segment
    /// must start **before** this packet (i.e. this packet belongs to the
    /// new segment).
    ///
    /// Non-reference-stream packets never reach this — see
    /// `crate::segment::SegmentMuxer`, which routes only the resolved
    /// reference stream's packets here and simply forwards everything else
    /// into whichever segment is currently open.
    pub fn on_reference_packet(&mut self, pts: Timestamp, is_key: bool) -> bool {
        let now = pts.ticks().map(Duration::from_micros);
        let Some(now) = now else {
            // No timestamp: cannot judge a boundary, stay in the current
            // segment.
            self.frames_in_segment += 1;
            return false;
        };
        let start = *self.segment_start.get_or_insert(now);
        let elapsed_us = now.as_micros().saturating_sub(start.as_micros());

        let wants_cut = match &self.trigger {
            SegmentTrigger::Interval(interval) => {
                elapsed_us.saturating_add(self.time_delta.as_micros()) >= interval.as_micros()
            }
            SegmentTrigger::ExplicitTimes(points) => points.get(self.next_point).is_some_and(|p| {
                now.as_micros().saturating_add(self.time_delta.as_micros()) >= p.as_micros()
            }),
            SegmentTrigger::ExplicitFrames(points) => points
                .get(self.next_point)
                .is_some_and(|&p| self.frames_in_segment >= p),
        };

        let long_enough = elapsed_us >= self.min_seg_duration.as_micros();
        let may_cut_here = is_key || self.break_non_keyframes;

        if wants_cut && long_enough && may_cut_here && self.frames_in_segment > 0 {
            self.segment_start = Some(now);
            self.frames_in_segment = 1;
            if matches!(
                self.trigger,
                SegmentTrigger::ExplicitTimes(_) | SegmentTrigger::ExplicitFrames(_)
            ) {
                self.next_point += 1;
            }
            return true;
        }

        self.frames_in_segment += 1;
        false
    }

    /// Packets seen in the segment currently open, for
    /// [`crate::segment::SegmentMuxer::write_empty_segments`]'s check.
    #[must_use]
    pub const fn frames_in_current_segment(&self) -> u64 {
        self.frames_in_segment
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn planner(seconds: i64) -> SegmentPlanner {
        SegmentPlanner::new(
            SegmentTrigger::Interval(Duration::from_micros(seconds * 1_000_000)),
            Duration::from_micros(0),
            Duration::from_micros(0),
            false,
        )
    }

    #[test]
    fn cuts_at_the_first_keyframe_at_or_past_the_interval() {
        let mut p = planner(2);
        // ms pts, is_key
        let frames = [
            (0, true),
            (1000, false),
            (2000, false),
            (2500, true),
            (3000, false),
        ];
        let cuts: Vec<bool> = frames
            .iter()
            .map(|&(ms, key)| p.on_reference_packet(Timestamp::new(ms * 1000), key))
            .collect();
        assert_eq!(cuts, vec![false, false, false, true, false]);
    }

    #[test]
    fn break_non_keyframes_cuts_at_the_exact_interval() {
        let mut p = SegmentPlanner::new(
            SegmentTrigger::Interval(Duration::from_micros(2_000_000)),
            Duration::from_micros(0),
            Duration::from_micros(0),
            true,
        );
        let frames = [(0, true), (1000, false), (2000, false)];
        let cuts: Vec<bool> = frames
            .iter()
            .map(|&(ms, key)| p.on_reference_packet(Timestamp::new(ms * 1000), key))
            .collect();
        assert_eq!(cuts, vec![false, false, true]);
    }

    #[test]
    fn min_seg_duration_suppresses_an_early_keyframe_cut() {
        let mut p = SegmentPlanner::new(
            SegmentTrigger::Interval(Duration::from_micros(1_000_000)),
            Duration::from_micros(0),
            Duration::from_micros(3_000_000),
            false,
        );
        let frames = [(0, true), (1200, true), (3200, true)];
        let cuts: Vec<bool> = frames
            .iter()
            .map(|&(ms, key)| p.on_reference_packet(Timestamp::new(ms * 1000), key))
            .collect();
        // The keyframe at 1200ms is past the 1s interval but before the 3s
        // minimum segment duration, so it does not cut; 3200ms does.
        assert_eq!(cuts, vec![false, false, true]);
    }

    #[test]
    fn explicit_times_cut_at_each_named_point_in_order() {
        let mut p = SegmentPlanner::new(
            SegmentTrigger::ExplicitTimes(vec![
                Duration::from_micros(1_500_000),
                Duration::from_micros(4_000_000),
            ]),
            Duration::from_micros(0),
            Duration::from_micros(0),
            false,
        );
        let frames = [(0, true), (1000, true), (1600, true), (4200, true)];
        let cuts: Vec<bool> = frames
            .iter()
            .map(|&(ms, key)| p.on_reference_packet(Timestamp::new(ms * 1000), key))
            .collect();
        assert_eq!(cuts, vec![false, false, true, true]);
    }

    #[test]
    fn a_packet_with_no_pts_never_triggers_a_cut() {
        let mut p = planner(2);
        assert!(!p.on_reference_packet(Timestamp::NONE, true));
        assert_eq!(p.frames_in_current_segment(), 1);
    }

    #[test]
    fn the_very_first_packet_never_cuts() {
        let mut p = planner(0); // interval 0 would otherwise cut immediately
        assert!(!p.on_reference_packet(Timestamp::new(0), true));
    }
}
