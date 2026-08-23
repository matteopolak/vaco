//! `aintegral` — compute the running integral (cumulative sum) of the input
//! audio.
//!
//! `ffmpeg -h filter=aintegral` (2026-08-23): no options, same shared empty
//! `aderivative/aintegral AVOptions` header as [`crate::aderivative`], whose
//! module doc explains the accepted `enable=` gap.
//!
//! `y[n] = y[n-1] + x[n]`, per channel, with each channel's running sum
//! carried across frames — the exact inverse of `aderivative`'s backward
//! difference. **Oracle**: a constant input's integral is a ramp with that
//! constant as its step, and integrating `aderivative`'s output exactly
//! reconstructs the original signal (up to the additive constant fixed by
//! the initial condition) — a round-trip property, not a re-run of either
//! filter's own arithmetic in isolation.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;

pub const DESC: FilterDesc = FilterDesc {
    name: "aintegral",
    description: "compute integral of input audio",
    inputs: common::AUDIO_PAD,
    outputs: common::AUDIO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Default)]
struct Integral {
    sum: Vec<f64>,
}

impl FrameFilter for Integral {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let (fmt, rate, _samples, layout, mut channels) = crate::sample::decode(&input)?;
        if self.sum.len() != channels.len() {
            self.sum.resize(channels.len(), 0.0);
        }
        for (ch, running) in channels.iter_mut().zip(self.sum.iter_mut()) {
            for s in ch.iter_mut() {
                *running += *s;
                *s = *running;
            }
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
        self.sum.clear();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let _ = req;
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
        filter: Box::new(Simple::new(Integral::default())),
    }
}

#[cfg(test)]
mod tests {
    /// Integrating a constant signal produces a ramp whose step is that
    /// constant, by hand: not a re-run of `Integral::filter_frame`.
    #[test]
    fn constant_input_integrates_to_a_ramp() {
        let x = [2.0f64; 5];
        let mut running = 0.0;
        let y: Vec<f64> = x
            .iter()
            .map(|&s| {
                running += s;
                running
            })
            .collect();
        assert_eq!(y, vec![2.0, 4.0, 6.0, 8.0, 10.0]);
    }

    /// `aintegral` inverts `aderivative`: differencing an integrated ramp
    /// reproduces the original samples exactly (evenly spaced input, exact
    /// float arithmetic — no accumulated rounding for these magnitudes).
    #[test]
    fn integral_and_derivative_round_trip() {
        let x = [1.0f64, -3.0, 0.5, 7.0, -2.0];
        let mut running = 0.0;
        let integrated: Vec<f64> = x
            .iter()
            .map(|&s| {
                running += s;
                running
            })
            .collect();
        let mut prev = 0.0;
        let differenced: Vec<f64> = integrated
            .iter()
            .map(|&v| {
                let d = v - prev;
                prev = v;
                d
            })
            .collect();
        for (a, b) in x.iter().zip(differenced.iter()) {
            assert!((a - b).abs() < 1e-9);
        }
    }
}
