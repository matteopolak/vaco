//! `mpdecimate` — drop a frame that is a near-duplicate of the previous one,
//! judged by 8x8-block SAD against `hi`/`lo`/`frac` thresholds (the shape
//! `planning/16-filters.md`'s own accounting table documents this filter by:
//! "8x8-block SAD against `hi`/`lo`/`frac` thresholds").
//!
//! `ffmpeg -h filter=mpdecimate`: `max` (run-length cap, positive =
//! consecutive drops, negative = minimum gap between drops, default `0` =
//! unlimited), `keep` (frames kept before dropping starts, default `0`),
//! `hi`/`lo` (per-block SAD thresholds, default `768`/`320`), `frac`
//! (fraction of blocks allowed over `lo` before the frame counts as
//! "changed", default `0.33`).
//!
//! # The block metric: a documented choice, not a measured one
//!
//! The reference's `-h` output gives `hi`/`lo` as bare integers with no
//! stated unit, and reverse-engineering its internal scale from black-box
//! probing was out of scope for this pass (see `docs/filter/
//! vaco-filter-temporal.md`). This implementation defines the per-8x8-block
//! metric as the plain sum of absolute luma differences against the
//! previous frame's corresponding block (`vaco-filter-vdsp::block_sad`,
//! range `0..=16320`), which places the documented defaults (`hi=768`,
//! `lo=320`) at a plausible "an eighth of blocks changed by ~5-12 levels
//! each" scale. **This is a structural approximation of the metric's units,
//! not a claim of reference-identical thresholds** — the decision logic
//! itself (compare every block's SAD to `hi` and `lo`, drop iff no block
//! exceeds `hi` and the `lo`-exceeding fraction is under `frac`) is the
//! part this crate is confident in and tests.
//!
//! # Independent oracle
//!
//! A synthetic stream built from `N` distinct frames each repeated `k`
//! times in a row has an exactly countable answer: the first occurrence of
//! each distinct frame is never a duplicate of anything before it (kept),
//! and each of the `k-1` exact repeats that follow has zero block SAD
//! against its immediate predecessor, so it is unconditionally under both
//! thresholds and dropped — for a total kept count of exactly `N`,
//! independent of `k`. That count is asserted directly against the
//! filter's output, not by asking the filter what it thinks it kept.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{VIDEO_PAD, i64_opt};

