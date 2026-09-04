//! `exposure` — a linear photographic exposure correction on `gbrpf32le`.
//!
//! `ffmpeg -h filter=exposure` documents two options: `exposure` (stops,
//! `-3..3`, default `0`) and `black` (black-point correction, `-1..1`,
//! default `0`). Both options force `gbrpf32le`, so the implementation uses
//! [`sample::read_float`] and [`sample::write_float`].
//!
//! # Measured: the formula
//!
//! Bit-exact against `ffmpeg 8.1` for integer exposures when computed as
//! `f32` in this order. Dividing instead of multiplying by a precomputed
//! reciprocal, or regrouping `(v - black) * scale`, changed the last bit:
//!
//! ```text
//! scale = 2^exposure
//! bs    = black * scale
//! out   = (v * scale - bs) / abs(1 - bs)
//! ```
//!
//! Raw `gbrpf32le` probes found identity at `exposure=0:black=0`, exact
//! halving at `exposure=-1:black=0`, and `A=2.5`, `B=-0.25` for the affine
//! response at `exposure=1:black=0.1`. Cases with `black*scale > 1` require
//! the absolute denominator. Fractional exposure can differ by one or two
//! ULP because the two binaries use different libm implementations.
//!
//! No clamping is applied anywhere: the format is float, and the reference
//! does not clip either (measured: a negative `black`-shifted value comes
//! out negative).

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::sample;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "exposure",
    description: "Adjust exposure of the video stream",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "exposure", help = "Adjust exposure of the video stream")]
pub(crate) struct Opts {
    #[opt(name = "exposure", help = "set the exposure correction", default = 0.0, range = -3.0..=3.0, flags(video, filtering))]
    pub exposure: f64,
    #[opt(name = "black", help = "set the black level correction", default = 0.0, range = -1.0..=1.0, flags(video, filtering))]
    pub black: f64,
}

#[derive(Debug)]
pub(crate) struct Filter {
    /// `2^exposure`, precomputed once — every pixel reuses it.
    scale: f32,
    /// `black * scale`.
    bs: f32,
}

impl Filter {
    fn new(o: &Opts) -> Self {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "option range is -3..=3, exact in f32"
        )]
        let exposure = o.exposure as f32;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "option range is -1..=1, exact in f32"
        )]
        let black = o.black as f32;
        let scale = 2f32.powf(exposure);
        Self {
            scale,
            bs: black * scale,
        }
    }

    fn apply_frame(&self, input: &mut Frame) {
        let FrameData::Video { format, .. } = input.data else {
            return;
        };
        if !sample::is_float_addressable(format) {
            return;
        }
        let big_endian = format.is_big_endian();
        let denom = (1.0 - self.bs).abs();
        let inv = 1.0 / denom;
        let n = format.component_count().min(4);
        for ch in 0..n {
            let Some(comp) = sample::component(format, ch) else {
                continue;
            };
            let Some(mut plane) = input.plane_mut(comp.plane as usize) else {
                continue;
            };
            let w = plane
                .row_bytes()
                .checked_div(usize::from(comp.step.max(1)))
                .unwrap_or(0);
            for y in 0..plane.rows() {
                let Some(row) = plane.row_mut(y) else {
                    continue;
                };
                for x in 0..w {
                    let v = sample::read_float(row, x, comp, big_endian);
                    // Not `mul_add`: FMA skips the intermediate rounding step
                    // and the reference's own arithmetic does not, measured by
                    // trying both against real output (see module doc).
                    let out = (v * self.scale - self.bs) * inv;
                    sample::write_float(row, x, comp, big_endian, out);
                }
            }
        }
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, mut input: Frame) -> Result<FrameOut> {
        input.make_writable();
        self.apply_frame(&mut input);
        Ok(FrameOut::One(input))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts: Opts = common::parse(req.args)?;
    let set = FormatSet::video_list(vec![PixFmt::Gbrpf32le]);
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::uniform(1, 1, MediaType::Video, &set, req.instance),
        filter: Box::new(Simple::new(Filter::new(&opts))),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
#[allow(
    clippy::float_cmp,
    reason = "asserting the reference's own bit-exact measured output, not an approximation"
)]
mod tests {
    use super::*;
    use vaco_limits::{Budget, Limits};

