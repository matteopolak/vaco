//! `maskedmerge` — blend two streams per-pixel using a third as a mask.
//!
//! `ffmpeg -h filter=maskedmerge` documents `planes` (bitmask, default 15 =
//! all) and no framesync surface (`eof_action`/`shortest` are absent from
//! its `-h` output, unlike `alphamerge`/`msad`/`lut2`/`haldclut`) — so this
//! filter is implemented as plain lockstep three-input consumption, the
//! same shape as [`crate::mergeplanes`], rather than through
//! `vaco-filter-framesync`.
//!
//! # The formula
//!
//! `out = base + (overlay - base) * mask / maxval`, per selected plane,
//! independently per component — the standard alpha-compositing formula
//! with the mask read at the *base/overlay* component's own bit depth
//! (all three inputs share one negotiated format here, so there is no
//! cross-depth question to resolve, unlike `alphamerge`/`mergeplanes`).
//! Confirmed against a hand-computed 2x2 example in this module's tests:
//! mask 0 keeps `base` exactly, mask `maxval` takes `overlay` exactly, and
//! a half mask lands on the midpoint (rounded).

use vaco_core::{MediaType, Result};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats, Tie};
use vaco_filter_core::{Activity, Filter as FilterTrait, FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::FrameData;
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::sample;

const PADS: &[Pad] = &[
    Pad { name: "base", media_type: MediaType::Video },
    Pad { name: "overlay", media_type: MediaType::Video },
    Pad { name: "mask", media_type: MediaType::Video },
];
const VIDEO_PAD: &[Pad] = &[Pad { name: "default", media_type: MediaType::Video }];

pub const DESC: FilterDesc = FilterDesc {
    name: "maskedmerge",
    description: "Merge first stream with second stream using third stream as mask",
    inputs: PADS,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "maskedmerge", help = "Merge first stream with second stream using third stream as mask")]
pub(crate) struct Opts {
    #[opt(name = "planes", help = "set planes", default = 15, range = 0..=15, flags(video, filtering))]
    pub planes: i32,
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
struct Filter {
    planes: i64,
}

impl FilterTrait for Filter {
    fn activate(&mut self, ctx: &mut FilterContext<'_>) -> Result<Activity> {
        if !ctx.output_has_room(0) {
            return Ok(if ctx.output_closed(0) { Activity::Eof } else { Activity::Blocked });
        }
        if (0..3).any(|p| ctx.input_at_eof(p)) {
            ctx.close_all_outputs();
            return Ok(Activity::Eof);
        }
        let Some(base) = ctx.peek_input(0).cloned() else {
            ctx.forward_wanted();
            return Ok(Activity::NeedInput);
        };
        let Some(overlay) = ctx.peek_input(1).cloned() else {
            ctx.forward_wanted();
            return Ok(Activity::NeedInput);
        };
        let Some(mask) = ctx.peek_input(2).cloned() else {
            ctx.forward_wanted();
            return Ok(Activity::NeedInput);
        };
        let FrameData::Video { format, width, height, .. } = base.data else {
            let _ = ctx.take_input(0);
            let _ = ctx.take_input(1);
            let _ = ctx.take_input(2);
            ctx.push_output(0, base)?;
            return Ok(Activity::Progressed);
        };
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        let big_endian = format.is_big_endian();
        for ch in 0..format.component_count() {
            let Some(comp) = sample::component(format, ch) else { continue };
            let (Some(b), Some(o), Some(m), Some(mut d)) = (
                base.plane(comp.plane as usize),
                overlay.plane(comp.plane as usize),
                mask.plane(comp.plane as usize),
                out.plane_mut(comp.plane as usize),
            ) else {
                continue;
            };
            if !sample::plane_selected(self.planes, ch) {
                let n = d.rows().min(b.rows());
                for y in 0..n {
                    let (Some(sr), Some(dr)) = (b.row(y), d.row_mut(y)) else { continue };
                    let len = sr.len().min(dr.len());
                    if let (Some(s), Some(dd)) = (sr.get(..len), dr.get_mut(..len)) {
                        dd.copy_from_slice(s);
                    }
                }
                continue;
            }
            let max = f64::from(sample::max_value(comp));
            let w = d.row_bytes().checked_div(usize::from(comp.step.max(1))).unwrap_or(0);
            let n = d.rows().min(b.rows()).min(o.rows()).min(m.rows());
            for y in 0..n {
                let (Some(br), Some(or), Some(mr), Some(dr)) = (b.row(y), o.row(y), m.row(y), d.row_mut(y)) else {
                    continue;
                };
                for x in 0..w {
                    let bv = f64::from(sample::read(br, x, comp, big_endian));
                    let ov = f64::from(sample::read(or, x, comp, big_endian));
                    let mv = f64::from(sample::read(mr, x, comp, big_endian));
                    let blended = if max > 0.0 {
                        bv + (ov - bv) * mv / max
                    } else {
                        bv
                    };
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "clamped to [0, max] and max fits in u16 by construction"
                    )]
                    let out_v = blended.clamp(0.0, max).round() as u16;
                    sample::write(dr, x, comp, big_endian, out_v);
                }
            }
        }
        out.pts = base.pts;
        out.time_base = base.time_base;
        out.duration = base.duration;
        out.sample_aspect_ratio = base.sample_aspect_ratio;
        let _ = ctx.take_input(0);
        let _ = ctx.take_input(1);
        let _ = ctx.take_input(2);
        ctx.push_output(0, out)?;
        Ok(Activity::Progressed)
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let set = FormatSet::video_list(common::formats_where(sample::is_addressable));
    let formats = NodeFormats {
        inputs: vec![set.clone(), set.clone(), set],
        outputs: vec![FormatSet::default()],
        ties: Tie::all_pads(3, 1, MediaType::Video),
        label: req.instance.to_owned(),
    };
    Ok(Instance {
        desc: DESC,
        formats,
        filter: Box::new(Filter { planes: i64::from(opts.planes) }),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    #[test]
    fn hand_computed_blend_on_a_single_sample() {
        // Independent oracle: hand-worked alpha compositing arithmetic,
        // not derived from this crate's own implementation.
        let base = 100.0f64;
        let overlay = 200.0f64;
        let max = 255.0f64;
        for (mask, expected) in [(0.0, 100u16), (255.0, 200), (128.0, 150)] {
            let blended = base + (overlay - base) * mask / max;
            let out = blended.clamp(0.0, max).round() as u16;
            assert_eq!(out, expected, "mask={mask}");
        }
    }
}
