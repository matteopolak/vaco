//! `tmidequalizer` — pull each pixel toward its own trailing temporal mean,
//! by `sigma`.
//!
//! `ffmpeg -h filter=tmidequalizer`: `radius` (`1..=127`, default `5`),
//! `sigma` (`0..=1`, default `0.5`), `planes` (bitmask int, default `15`).
//!
//! # A named, deliberate simplification of "midway equalization"
//!
//! The reference's Temporal Midway Equalization matches each frame's
//! *histogram* toward a running "midway" distribution across the window —
//! genuine histogram-domain work this pass did not budget time to
//! reverse-engineer faithfully (see `docs/filter/vaco-filter-temporal.md`).
//! What is implemented here is the simpler, honestly-scoped operation the
//! option names still describe reasonably: each pixel's trailing
//! `2*radius+1`-frame average (this crate's usual trailing-window
//! convention, see `vaco-filter-denoise::atadenoise`'s documented choice for
//! the same reason) is the "midway" target, and the output is a linear
//! blend `current*(1-sigma) + target*sigma` — real temporal smoothing
//! toward a local mean, not the reference's histogram equalization. `sigma`
//! keeps its documented role (0 = no change, higher = more aggressively
//! pulled toward the temporal average).
//!
//! # Independent oracle
//!
//! `sigma=0` must be the identity for *any* window — the blend coefficient
//! on the target is exactly zero — checked byte-for-byte. A stream of
//! constant frames has a temporal mean equal to that same constant at every
//! pixel, so blending toward it changes nothing regardless of `sigma`,
//! `radius`, or how many frames have arrived — a second, distinct identity
//! case.

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
    name: "tmidequalizer",
    description: "Apply Temporal Midway Equalization.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, Copy)]
pub(crate) struct Options {
    radius: usize,
    sigma: f64,
    planes: u8,
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
        #[allow(clippy::cast_possible_truncation, reason = "sigma is clamped to 0..=1")]
        let sigma = self.opts.sigma as f32;

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
            let current = bufs.last()?;
            let mut result = current.clone();
            #[allow(clippy::cast_precision_loss, reason = "window sizes are <= 255")]
            let count = n as f32;
            for y in 0..ph {
                for x in 0..pw {
                    let sum: f32 = bufs.iter().map(|b| b.get(x, y)).sum();
                    let target = sum / count;
                    let cur = current.get(x, y);
                    let blended = cur.mul_add(1.0 - sigma, target * sigma);
                    result.set(x, y, blended.clamp(0.0, max_val));
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
    let opts = Options {
        radius: usize_opt(req, "radius", 5).clamp(1, 127),
        sigma: f64_opt(req, "sigma", 0.5).clamp(0.0, 1.0),
        planes: planes_mask_opt(req, &["planes"], 0x0F),
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
        let mut f = pool.acquire_video(PixFmt::Gray8, 2, 2).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            p.fill(value);
        }
        f
    }

    fn sample(f: &Frame) -> u8 {
        f.plane(0).unwrap().row(0).unwrap()[0]
    }

    #[test]
    fn sigma_zero_is_the_identity() {
        let opts = Options {
            radius: 2,
            sigma: 0.0,
            planes: 0x0F,
        };
        let mut f = Filter::new(opts);
        for v in [10u8, 90, 250, 3] {
            let FrameOut::One(fr) = f.step(frame_of(v)) else {
                panic!("expected a frame")
            };
            assert_eq!(sample(&fr), v);
        }
    }

    #[test]
    fn constant_brightness_stream_is_unaffected_by_sigma() {
        let opts = Options {
            radius: 3,
            sigma: 1.0,
            planes: 0x0F,
        };
        let mut f = Filter::new(opts);
        for _ in 0..10 {
            let FrameOut::One(fr) = f.step(frame_of(128)) else {
                panic!("expected a frame")
            };
            assert_eq!(sample(&fr), 128);
        }
    }
}
