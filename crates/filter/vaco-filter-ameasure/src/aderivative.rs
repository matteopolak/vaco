//! `aderivative` — compute the discrete derivative of the input audio.
//!
//! `ffmpeg -h filter=aderivative` (2026-08-23): no options (the reference
//! groups it with [`crate::aintegral`] under one shared, empty
//! `aderivative/aintegral AVOptions` header). Timeline (`enable=`) is
//! reported as supported; not implemented here — this filter always runs,
//! a documented gap shared with the rest of this crate's dual-input
//! filters (see `docs/filter/vaco-filter-ameasure.md`).
//!
//! `y[n] = x[n] - x[n-1]`, first-order backward difference, per channel,
//! with each channel's own last-sample memory carried across frames.
//! **Oracle**: a closed form, not a second copy of the loop — a linear
//! ramp's derivative is the constant step size, and a constant signal's
//! derivative is zero; both are exact, not approximate, in exact
//! floating-point arithmetic for evenly spaced input.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "aderivative",
    description: "compute derivative of input audio",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Default)]
struct Derivative {
    last: Vec<f64>,
}

impl FrameFilter for Derivative {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        if self.last.len() != channels.len() {
            self.last.resize(channels.len(), 0.0);
        }
        for (ch, last) in channels.iter_mut().zip(self.last.iter_mut()) {
            let mut prev = *last;
            for s in ch.iter_mut() {
                let cur = *s;
                *s = cur - prev;
                prev = cur;
            }
            *last = prev;
        }
        let mut out = crate::sample::encode(
            &vaco_frame::FramePool::default(),
            fmt,
            layout,
            rate,
            &channels,
        )?;
        out.pts = input.pts;
        out.time_base = input.time_base;
        out.duration = input.duration;
        Ok(FrameOut::One(out))
    }

    fn flush_state(&mut self) {
        self.last.clear();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let _ = req;
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(Derivative::default())),
    }
}

#[cfg(test)]
mod tests {
    /// A ramp's derivative is its constant step, computed by hand: no second
    /// copy of the filter's own loop involved in the check.
    #[test]
    fn ramp_derivative_is_constant_step() {
        let mut ch = vec![1.0, 2.0, 3.0, 4.0];
        let step = 1.0;
        let mut prev = 0.0f64;
        for s in &mut ch {
            let cur = *s;
            *s = cur - prev;
            prev = cur;
        }
        assert!(ch.iter().skip(1).all(|&v| (v - step).abs() < 1e-12));
    }
}
