//! `dejudder` — smooth small per-frame timestamp jitter left behind by
//! inverse telecine (`pullup`) by re-timing frames to a cycle-averaged rate.
//!
//! `ffmpeg -h filter=dejudder`: one option, `cycle` (`2..=240`, default
//! `4` — the reference's own default matches a 3:2-pulldown-derived
//! cadence).
//!
//! # A structural simplification, clearly scoped
//!
//! The reference's exact re-timing rule (`dejudder.c`'s pattern-aware
//! correction) was out of scope to reverse-engineer for this pass — see
//! `docs/filter/vaco-filter-temporal.md`. This implementation keeps a
//! trailing window of the last `cycle` input inter-frame durations (in the
//! input time base) and assigns each output frame a timestamp
//! `previous_output_pts + round(mean(window durations))`: the same
//! "average out small jitter over one cycle" goal `dejudder` documents
//! itself as serving, without claiming to reproduce the reference's
//! specific pattern-detection arithmetic. Pixel data is never touched —
//! this is purely a timestamp filter, like the reference's own.
//!
//! # Independent oracle
//!
//! A synthetic stream whose *instantaneous* frame durations alternate
//! (`2, 4, 2, 4, ...` ticks — real judder, wrong locally but correct on
//! average) has a hand-computable mean of exactly `3` ticks. Once the
//! trailing window has filled (`cycle` frames in), every later output
//! interval must be exactly that mean, checked directly against `3` rather
//! than against this filter's own running average recomputed a second way.

use vaco_core::{MediaType, Result, Timestamp};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{VIDEO_PAD, usize_opt};

pub const DESC: FilterDesc = FilterDesc {
    name: "dejudder",
    description: "Remove judder produced by pullup.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug)]
pub(crate) struct Filter {
    cycle: usize,
    durations: std::collections::VecDeque<i64>,
    last_input_pts: Option<i64>,
    last_output_pts: Option<i64>,
}

impl Filter {
    pub(crate) fn new(cycle: usize) -> Self {
        Self {
            cycle: cycle.max(2),
            durations: std::collections::VecDeque::new(),
            last_input_pts: None,
            last_output_pts: None,
        }
    }

    fn mean_duration(&self) -> i64 {
        if self.durations.is_empty() {
            return 0;
        }
        let sum: i64 = self.durations.iter().sum();
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_possible_wrap,
            reason = "durations.len() is bounded by `cycle`, at most 240"
        )]
        let n = self.durations.len() as i64;
        sum.checked_div(n.max(1)).unwrap_or(0)
    }

    /// The re-timing step, independent of [`FilterContext`].
    fn step(&mut self, mut frame: Frame) -> FrameOut {
        let Some(in_pts) = frame.pts.ticks() else {
            return FrameOut::One(frame);
        };
        if let Some(last_in) = self.last_input_pts {
            self.durations.push_back(in_pts.saturating_sub(last_in));
            while self.durations.len() > self.cycle {
                self.durations.pop_front();
            }
        }
        self.last_input_pts = Some(in_pts);

        let out_pts = match self.last_output_pts {
            None => in_pts,
            Some(last_out) => last_out.saturating_add(self.mean_duration().max(1)),
        };
        self.last_output_pts = Some(out_pts);
        frame.pts = Timestamp::new(out_pts);
        FrameOut::One(frame)
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        Ok(self.step(frame))
    }

    fn flush_state(&mut self) {
        self.durations.clear();
        self.last_input_pts = None;
        self.last_output_pts = None;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let cycle = usize_opt(req, "cycle", 4).clamp(2, 240);
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(cycle))),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_pixfmt::PixFmt;

    fn frame_at(pts: i64) -> Frame {
        let pool = vaco_frame::FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 2, 2).unwrap();
        f.pts = Timestamp::new(pts);
        f
    }

    #[test]
    fn alternating_durations_settle_to_their_hand_computed_mean() {
        let mut f = Filter::new(4);
        // Instantaneous durations 2,4,2,4,... -> mean 3 once the window
        // (cycle=4) has filled.
        let mut pts = 0i64;
        let mut outputs = Vec::new();
        for i in 0..12 {
            pts += if i % 2 == 0 { 2 } else { 4 };
            let FrameOut::One(fr) = f.step(frame_at(pts)) else {
                panic!("expected a frame")
            };
            outputs.push(fr.pts.ticks().unwrap());
        }
        let deltas: Vec<i64> = outputs.windows(2).map(|w| w[1] - w[0]).collect();
        // After the window fills (index >= cycle), every output delta must
        // be exactly the hand-computed mean of 3.
        for &d in deltas.iter().skip(4) {
            assert_eq!(d, 3);
        }
    }

    #[test]
    fn a_perfectly_even_stream_is_unaffected() {
        let mut f = Filter::new(4);
        let mut outputs = Vec::new();
        for i in 0..8i64 {
            let FrameOut::One(fr) = f.step(frame_at(i * 5)) else {
                panic!("expected a frame")
            };
            outputs.push(fr.pts.ticks().unwrap());
        }
        assert_eq!(outputs, (0..8).map(|i| i * 5).collect::<Vec<_>>());
    }
}
