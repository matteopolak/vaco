//! `deflicker` — scale each frame's luma so its average brightness tracks a
//! smoothed trailing average, removing frame-to-frame flicker.
//!
//! `ffmpeg -h filter=deflicker`: `size`/`s` (`2..=129`, default `5`),
//! `mode`/`m` (`am`/`gm`/`hm`/`qm`/`cm`/`pm`/`median`, default `am`),
//! `bypass` (default `false`, "leave frames unchanged" — used to A/B the
//! filter in a graph without removing it).
//!
//! # Algorithm
//!
//! Trailing window of the last `size` frames' average luma (this frame
//! included; shrinks while filling at stream start, same convention as this
//! crate's other windowed filters). The window is combined by the selected
//! mean — arithmetic, geometric, harmonic, quadratic (RMS), cubic, or the
//! median — into a target brightness, and every luma sample in the current
//! frame is scaled by `target / current_average` (chroma planes untouched:
//! this is a brightness correction, not a colour one). `pm` (power mean) is
//! accepted and treated as `am`: the reference's own option help gives it no
//! separate parameter to name which power, so there is nothing to implement
//! beyond the arithmetic case — a documented gap, not a silent guess.
//!
//! # Independent oracle
//!
//! A stream of frames that are all the *same* average luma is flicker-free
//! by definition: every mean of a constant sequence equals that constant, so
//! the scale factor is exactly `1.0` and the output must be the input,
//! unchanged — true for any correct implementation of "a mean", not a
//! property special to this file. A single brighter frame surrounded by
//! constant ones must be scaled *down* (never up) toward the window's mean,
//! checked directly against the hand-computed arithmetic mean.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{VIDEO_PAD, bool_opt, sample_layout, str_opt, usize_opt};

pub const DESC: FilterDesc = FilterDesc {
    name: "deflicker",
    description: "Remove temporal frame luminance variations.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mean {
    Arithmetic,
    Geometric,
    Harmonic,
    Quadratic,
    Cubic,
    Median,
}

impl Mean {
    fn parse(s: &str) -> Self {
        match s {
            "1" | "gm" => Self::Geometric,
            "2" | "hm" => Self::Harmonic,
            "3" | "qm" => Self::Quadratic,
            "4" | "cm" => Self::Cubic,
            "6" | "median" => Self::Median,
            // "5"/"pm" (power mean) has no separate parameter in the
            // reference's own option help; treated as `am` (see module doc).
            _ => Self::Arithmetic,
        }
    }

