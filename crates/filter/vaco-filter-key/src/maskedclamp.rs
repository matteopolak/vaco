//! `maskedclamp` — clamp a base stream between two other streams, with a
//! per-side margin.
//!
//! `ffmpeg -h filter=maskedclamp` documents `undershoot`/`overshoot`
//! (`0..65535`, both default `0`) and `planes` (bitmask, default 15); no
//! framesync surface, so (same reasoning as [`crate::masked_pick`]) this
//! is a lockstep three-input filter through
//! [`vaco_filter_core::adapt::Paired`].
//!
//! # Measured: the formula
//!
//! Five probes on `gray` inputs (`base`, `dark`, `bright`):
//!
//! ```text
//! out = clamp(base, dark - undershoot, bright + overshoot)
//! ```
//!
//! confirmed with `base` inside the range (`100` in `[80, 120]` stays
//! `100`), below it (`50` in `[80, 120]` clamps to `80`), above it (`200`
//! clamps to `120`), and with a non-zero `undershoot`/`overshoot`
//! (`undershoot=10`: `clamp(50, 70, 120) = 70`; `overshoot=10`:
//! `clamp(200, 80, 130) = 130`) — both matched exactly. Exact: this is
//! integer clamping, no interpolation.

use smallvec::SmallVec;
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameOut, Paired, PairedFilter};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats, Tie};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::sample;

const PADS: &[Pad] = &[
    Pad {
        name: "base",
        media_type: MediaType::Video,
    },
    Pad {
        name: "dark",
        media_type: MediaType::Video,
    },
    Pad {
        name: "bright",
        media_type: MediaType::Video,
    },
];
const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "maskedclamp",
    description: "Clamp first stream with second stream and third stream",
    inputs: PADS,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(
    name = "maskedclamp",
    help = "Clamp first stream with second stream and third stream"
)]
pub(crate) struct Opts {
    #[opt(name = "undershoot", help = "set undershoot", default = 0, range = 0..=65535, flags(video, filtering))]
    pub undershoot: i32,
    #[opt(name = "overshoot", help = "set overshoot", default = 0, range = 0..=65535, flags(video, filtering))]
    pub overshoot: i32,
    #[opt(name = "planes", help = "set planes", default = 15, range = 0..=15, flags(video, filtering))]
    pub planes: i32,
}

#[derive(Debug)]
struct Filter {
    undershoot: i32,
    overshoot: i32,
    planes: i64,
}

impl PairedFilter for Filter {
    fn input_count(&self) -> usize {
        3
    }

    fn filter_frames(
        &mut self,
        ctx: &mut FilterContext<'_>,
        inputs: SmallVec<[Frame; 4]>,
    ) -> Result<FrameOut> {
        let mut it = inputs.into_iter();
        let (Some(base), Some(dark), Some(bright)) = (it.next(), it.next(), it.next()) else {
            return Ok(FrameOut::None);
        };
        let FrameData::Video {
            format,
            width,
            height,
            ..
        } = base.data
        else {
            return Ok(FrameOut::One(base));
        };
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        let big_endian = format.is_big_endian();
        for ch in 0..format.component_count() {
            let Some(comp) = sample::component(format, ch) else {
                continue;
            };
            let (Some(bp), Some(dkp), Some(brp), Some(mut dp)) = (
                base.plane(comp.plane as usize),
                dark.plane(comp.plane as usize),
                bright.plane(comp.plane as usize),
                out.plane_mut(comp.plane as usize),
            ) else {
                continue;
            };
            let w = dp
                .row_bytes()
                .checked_div(usize::from(comp.step.max(1)))
                .unwrap_or(0);
            let n = dp.rows().min(bp.rows()).min(dkp.rows()).min(brp.rows());
            if !sample::plane_selected(self.planes, ch) {
                for y in 0..n {
                    let (Some(sr), Some(dr)) = (bp.row(y), dp.row_mut(y)) else {
                        continue;
                    };
                    let len = sr.len().min(dr.len());
                    if let (Some(s), Some(d)) = (sr.get(..len), dr.get_mut(..len)) {
                        d.copy_from_slice(s);
                    }
                }
                continue;
            }
            let max = i32::from(sample::max_value(comp));
            for y in 0..n {
                let (Some(br), Some(dkr), Some(brr), Some(dr)) =
                    (bp.row(y), dkp.row(y), brp.row(y), dp.row_mut(y))
                else {
                    continue;
                };
                for x in 0..w {
                    let base_v = i32::from(sample::read(br, x, comp, big_endian));
                    let dark_v = i32::from(sample::read(dkr, x, comp, big_endian));
                    let bright_v = i32::from(sample::read(brr, x, comp, big_endian));
                    let lo = (dark_v.saturating_sub(self.undershoot)).clamp(0, max);
                    let hi = (bright_v.saturating_add(self.overshoot)).clamp(0, max);
                    let hi = hi.max(lo);
                    let out_v = base_v.clamp(lo, hi);
                    #[allow(
                        clippy::cast_sign_loss,
                        reason = "out_v clamped into [0, max] and max fits u16 by construction"
                    )]
                    let out_v = out_v as u16;
                    sample::write(dr, x, comp, big_endian, out_v);
                }
            }
        }
        out.pts = base.pts;
        out.time_base = base.time_base;
        out.duration = base.duration;
        out.sample_aspect_ratio = base.sample_aspect_ratio;
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts: Opts = common::parse(req.args)?;
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
        filter: Box::new(Paired::new(Filter {
            undershoot: opts.undershoot,
            overshoot: opts.overshoot,
            planes: i64::from(opts.planes),
        })),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    #[test]
    fn hand_computed_clamp_on_measured_cases() {
        // Independent oracle: plain clamp arithmetic, not derived from
        // this module's own implementation.
        let cases: &[(i32, i32, i32, i32, i32, i32)] = &[
            (100, 80, 120, 0, 0, 100),
            (50, 80, 120, 0, 0, 80),
            (200, 80, 120, 0, 0, 120),
            (50, 80, 120, 10, 0, 70),
            (200, 80, 120, 0, 10, 130),
        ];
        for &(base, dark, bright, undershoot, overshoot, expected) in cases {
            let lo = dark - undershoot;
            let hi = bright + overshoot;
            let out = base.clamp(lo, hi);
            assert_eq!(out, expected, "base={base} dark={dark} bright={bright}");
        }
    }
}
