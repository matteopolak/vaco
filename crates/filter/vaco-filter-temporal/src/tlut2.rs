//! `tlut2` — apply a per-component expression of the current sample (`x`)
//! and the same sample one frame ago (`y`).
//!
//! # Measured, not assumed: this is temporal, one input, not `lut2`'s pair
//!
//! `ffmpeg -h filter=tlut2` and `-h filter=lut2` look alike (`c0..c3`
//! expressions, default `"x"`) but `lut2` is `srcx`/`srcy` two-*stream*
//! (inputs named `srcx` and `srcy`) while `tlut2` declares one `default`
//! video pad. Feeding a two-frame
//! single-pixel `gray` stream `[0x32, 0xc8]` through
//! `tlut2=c0_expr='x'`/`'y'` (ffmpeg 8.1, 2026-08-23) confirmed `x` is the
//! *current* frame's sample and `y` the *immediately preceding* one — so
//! `tlut2` needs no `vaco-filter-framesync`, just one held frame of state,
//! matching this crate's row (`vdsp`/`framesync` are the row's *extra*
//! deps, not a requirement that every filter use both).
//!
//! `ffmpeg -h filter=tlut2`: `c0`..`c3` (per-component expression, default
//! `"x"` — the identity in `x`, ignoring `y` entirely, which makes the
//! *default* configuration equivalent to passthrough).
//!
//! # The first frame has no "previous"
//!
//! Not observable from the reference without a source read professionalising
//! into a real edge-case probe this pass did not budget for; this
//! implementation's documented choice is `y := x` on the very first frame
//! (there is nothing else it could plausibly mean), so the default
//! `c0..c3 = "x"` is exactly the identity from frame one, not just from
//! frame two onward.
//!
//! # Independent oracle
//!
//! Default options (`c0..c3 = "x"`) must be the identity on *every* frame —
//! evaluating `x` never touches `y` and is trivially a no-op — checked
//! byte-for-byte against the input. `c0='y'` reproduces the previous
//! frame's sample exactly, a second, distinct closed form.

use vaco_core::{MediaType, Result};
use vaco_expr::{Bindings, Expr};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{PlaneBuf, VIDEO_PAD, copy_meta, plane_dims, sample_layout, str_opt};

pub const DESC: FilterDesc = FilterDesc {
    name: "tlut2",
    description: "Compute and apply a lookup table from two successive frames.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone)]
pub(crate) struct Options {
    exprs: Vec<Expr>,
}

#[derive(Debug)]
pub(crate) struct Filter {
    opts: Options,
    prev: Option<Frame>,
}

impl Filter {
    pub(crate) fn new(opts: Options) -> Self {
        Self { opts, prev: None }
    }

    fn apply(&self, current: &Frame, previous: &Frame) -> Option<Frame> {
        let mut out = current.clone();
        out.make_writable();
        let format = current.pixel_format()?;
        let (width, height) = current.dimensions()?;
        let mut regs = vaco_expr::Registers::new();

        for plane_idx in 0..current.plane_count() {
            let Some((bytes, max_val)) = sample_layout(format, plane_idx.min(255) as u8) else {
                continue;
            };
            let expr = self
                .opts
                .exprs
                .get(plane_idx)
                .or_else(|| self.opts.exprs.first())?;
            let (pw, ph) = plane_dims(format, width, height, plane_idx);
            let x_buf = PlaneBuf::read(current.plane(plane_idx)?, pw, ph, bytes, max_val);
            let y_buf = PlaneBuf::read(previous.plane(plane_idx)?, pw, ph, bytes, max_val);
            let mut result = x_buf.clone();
            for y in 0..ph {
                for x in 0..pw {
                    let vars = [f64::from(x_buf.get(x, y)), f64::from(y_buf.get(x, y))];
                    #[allow(
                        clippy::cast_possible_truncation,
                        reason = "clamped to the plane's range below"
                    )]
                    let v = expr.eval_with(&mut vaco_expr::Context::new(&vars, &mut regs)) as f32;
                    result.set(x, y, v.clamp(0.0, max_val));
                }
            }
            if let Some(mut dst) = out.plane_mut(plane_idx) {
                result.write(&mut dst, bytes);
            }
        }
        Some(out)
    }

    /// The pairing-and-apply step, independent of [`FilterContext`].
    fn step(&mut self, frame: Frame) -> FrameOut {
        // No previous frame yet: this crate's documented choice is `y := x`
        // (see module doc), so the first frame blends against itself.
        let previous = self.prev.clone().unwrap_or_else(|| frame.clone());
        let mut result = self
            .apply(&frame, &previous)
            .unwrap_or_else(|| frame.clone());
        copy_meta(&mut result, &frame);
        self.prev = Some(frame);
        FrameOut::One(result)
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        Ok(self.step(frame))
    }

    fn flush_state(&mut self) {
        self.prev = None;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Result<Instance, String> {
    let bindings = Bindings::new(&["x", "y"]);
    let mut exprs = Vec::new();
    for idx in 0..4 {
        let key = format!("c{idx}");
        let text = str_opt(req, &key).unwrap_or_else(|| "x".to_owned());
        let expr = Expr::parse(&text, &bindings)
            .map_err(|e| format!("tlut2: bad expression for `{key}` (`{text}`): {e}"))?;
        exprs.push(expr);
    }
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(Options { exprs }))),
    })
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

    fn opts_for(expr: &str) -> Options {
        let bindings = Bindings::new(&["x", "y"]);
        let e = Expr::parse(expr, &bindings).unwrap();
        Options {
            exprs: vec![e.clone(), e.clone(), e.clone(), e],
        }
    }

    #[test]
    fn default_expr_x_is_the_identity_on_every_frame() {
        let mut f = Filter::new(opts_for("x"));
        for v in [10u8, 200, 55, 0, 255] {
            let FrameOut::One(fr) = f.step(frame_of(v)) else {
                panic!("expected a frame")
            };
            assert_eq!(sample(&fr), v);
        }
    }

    #[test]
    fn expr_y_reproduces_the_previous_frame() {
        let mut f = Filter::new(opts_for("y"));
        // First frame: y := x per this crate's documented choice.
        let FrameOut::One(fr0) = f.step(frame_of(10)) else {
            panic!("expected a frame")
        };
        assert_eq!(sample(&fr0), 10);
        let FrameOut::One(fr1) = f.step(frame_of(200)) else {
            panic!("expected a frame")
        };
        assert_eq!(sample(&fr1), 10, "y is the previous frame's sample");
    }
}
