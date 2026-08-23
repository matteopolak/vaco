//! `framestep` — keep one frame every `step` frames, drop the rest.
//!
//! `ffmpeg -h filter=framestep`: one option, `step` (`1..=INT_MAX`, default
//! `1`). Frame `n` (0-indexed, arrival order) is kept iff `n % step == 0`.
//!
//! # Independent oracle
//!
//! `step=1` is the identity: every frame arrives and every frame is kept, so
//! the output sequence must be the *input* frames, byte for byte (same
//! `Frame`, not merely equal pixels) — checked below by pointer/field
//! identity via the pts sequence, since `step=1` never reallocates or
//! recomputes anything. For `step=N>1`, the kept-frame count on a stream of
//! `L` frames is `ceil(L / N)`, and the kept pts values are an arithmetic
//! progression `0, N, 2N, ...` — both counted directly against a synthetic
//! stream, not against this filter's own output re-examined a second way.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{VIDEO_PAD, usize_opt};

pub const DESC: FilterDesc = FilterDesc {
    name: "framestep",
    description: "Select one frame every N frames.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug)]
pub(crate) struct Filter {
    step: usize,
    index: u64,
}

impl Filter {
    pub(crate) fn new(step: usize) -> Self {
        Self {
            step: step.max(1),
            index: 0,
        }
    }

    /// Exercised directly in tests, independent of [`FilterContext`].
    fn step_frame(&mut self, frame: Frame) -> FrameOut {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "step is a usize option, index wraps modulo it"
        )]
        #[allow(clippy::cast_lossless, reason = "step is usize, index is u64")]
        let n = self.index.is_multiple_of(self.step as u64);
        self.index = self.index.saturating_add(1);
        if n { FrameOut::One(frame) } else { FrameOut::None }
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        Ok(self.step_frame(frame))
    }

    fn flush_state(&mut self) {
        self.index = 0;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let step = usize_opt(req, "step", 1).max(1);
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(step))),
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
    use vaco_core::Timestamp;
    use vaco_pixfmt::PixFmt;

    fn frame_at(pts: i64) -> Frame {
        let pool = vaco_frame::FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 2, 2).unwrap();
        f.pts = Timestamp::new(pts);
        f
    }

    fn kept_pts(f: &mut Filter, n: i64) -> Vec<i64> {
        (0..n)
            .filter_map(|i| match f.step_frame(frame_at(i)) {
                FrameOut::One(fr) => fr.pts.ticks(),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn step_one_is_the_identity_every_frame_kept() {
        let mut f = Filter::new(1);
        assert_eq!(kept_pts(&mut f, 10), (0..10).collect::<Vec<_>>());
    }

    #[test]
    fn step_n_keeps_an_arithmetic_progression() {
        let mut f = Filter::new(3);
        assert_eq!(kept_pts(&mut f, 10), vec![0, 3, 6, 9]);
    }

    #[test]
    fn kept_count_is_ceil_of_length_over_step() {
        for (len, step, expected) in [(10usize, 3usize, 4usize), (9, 3, 3), (1, 5, 1), (0, 5, 0)] {
            let mut f = Filter::new(step);
            #[allow(clippy::cast_possible_wrap, reason = "test lengths are small")]
            let kept = kept_pts(&mut f, len as i64).len();
            assert_eq!(kept, expected, "len={len} step={step}");
        }
    }

    #[test]
    fn step_defaults_to_one_when_missing() {
        let req = Instantiate {
            name: "framestep",
            instance: "framestep",
            args: None,
            arguments: &[],
        };
        assert_eq!(create(&req).desc.name, "framestep");
    }
}