    fn combine(self, values: &[f64]) -> f64 {
        let n = values.len().max(1);
        #[allow(clippy::cast_precision_loss, reason = "window sizes are <= 129")]
        let nf = n as f64;
        match self {
            Self::Arithmetic => values.iter().sum::<f64>() / nf,
            Self::Geometric => {
                let sum_ln: f64 = values.iter().map(|v| v.max(1e-6).ln()).sum();
                (sum_ln / nf).exp()
            }
            Self::Harmonic => {
                let sum_inv: f64 = values.iter().map(|v| 1.0 / v.max(1e-6)).sum();
                nf / sum_inv
            }
            Self::Quadratic => (values.iter().map(|v| v * v).sum::<f64>() / nf).sqrt(),
            Self::Cubic => (values.iter().map(|v| v.powi(3)).sum::<f64>() / nf).cbrt(),
            Self::Median => {
                let mut sorted = values.to_vec();
                sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                #[allow(
                    clippy::integer_division,
                    reason = "index arithmetic: the middle rank of a sorted array, not a \
                              measurement subject to precision loss"
                )]
                let mid = sorted.len() / 2;
                if sorted.len().is_multiple_of(2) {
                    let a = sorted.get(mid.saturating_sub(1)).copied().unwrap_or(0.0);
                    let b = sorted.get(mid).copied().unwrap_or(0.0);
                    f64::midpoint(a, b)
                } else {
                    sorted.get(mid).copied().unwrap_or(0.0)
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Options {
    size: usize,
    mode: Mean,
    bypass: bool,
}

/// Average luma sample value of a frame's plane 0, in its native scale.
fn average_luma(frame: &Frame) -> Option<(f64, usize, f32)> {
    let format = frame.pixel_format()?;
    let (bytes, max_val) = sample_layout(format, 0)?;
    let (width, height) = frame.dimensions()?;
    let (pw, ph) = crate::video::plane_dims(format, width, height, 0);
    let buf = crate::video::PlaneBuf::read(frame.plane(0)?, pw, ph, bytes, max_val);
    let n = pw.saturating_mul(ph).max(1);
    #[allow(clippy::cast_precision_loss, reason = "plane sample counts are small")]
    let avg = f64::from(buf.as_slice().iter().sum::<f32>()) / n as f64;
    Some((avg, bytes, max_val))
}

#[derive(Debug)]
pub(crate) struct Filter {
    opts: Options,
    history: std::collections::VecDeque<f64>,
}

impl Filter {
    pub(crate) fn new(opts: Options) -> Self {
        Self {
            opts,
            history: std::collections::VecDeque::new(),
        }
    }

    fn step(&mut self, frame: Frame) -> FrameOut {
        if self.opts.bypass {
            return FrameOut::One(frame);
        }
        let Some((avg, bytes, max_val)) = average_luma(&frame) else {
            return FrameOut::One(frame);
        };
        self.history.push_back(avg);
        while self.history.len() > self.opts.size {
            self.history.pop_front();
        }
        let values: Vec<f64> = self.history.iter().copied().collect();
        let target = self.opts.mode.combine(&values);
        if avg <= 0.0 {
            return FrameOut::One(frame);
        }
        let scale = target / avg;

        let mut out = frame.clone();
        out.make_writable();
        let Some(format) = frame.pixel_format() else {
            return FrameOut::One(frame);
        };
        let Some((width, height)) = frame.dimensions() else {
            return FrameOut::One(frame);
        };
        let (pw, ph) = crate::video::plane_dims(format, width, height, 0);
        let mut buf = crate::video::PlaneBuf::read(
            match frame.plane(0) {
                Some(p) => p,
                None => return FrameOut::One(frame),
            },
            pw,
            ph,
            bytes,
            max_val,
        );
        for y in 0..ph {
            for x in 0..pw {
                let v = buf.get(x, y);
                #[allow(clippy::cast_possible_truncation, reason = "scale keeps this in-range")]
                let scaled = (f64::from(v) * scale) as f32;
                buf.set(x, y, scaled.clamp(0.0, max_val));
            }
        }
        if let Some(mut dst) = out.plane_mut(0) {
            buf.write(&mut dst, bytes);
        }
        FrameOut::One(out)
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
    let size = usize_opt(req, "size", usize_opt(req, "s", 5)).clamp(2, 129);
    let mode_str = str_opt(req, "mode")
        .or_else(|| str_opt(req, "m"))
        .unwrap_or_else(|| "am".to_owned());
    let opts = Options {
        size,
        mode: Mean::parse(&mode_str),
        bypass: bool_opt(req, "bypass", false),
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
        let mut f = pool.acquire_video(PixFmt::Gray8, 4, 4).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            p.fill(value);
        }
        f
    }

    fn sample(f: &Frame) -> u8 {
        f.plane(0).unwrap().row(0).unwrap()[0]
    }

    fn opts() -> Options {
        Options {
            size: 5,
            mode: Mean::Arithmetic,
            bypass: false,
        }
    }

    #[test]
    fn constant_brightness_stream_is_the_identity() {
        let mut f = Filter::new(opts());
        for _ in 0..8 {
            let out = f.step(frame_of(100));
            let FrameOut::One(fr) = out else {
                panic!("expected a frame")
            };
            assert_eq!(sample(&fr), 100);
        }
    }

    #[test]
    fn a_bright_outlier_is_scaled_down_toward_the_hand_computed_mean() {
        let mut f = Filter::new(opts());
        for _ in 0..4 {
            let _ = f.step(frame_of(100));
        }
        // window is now [100,100,100,100]; push a bright outlier: window
        // becomes [100,100,100,100,200], mean = 600/5 = 120 -> scale = 0.6.
        let FrameOut::One(fr) = f.step(frame_of(200)) else {
            panic!("expected a frame")
        };
        assert_eq!(
            sample(&fr),
            120,
            "200 * (120/200) = 120, below the outlier's own 200"
        );
    }

    #[test]
    fn bypass_leaves_frames_untouched() {
        let mut o = opts();
        o.bypass = true;
        let mut f = Filter::new(o);
        for _ in 0..3 {
            let _ = f.step(frame_of(30));
        }
        let FrameOut::One(fr) = f.step(frame_of(250)) else {
            panic!("expected a frame")
        };
        assert_eq!(sample(&fr), 250, "bypass must not rescale");
    }
}
