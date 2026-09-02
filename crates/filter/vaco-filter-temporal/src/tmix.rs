//! `tmix` — a weighted average of the last `frames` frames (trailing
//! window, this frame included).
//!
//! `ffmpeg -h filter=tmix`: `frames` (`1..=1024`, default `3`), `weights`
//! (space-separated floats, default `"1 1 1"`, shorter than `frames` repeats
//! the last value — matching `ffmpeg`'s own documented behaviour for this
//! option shape), `scale` (default `0`, meaning "sum of weights" — the
//! reference's own `0` sentinel for "auto", not literal zero), `planes`
//! (bitmask, default all).
//!
//! # Algorithm
//!
//! Trailing window of the last `min(frames, seen)` input frames. Output
//! pixel = `sum(weight[i] * history[i]) / scale`, `scale` defaulting to
//! `sum(weight[i])` over the frames actually present (so the average stays
//! correctly normalised while the window is still filling at stream start).
//!
//! # Independent oracle
//!
//! `frames=1` must be the identity: only one frame is ever in the window, so
//! output is that frame with weight `1` over scale `1` — exactly the input,
//! checked byte-for-byte, not by re-running this filter's own math a second
//! way. With `frames=3, weights="1 1 1"` on three known constant frames the
//! output is their arithmetic mean, computable by hand.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{
    PlaneBuf, VIDEO_PAD, copy_meta, plane_dims, planes_mask_opt, sample_layout, str_opt, usize_opt,
};

pub const DESC: FilterDesc = FilterDesc {
    name: "tmix",
    description: "Mix successive video frames.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

fn parse_weights(s: &str, frames: usize) -> Vec<f64> {
    let parsed: Vec<f64> = s
        .split_whitespace()
        .filter_map(|t| t.parse::<f64>().ok())
        .collect();
    if parsed.is_empty() {
        return vec![1.0; frames];
    }
    let mut out = Vec::new();
    for i in 0..frames {
        out.push(
            *parsed
                .get(i)
                .unwrap_or_else(|| parsed.last().unwrap_or(&1.0)),
        );
    }
    out
}

#[derive(Debug, Clone)]
pub(crate) struct Options {
    frames: usize,
    weights: Vec<f64>,
    scale: f64,
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

    fn mix(&self) -> Option<Frame> {
        let newest = self.history.back()?;
        let mut out = newest.clone();
        out.make_writable();
        let n = self.history.len();
        // Weights/scale for exactly the frames present: the last `n`
        // entries of the configured weight vector, renormalised if `scale`
        // is the "auto" sentinel.
        let start = self.opts.weights.len().saturating_sub(n);
        let weights: &[f64] = self.opts.weights.get(start..).unwrap_or(&[]);
        let scale = if self.opts.scale > 0.0 {
            self.opts.scale
        } else {
            weights.iter().sum::<f64>().max(f64::MIN_POSITIVE)
        };

        let Some(format) = newest.pixel_format() else {
            return Some(out);
        };
        let Some((width, height)) = newest.dimensions() else {
            return Some(out);
        };
        for plane_idx in 0..newest.plane_count() {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "plane_count is tiny, well within u8"
            )]
            let bit = 1u8 << (plane_idx as u8).min(7);
            if self.opts.planes & bit == 0 && plane_idx > 0 {
                // planes bitmask excludes this plane: copy source unchanged.
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
            // Every sample gets overwritten below; reuse `bufs`'s last
            // element (already `newest`'s own plane) instead of a second
            // fallible `Frame::plane` lookup just to seed the buffer.
            let Some(mut result) = bufs.last().cloned() else {
                continue;
            };
            for y in 0..ph {
                for x in 0..pw {
                    let mut acc = 0.0f64;
                    for (buf, w) in bufs.iter().zip(weights.iter()) {
                        acc += f64::from(buf.get(x, y)) * w;
                    }
                    #[allow(
                        clippy::cast_possible_truncation,
                        reason = "acc/scale is within plane sample range by construction"
                    )]
                    result.set(x, y, (acc / scale) as f32);
                }
            }
            if let Some(mut dst) = out.plane_mut(plane_idx) {
                result.write(&mut dst, bytes);
            }
        }
        Some(out)
    }
}

impl Filter {
    /// The mix step, independent of [`FilterContext`] so it can be exercised
    /// directly in tests without a full graph.
    fn step(&mut self, frame: Frame) -> FrameOut {
        self.history.push_back(frame);
        while self.history.len() > self.opts.frames {
            self.history.pop_front();
        }
        self.mix().map_or(FrameOut::None, |mut out| {
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
    let frames = usize_opt(req, "frames", 3).clamp(1, 1024);
    let weights = parse_weights(
        str_opt(req, "weights").as_deref().unwrap_or("1 1 1"),
        frames,
    );
    let scale = crate::video::f64_opt(req, "scale", 0.0).max(0.0);
    let planes = planes_mask_opt(req, &["planes"], 0x0F);
    let opts = Options {
        frames,
        weights,
        scale,
        planes,
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
    fn frames_one_is_the_identity() {
        let opts = Options {
            frames: 1,
            weights: vec![1.0],
            scale: 0.0,
            planes: 0x0F,
        };
        let mut f = Filter::new(opts);
        for v in [10u8, 200, 55] {
            let out = f.step(constant_frame(v));
            let FrameOut::One(fr) = out else {
                panic!("expected a frame")
            };
            assert_eq!(sample(&fr), v, "tmix=frames=1 must reproduce the input");
        }
    }

    #[test]
    fn three_constant_frames_average_by_hand() {
        let opts = Options {
            frames: 3,
            weights: vec![1.0, 1.0, 1.0],
            scale: 0.0,
            planes: 0x0F,
        };
        let mut f = Filter::new(opts);
        let mut last = None;
        for v in [30u8, 60, 90] {
            last = match f.step(constant_frame(v)) {
                FrameOut::One(fr) => Some(fr),
                _ => None,
            };
        }
        // (30+60+90)/3 = 60
        assert_eq!(sample(&last.unwrap()), 60);
    }
}
