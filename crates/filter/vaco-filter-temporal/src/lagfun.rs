//! `lagfun` — a per-pixel one-pole decay that lets brightening through
//! instantly but makes darkening fade out over several frames.
//!
//! `ffmpeg -h filter=lagfun`: `decay` (`0..=1`, default `0.95`), `planes`
//! (bitmask, default `F` = all).
//!
//! # Algorithm, measured (ffmpeg 8.1, 2026-08-23)
//!
//! Pinned with a five-frame single-pixel `gray` stream
//! (`200,50,50,200,0`) through `lagfun=decay=0.5` and reading the exact
//! output bytes back (`200,100,50,200,100`): the first frame passes
//! through unchanged, and every later sample is
//! `state[n] = max(in[n], state[n-1] * decay)` — a pixel that just went dark
//! decays from its previous (brighter) state at rate `decay` per frame
//! rather than snapping down, while a pixel that gets brighter than the
//! decayed state takes over immediately. That is exactly "slowly update
//! darker pixels": brightening is instantaneous, darkening lags.
//!
//! # Independent oracle
//!
//! A strictly non-decreasing input sequence (never gets darker) must pass
//! through completely unchanged for any `decay`, because `max(in[n],
//! state[n-1]*decay) == in[n]` whenever `in[n] >= in[n-1] >= state[n-1]*decay`
//! — an algebraic property of the `max`, not a claim about this file's
//! particular loop. `decay=0` (unconditionally allowed by the option range)
//! makes every frame after the first collapse to `max(in[n], 0) = in[n]`,
//! i.e. the identity — a second, distinct closed-form check.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{PlaneBuf, VIDEO_PAD, copy_meta, f64_opt, plane_dims, planes_mask_opt, sample_layout};

pub const DESC: FilterDesc = FilterDesc {
    name: "lagfun",
    description: "Slowly update darker pixels.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, Copy)]
pub(crate) struct Options {
    decay: f32,
    planes: u8,
}

#[derive(Debug)]
pub(crate) struct Filter {
    opts: Options,
    state: Vec<Option<PlaneBuf>>,
}

impl Filter {
    pub(crate) fn new(opts: Options) -> Self {
        Self {
            opts,
            state: Vec::new(),
        }
    }

    fn step(&mut self, frame: Frame) -> FrameOut {
        let Some(format) = frame.pixel_format() else {
            return FrameOut::One(frame);
        };
        let Some((width, height)) = frame.dimensions() else {
            return FrameOut::One(frame);
        };
        let mut out = frame.clone();
        out.make_writable();
        if self.state.len() < frame.plane_count() {
            self.state.resize_with(frame.plane_count(), || None);
        }

        for plane_idx in 0..frame.plane_count() {
            #[allow(clippy::cast_possible_truncation, reason = "plane index is tiny")]
            let bit = 1u8 << (plane_idx as u8).min(7);
            if self.opts.planes & bit == 0 {
                continue;
            }
            let Some((bytes, max_val)) = sample_layout(format, plane_idx.min(255) as u8) else {
                continue;
            };
            let (pw, ph) = plane_dims(format, width, height, plane_idx);
            let Some(plane) = frame.plane(plane_idx) else {
                continue;
            };
            let current = PlaneBuf::read(plane, pw, ph, bytes, max_val);
            let mut next = current.clone();

            if let Some(Some(prev_state)) = self.state.get(plane_idx) {
                for y in 0..ph {
                    for x in 0..pw {
                        let decayed = prev_state.get(x, y) * self.opts.decay;
                        next.set(x, y, current.get(x, y).max(decayed));
                    }
                }
            }
            if let Some(mut dst) = out.plane_mut(plane_idx) {
                next.write(&mut dst, bytes);
            }
            if let Some(slot) = self.state.get_mut(plane_idx) {
                *slot = Some(next);
            }
        }
        copy_meta(&mut out, &frame);
        FrameOut::One(out)
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        Ok(self.step(frame))
    }

    fn flush_state(&mut self) {
        self.state.clear();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    #[allow(clippy::cast_possible_truncation, reason = "decay is clamped to 0..=1")]
    let decay = f64_opt(req, "decay", 0.95).clamp(0.0, 1.0) as f32;
    let planes = planes_mask_opt(req, &["planes"], 0x0F);
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(Options { decay, planes }))),
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
        let mut f = pool.acquire_video(PixFmt::Gray8, 1, 1).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            p.fill(value);
        }
        f
    }

    fn sample(f: &Frame) -> u8 {
        f.plane(0).unwrap().row(0).unwrap()[0]
    }

    #[test]
    fn measured_five_frame_sequence_matches_the_reference_bytes() {
        let mut f = Filter::new(Options {
            decay: 0.5,
            planes: 0x0F,
        });
        let input = [200u8, 50, 50, 200, 0];
        let expected = [200u8, 100, 50, 200, 100];
        for (v, want) in input.into_iter().zip(expected) {
            let FrameOut::One(fr) = f.step(frame_of(v)) else {
                panic!("expected a frame")
            };
            assert_eq!(sample(&fr), want);
        }
    }

    #[test]
    fn a_non_decreasing_stream_is_the_identity() {
        let mut f = Filter::new(Options {
            decay: 0.95,
            planes: 0x0F,
        });
        for v in [0u8, 10, 50, 120, 255] {
            let FrameOut::One(fr) = f.step(frame_of(v)) else {
                panic!("expected a frame")
            };
            assert_eq!(sample(&fr), v);
        }
    }

    #[test]
    fn decay_zero_is_the_identity_after_the_first_frame() {
        let mut f = Filter::new(Options {
            decay: 0.0,
            planes: 0x0F,
        });
        for v in [200u8, 0, 0, 5, 0] {
            let FrameOut::One(fr) = f.step(frame_of(v)) else {
                panic!("expected a frame")
            };
            assert_eq!(sample(&fr), v);
        }
    }
}
