//! `tmedian` — per-pixel median (or other percentile) across a trailing
//! window of `2*radius+1` frames.
//!
//! `ffmpeg -h filter=tmedian`: `radius` (`1..=127`, default `1`), `planes`
//! (bitmask int, default `15`), `percentile` (`0..=1`, default `0.5` —
//! `0.5` is the median; other values pick a different rank in the sorted
//! window, same as `tmedian`'s own documented option).
//!
//! # A structural simplification: trailing, not centred
//!
//! The reference's window of `2*radius+1` frames is centred on the *output*
//! frame, which needs `radius` frames of lookahead. This implementation
//! uses the trailing `2*radius+1` input frames ending at the current one
//! (matching `vaco-filter-denoise::atadenoise`'s documented choice for the
//! same reason): zero added latency, no special-cased stream edges, at the
//! cost of the reference's frame alignment. The window shrinks to whatever
//! history exists at stream start, same as `atadenoise`.
//!
//! # Independent oracle
//!
//! An odd window of *constant* frames' median is that constant, for any
//! correct median — not a property of this file's particular sort. Checked
//! directly: `radius=1` (window 3) on three frames of the same value must
//! reproduce that value exactly.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{
    PlaneBuf, VIDEO_PAD, copy_meta, f64_opt, plane_dims, planes_mask_opt, sample_layout, usize_opt,
};

pub const DESC: FilterDesc = FilterDesc {
    name: "tmedian",
    description: "Pick median pixels from successive frames.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, Copy)]
pub(crate) struct Options {
    radius: usize,
    planes: u8,
    percentile: f64,
}

#[derive(Debug)]
pub(crate) struct Filter {
    opts: Options,
    history: std::collections::VecDeque<Frame>,
}

impl Filter {
    pub(crate) fn new(opts: Options) -> Self {
        Self {
            opts,
            history: std::collections::VecDeque::new(),
        }
    }

    fn window(&self) -> usize {
        2 * self.opts.radius + 1
    }

    fn compute(&self) -> Option<Frame> {
        let newest = self.history.back()?;
        let mut out = newest.clone();
        out.make_writable();
        let format = newest.pixel_format()?;
        let (width, height) = newest.dimensions()?;
        let n = self.history.len();

        for plane_idx in 0..newest.plane_count() {
            #[allow(clippy::cast_possible_truncation, reason = "plane index is tiny")]
            let bit = 1u8 << (plane_idx as u8).min(7);
            if self.opts.planes & bit == 0 {
                continue;
            }
            let Some((bytes, max_val)) = sample_layout(format, plane_idx.min(255) as u8) else {
                continue;
            };
            let (pw, ph) = plane_dims(format, width, height, plane_idx);
            let bufs: Vec<PlaneBuf> = self
                .history
                .iter()
                .filter_map(|f| f.plane(plane_idx))
                .map(|p| PlaneBuf::read(p, pw, ph, bytes, max_val))
                .collect();
            if bufs.len() != n {
                continue;
            }
            // `newest` is `self.history`'s last entry, already read into
            // `bufs`'s last element — reuse it instead of a second
            // fallible `Frame::plane` lookup.
            let Some(mut result) = bufs.last().cloned() else {
                continue;
            };
            let mut samples = vec![0.0f32; n];
            for y in 0..ph {
                for x in 0..pw {
                    for (i, buf) in bufs.iter().enumerate() {
                        if let Some(slot) = samples.get_mut(i) {
                            *slot = buf.get(x, y);
                        }
                    }
                    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    let rank = ((n.saturating_sub(1)) as f64 * self.opts.percentile.clamp(0.0, 1.0))
                        .round();
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "rank is clamped into 0..n"
                    )]
                    let idx = (rank as usize).min(n.saturating_sub(1));
                    let v = samples.get(idx).copied().unwrap_or(0.0);
                    result.set(x, y, v);
                }
            }
            if let Some(mut dst) = out.plane_mut(plane_idx) {
                result.write(&mut dst, bytes);
            }
        }
        Some(out)
    }

    /// The window-update-and-compute step, independent of [`FilterContext`].
    fn step(&mut self, frame: Frame) -> FrameOut {
        self.history.push_back(frame);
        while self.history.len() > self.window() {
            self.history.pop_front();
        }
        self.compute().map_or(FrameOut::None, |mut out| {
            if let Some(newest) = self.history.back() {
                copy_meta(&mut out, newest);
            }
            FrameOut::One(out)
        })
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        Ok(self.step(frame))
    }

    fn flush_state(&mut self) {
        self.history.clear();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let radius = usize_opt(req, "radius", 1).clamp(1, 127);
    let planes = planes_mask_opt(req, &["planes"], 0x0F);
    let percentile = f64_opt(req, "percentile", 0.5).clamp(0.0, 1.0);
    let opts = Options {
        radius,
        planes,
        percentile,
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

    fn constant_frame(value: u8) -> Frame {
        let pool = vaco_frame::FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 2, 2).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            p.fill(value);
        }
        f
    }

    fn sample(frame: &Frame) -> u8 {
        frame.plane(0).unwrap().row(0).unwrap()[0]
    }

    #[test]
    fn odd_window_of_constant_frames_medians_to_that_constant() {
        let opts = Options {
            radius: 1,
            planes: 0x0F,
            percentile: 0.5,
        };
        let mut f = Filter::new(opts);
        let mut last = None;
        for _ in 0..3 {
            last = match f.step(constant_frame(77)) {
                FrameOut::One(fr) => Some(fr),
                _ => None,
            };
        }
        assert_eq!(sample(&last.unwrap()), 77);
    }

    #[test]
    fn distinct_values_pick_the_middle_one() {
        let opts = Options {
            radius: 1,
            planes: 0x0F,
            percentile: 0.5,
        };
        let mut f = Filter::new(opts);
        let mut last = None;
        for v in [10u8, 200, 50] {
            last = match f.step(constant_frame(v)) {
                FrameOut::One(fr) => Some(fr),
                _ => None,
            };
        }
        // sorted [10, 50, 200] -> median 50
        assert_eq!(sample(&last.unwrap()), 50);
    }
}