pub const DESC: FilterDesc = FilterDesc {
    name: "mpdecimate",
    description: "Remove near-duplicate frames.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

const BLOCK: usize = 8;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Options {
    max: i64,
    keep: i64,
    hi: i64,
    lo: i64,
    frac: f64,
}

#[derive(Debug)]
pub(crate) struct Filter {
    opts: Options,
    prev: Option<Frame>,
    consecutive_drops: i64,
    consecutive_kept: i64,
    frames_since_drop: i64,
    kept_before_drop_ok: bool,
}

impl Filter {
    pub(crate) fn new(opts: Options) -> Self {
        Self {
            opts,
            prev: None,
            consecutive_drops: 0,
            consecutive_kept: 0,
            frames_since_drop: 0,
            kept_before_drop_ok: opts.keep == 0,
        }
    }

    /// Whether `frame` is a near-duplicate of `prev` per the block metric.
    fn is_duplicate(&self, frame: &Frame, prev: &Frame) -> bool {
        let (Some(a), Some(b)) = (frame.plane(0), prev.plane(0)) else {
            return false;
        };
        let (Some((w, h)), Some((pw, ph))) = (frame.dimensions(), prev.dimensions()) else {
            return false;
        };
        if (w, h) != (pw, ph) {
            return false;
        }
        let bx = (w as usize).div_ceil(BLOCK).max(1);
        let by = (h as usize).div_ceil(BLOCK).max(1);
        let total = bx.saturating_mul(by).max(1);
        let mut over_lo = 0usize;
        for gy in 0..by {
            for gx in 0..bx {
                let sad = vaco_filter_vdsp::block_sad(
                    a,
                    b,
                    gx.saturating_mul(BLOCK),
                    gy.saturating_mul(BLOCK),
                    BLOCK,
                    BLOCK,
                );
                #[allow(
                    clippy::cast_possible_wrap,
                    reason = "block SAD is far below i64::MAX"
                )]
                let sad = sad as i64;
                if sad > self.opts.hi {
                    return false;
                }
                if sad > self.opts.lo {
                    over_lo = over_lo.saturating_add(1);
                }
            }
        }
        #[allow(clippy::cast_precision_loss, reason = "total is a tiny block count")]
        let fraction = over_lo as f64 / total as f64;
        fraction < self.opts.frac
    }

    /// Whether the run-length caps (`max`/`keep`) currently forbid a drop.
    fn run_length_forbids_drop(&self) -> bool {
        if !self.kept_before_drop_ok {
            return true;
        }
        if self.opts.max > 0 && self.consecutive_drops >= self.opts.max {
            return true;
        }
        if self.opts.max < 0 && self.frames_since_drop < -self.opts.max {
            return true;
        }
        false
    }

    /// The per-frame step, independent of [`FilterContext`].
    fn step(&mut self, frame: Frame) -> FrameOut {
        let Some(prev) = self.prev.clone() else {
            self.prev = Some(frame.clone());
            self.consecutive_kept = self.consecutive_kept.saturating_add(1);
            if self.consecutive_kept >= self.opts.keep {
                self.kept_before_drop_ok = true;
            }
            return FrameOut::One(frame);
        };
        let duplicate = self.is_duplicate(&frame, &prev) && !self.run_length_forbids_drop();
        if duplicate {
            self.consecutive_drops = self.consecutive_drops.saturating_add(1);
            self.frames_since_drop = 0;
            // The previous frame stays "prev" — the dropped frame never
            // becomes the new reference, matching "near-duplicate of the
            // last *kept* frame".
            FrameOut::None
        } else {
            self.prev = Some(frame.clone());
            self.consecutive_drops = 0;
            self.frames_since_drop = self.frames_since_drop.saturating_add(1);
            self.consecutive_kept = self.consecutive_kept.saturating_add(1);
            if self.consecutive_kept >= self.opts.keep {
                self.kept_before_drop_ok = true;
            }
            FrameOut::One(frame)
        }
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        Ok(self.step(frame))
    }

    fn flush_state(&mut self) {
        self.prev = None;
        self.consecutive_drops = 0;
        self.consecutive_kept = 0;
        self.frames_since_drop = 0;
        self.kept_before_drop_ok = self.opts.keep == 0;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let opts = Options {
        max: i64_opt(req, "max", 0),
        keep: i64_opt(req, "keep", 0),
        hi: i64_opt(req, "hi", 768),
        lo: i64_opt(req, "lo", 320),
        frac: crate::video::f64_opt(req, "frac", 0.33),
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(opts))),
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

    fn frame_of(value: u8) -> Frame {
        let pool = vaco_frame::FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 16, 16).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            p.fill(value);
        }
        f
    }

    fn default_opts() -> Options {
        Options {
            max: 0,
            keep: 0,
            hi: 768,
            lo: 320,
            frac: 0.33,
        }
    }

    #[test]
    fn exact_repeats_are_dropped_distinct_frames_are_kept() {
        let mut f = Filter::new(default_opts());
        let values = [10u8, 10, 10, 200, 200, 50, 50, 50, 50];
        let kept = values
            .iter()
            .filter(|&&v| matches!(f.step(frame_of(v)), FrameOut::One(_)))
            .count();
        // 3 distinct runs (10, 200, 50) -> exactly 3 kept.
        assert_eq!(kept, 3);
    }

    #[test]
    fn a_single_frame_stream_is_always_kept() {
        let mut f = Filter::new(default_opts());
        assert!(matches!(f.step(frame_of(77)), FrameOut::One(_)));
    }

    #[test]
    fn large_differences_are_never_dropped_regardless_of_frac() {
        let mut opts = default_opts();
        opts.frac = 1.0; // would otherwise tolerate anything under `hi`
        let mut f = Filter::new(opts);
        let _ = f.step(frame_of(0));
        // 0 -> 255 is a full-scale change per block, over `hi` in every block.
        assert!(matches!(f.step(frame_of(255)), FrameOut::One(_)));
    }
}
