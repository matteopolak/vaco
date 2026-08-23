//! `lut2` — apply a lookup table computed from two video inputs.
//!
//! `ffmpeg -h filter=lut2` documents `c0`..`c3` (expression, default `"x"`)
//! and `d` (output depth, 0-16, default 0 = same as input), plus the shared
//! `vaco-filter-framesync` surface.
//!
//! # Measured variables
//!
//! Only `x` (input 0's sample) and `y` (input 1's sample) are bound —
//! unlike [`crate::lut`], `w`/`h` are **not** available here (probed and
//! rejected by the reference). `d` genuinely changes the output pixel
//! format's bit depth (`d=9` on `gray` produces `gray9le`); this crate
//! implements the default `d=0` (output depth follows input 0's depth)
//! and, for `d != 0`, parses the option but does not remap to a
//! different-depth sibling format — that needs a generic
//! "same shape, different depth" pixel-format lookup this crate does not
//! yet build (`sample::gray_format_for` only covers the single-plane
//! case `alphaextract`/`extractplanes` need). Documented rather than
//! guessed at.
//!
//! # Shape: two inputs, framesync
//!
//! `vaco-filter-framesync`'s own module doc names `lut2` directly as one of
//! the family [`FsInput::dual`] covers, so this filter is the same shape as
//! `overlay`: input 0 drives, input 1 is sampled.

use vaco_core::{MediaType, Result};
use vaco_expr::{Bindings, Expr};
use vaco_filter_core::adapt::FrameOut;
use vaco_filter_core::negotiate::{FormatSet, NodeFormats, Tie};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_filter_framesync::{FrameSyncEvent, FrameSyncFilter, FrameSyncOpts, FsInput, Synced};
use vaco_frame::FrameData;
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::sample;

const VARS: &[&str] = &["x", "y"];

pub const DESC: FilterDesc = FilterDesc {
    name: "lut2",
    description: "Compute and apply a lookup table from two video inputs",
    inputs: vaco_filter_framesync::mock::DUAL_VIDEO_PADS,
    outputs: vaco_filter_framesync::mock::VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "lut2", help = "Compute and apply a lookup table from two video inputs")]
pub(crate) struct Opts {
    #[opt(name = "c0", help = "set component #0 expression", default = "x".to_owned(), flags(video, filtering))]
    pub c0: String,
    #[opt(name = "c1", help = "set component #1 expression", default = "x".to_owned(), flags(video, filtering))]
    pub c1: String,
    #[opt(name = "c2", help = "set component #2 expression", default = "x".to_owned(), flags(video, filtering))]
    pub c2: String,
    #[opt(name = "c3", help = "set component #3 expression", default = "x".to_owned(), flags(video, filtering))]
    pub c3: String,
    #[opt(name = "d", help = "set output depth (not implemented; parsed only)", default = 0, range = 0..=16, flags(video, filtering))]
    pub d: i32,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        Ok(o)
    }
}

#[derive(Debug)]
pub(crate) struct Lut2 {
    exprs: [Expr; 4],
}

impl Lut2 {
    fn new(o: &Opts) -> std::result::Result<Self, String> {
        let bindings = Bindings::new(VARS);
        let parse = |s: &str| Expr::parse(s, &bindings).map_err(|e| format!("lut2: bad expression `{s}`: {e}"));
        Ok(Self {
            exprs: [parse(&o.c0)?, parse(&o.c1)?, parse(&o.c2)?, parse(&o.c3)?],
        })
    }
}

impl FrameSyncFilter for Lut2 {
    fn on_event(
        &mut self,
        ctx: &mut FilterContext<'_>,
        event: &mut FrameSyncEvent<'_>,
    ) -> Result<FrameOut> {
        let (Some(main), Some(second)) = (event.take(0), event.get(1).cloned()) else {
            return Ok(FrameOut::None);
        };
        let FrameData::Video { format, width, height, .. } = main.data else {
            return Ok(FrameOut::One(main));
        };
        if !sample::is_addressable(format) {
            return Ok(FrameOut::One(main));
        }
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        let big_endian = format.is_big_endian();
        let n = format.component_count().min(4);
        for ch in 0..n {
            let Some(comp) = sample::component(format, ch) else {
                continue;
            };
            let max = f64::from(sample::max_value(comp));
            let Some(src_x) = main.plane(comp.plane as usize) else {
                continue;
            };
            let Some(src_y) = second.plane(comp.plane as usize) else {
                continue;
            };
            let Some(mut dst) = out.plane_mut(comp.plane as usize) else {
                continue;
            };
            let Some(expr) = self.exprs.get(ch) else {
                continue;
            };
            let plane_width = dst
                .row_bytes()
                .checked_div(usize::from(comp.step.max(1)))
                .unwrap_or(0);
            for y in 0..dst.rows() {
                let (Some(rx), Some(ry)) = (src_x.row(y), src_y.row(y)) else {
                    continue;
                };
                let Some(rd) = dst.row_mut(y) else { continue };
                for x in 0..plane_width {
                    let vx = f64::from(sample::read(rx, x, comp, big_endian));
                    let vy = f64::from(sample::read(ry, x, comp, big_endian));
                    let out_v = expr.eval(&[vx, vy]);
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "clamped to [0, max] and max fits in u16 by construction"
                    )]
                    let out_v = out_v.clamp(0.0, max).round() as u16;
                    sample::write(rd, x, comp, big_endian, out_v);
                }
            }
        }
        out.pts = main.pts;
        out.time_base = main.time_base;
        out.duration = main.duration;
        out.sample_aspect_ratio = main.sample_aspect_ratio;
        Ok(FrameOut::One(out))
    }

    fn inputs(&self, n: usize) -> Vec<FsInput> {
        FsInput::dual(n)
    }

    fn opts(&self) -> FrameSyncOpts {
        FrameSyncOpts::default()
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Lut2::new(&opts)?;
    let set = FormatSet::video_list(common::formats_where(sample::is_addressable));
    let formats = NodeFormats {
        inputs: vec![set.clone(), set.clone()],
        outputs: vec![set],
        ties: Tie::all_pads(2, 1, MediaType::Video),
        label: req.instance.to_owned(),
    };
    Ok(Instance {
        desc: DESC,
        formats,
        filter: Box::new(Synced::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::float_cmp, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn default_expression_is_x_ignoring_y() {
        let opts = Opts {
            c0: "x".to_owned(),
            c1: "x".to_owned(),
            c2: "x".to_owned(),
            c3: "x".to_owned(),
            d: 0,
        };
        let lut2 = Lut2::new(&opts).unwrap();
        assert_eq!(lut2.exprs[0].eval(&[42.0, 99.0]), 42.0);
    }

    #[test]
    fn sum_expression_adds_both_inputs() {
        let opts = Opts {
            c0: "x+y".to_owned(),
            c1: "x".to_owned(),
            c2: "x".to_owned(),
            c3: "x".to_owned(),
            d: 0,
        };
        let lut2 = Lut2::new(&opts).unwrap();
        assert_eq!(lut2.exprs[0].eval(&[3.0, 4.0]), 7.0);
    }
}