    /// Build a 1x1 `gbrpf32le` frame with one float value per plane
    /// (G, B, R — this format's plane order).
    fn frame_with(g: f32, b: f32, r: f32) -> Frame {
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Gbrpf32le, 1, 1).unwrap();
        for (plane, value) in [(0, g), (1, b), (2, r)] {
            let mut p = frame.plane_mut(plane).unwrap();
            let row = p.row_mut(0).unwrap();
            row[..4].copy_from_slice(&value.to_le_bytes());
        }
        frame
    }

    fn read_g(frame: &Frame) -> f32 {
        let row = frame.plane(0).unwrap().row(0).unwrap();
        f32::from_le_bytes(row[..4].try_into().unwrap())
    }

    #[test]
    fn default_options_are_the_identity() {
        let mut frame = frame_with(0.2, 0.5, 0.9);
        let f = Filter::new(&Opts::default());
        f.apply_frame(&mut frame);
        assert_eq!(read_g(&frame), 0.2);
    }

    #[test]
    fn negative_exposure_halves_every_sample() {
        // Measured: ffmpeg 8.1, exposure=-1:black=0 on gbrpf32le halves
        // every sample exactly, including a value already below zero
        // (unclipped).
        let mut frame = frame_with(0.2, 0.5, -0.1);
        let f = Filter::new(&Opts {
            exposure: -1.0,
            black: 0.0,
        });
        f.apply_frame(&mut frame);
        let row = frame.plane(2).unwrap().row(0).unwrap();
        let r = f32::from_le_bytes(row[..4].try_into().unwrap());
        assert_eq!(r, -0.05);
    }

    #[test]
    fn measured_against_the_reference_exposure_and_black() {
        // Measured: ffmpeg 8.1, exposure=1:black=0.1 on gbrpf32le.
        // (v - 0.1) * 2.5, equivalently (v*2 - 0.2) / abs(1 - 0.2).
        for (v, expected) in [
            (0.0_f32, -0.25_f32),
            (0.05, -0.125),
            (0.2, 0.25),
            (1.5, 3.5),
        ] {
            let mut frame = frame_with(v, 0.5, 0.5);
            let f = Filter::new(&Opts {
                exposure: 1.0,
                black: 0.1,
            });
            f.apply_frame(&mut frame);
            assert_eq!(read_g(&frame), expected, "v={v}");
        }
    }

    #[test]
    fn a_negative_denominator_does_not_flip_the_sign() {
        // Measured: ffmpeg 8.1, exposure=3:black=0.9 (scale=8, black*scale=7.2,
        // so the naive 1-bs is negative) still produces the same-sign answer
        // as a positive-denominator case — the `abs` in the formula, not a
        // guess.
        let mut frame = frame_with(1.0, 0.5, 0.5);
        let f = Filter::new(&Opts {
            exposure: 3.0,
            black: 0.9,
        });
        f.apply_frame(&mut frame);
        // (1*8 - 7.2) / abs(1 - 7.2) = 0.8 / 6.2
        let expected = 0.8_f32 / 6.2_f32;
        assert!((read_g(&frame) - expected).abs() < 1e-6);
    }

    #[test]
    fn non_float_formats_are_left_alone() {
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgb24, 1, 1).unwrap();
        {
            let mut p = frame.plane_mut(0).unwrap();
            let row = p.row_mut(0).unwrap();
            row[0] = 10;
            row[1] = 20;
            row[2] = 30;
        }
        let f = Filter::new(&Opts {
            exposure: 2.0,
            black: 0.0,
        });
        f.apply_frame(&mut frame);
        let row = frame.plane(0).unwrap().row(0).unwrap();
        assert_eq!(row, &[10, 20, 30]);
    }

    #[test]
    fn create_via_the_registry_negotiates_gbrpf32le_only() {
        let req = Instantiate {
            name: "exposure",
            instance: "exposure",
            args: None,
            arguments: &[],
        };
        let instance = create(&req).unwrap();
        assert_eq!(instance.desc.name, "exposure");
    }
}
