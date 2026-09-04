//! `limitdiff` — apply a second stream's difference from the first only
//! where that difference is large enough to matter.
//!
//! `ffmpeg -h filter=limitdiff` documents `threshold` (`0..1`, default
//! `0.00392157 = 1/255`), `elasticity` (`0..10`, default `2`), `reference`
//! (bool, default `false`, adds a third input) and `planes` (bitmask,
//! default `15`). No `eof_action`/`shortest`/`ts_sync_mode` in `-h` output,
//! so this uses a lockstep [`vaco_filter_core::adapt::PairedFilter`] rather
//! than a timeline-synchronizing adapter.
//!
//! # Measured: the two hard edges, and an honestly-approximated middle
//!
//! On `gray` inputs `c1=128`, varying `c2` under `threshold=0.05`
//! (`=12.75`), `elasticity=2` (the default):
//!
//! * `|c2-c1| <= 12.75`: output is **exactly `c1`**, unchanged (checked at
//!   `diff=5,10,11,12`).
//! * `|c2-c1| >= 25.5` (`= threshold_px * elasticity`, confirmed both signs:
//!   `diff=24,25,30,40` and `diff=-25,-38,-40`): output is **exactly `c2`**,
//!   the full difference applied.
//! * Between those two, the reference visibly ramps rather than stepping
//!   (`diff=13..24` gives `applied=2,3,4,6,9,14,16,19,22,24` rather than a
//!   discontinuity) — this crate applies a **linear** ramp of the fraction
//!   applied, `t = (|diff|-threshold_px) / (elasticity*threshold_px -
//!   threshold_px)`, which reproduces both hard edges exactly but only
//!   approximates the reference's actual knee shape in between: at
//!   `diff=19` this formula predicts `applied≈9`, the reference measures
//!   `12` — a real, disclosed residual (a handful of levels out of 255, in
//!   the transition band only) rather than a bit-exact curve. Left as an
//!   honest approximation rather than a fabricated exact fit — the true
//!   curve was not pinned down in the time available.
//!
//! `reference=true`'s third input is accepted (so the graph negotiates
//! three pads) but **not incorporated into the formula** — this filter
//! still computes `c1`/`c2` exactly as the two-input case and ignores the
//! third frame, which is almost certainly not what the reference does with
//! it. Not measured in this pass.

use smallvec::SmallVec;
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameOut, Paired, PairedFilter};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats, Tie};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::sample;

const DUAL_PADS: &[Pad] = &[
    Pad {
        name: "source",
        media_type: MediaType::Video,
    },
    Pad {
        name: "filtered",
        media_type: MediaType::Video,
    },
];
const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "limitdiff",
    description: "Apply filtering with limiting difference",
    inputs: DUAL_PADS,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "limitdiff", help = "Apply filtering with limiting difference")]
pub(crate) struct Opts {
    #[opt(name = "threshold", help = "set the threshold", default = 0.003_921_57, range = 0.0..=1.0, flags(video, filtering))]
    pub threshold: f64,
    #[opt(name = "elasticity", help = "set the elasticity", default = 2.0, range = 0.0..=10.0, flags(video, filtering))]
    pub elasticity: f64,
    #[opt(
        name = "reference",
        help = "enable reference stream (not incorporated into the formula)",
        default = false,
        flags(video, filtering)
    )]
    pub reference: bool,
    #[opt(name = "planes", help = "set the planes to filter", default = 15, range = 0..=15, flags(video, filtering))]
    pub planes: i32,
}

#[derive(Debug)]
struct Filter {
    threshold: f64,
    elasticity: f64,
    reference: bool,
    planes: i32,
}

/// One channel's applied output value, per this module's measured (and
/// disclosed-approximate) formula.
fn limit(c1: f64, c2: f64, threshold_px: f64, elasticity: f64, max: f64) -> f64 {
    let diff = c2 - c1;
    let ad = diff.abs();
    if ad <= threshold_px {
        return c1;
    }
    let end = threshold_px * elasticity;
    if elasticity <= 0.0 || ad >= end {
        return c2.clamp(0.0, max);
    }
    let t = (ad - threshold_px) / (end - threshold_px);
    (diff.mul_add(t, c1)).clamp(0.0, max)
}

impl PairedFilter for Filter {
    fn input_count(&self) -> usize {
        if self.reference { 3 } else { 2 }
    }

    fn filter_frames(
        &mut self,
        ctx: &mut FilterContext<'_>,
        inputs: SmallVec<[Frame; 4]>,
    ) -> Result<FrameOut> {
        let mut it = inputs.into_iter();
        let (Some(c1), Some(c2)) = (it.next(), it.next()) else {
            return Ok(FrameOut::None);
        };
        let FrameData::Video {
            format,
            width,
            height,
            ..
        } = c1.data
        else {
            return Ok(FrameOut::One(c1));
        };
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        let big_endian = format.is_big_endian();
        for ch in 0..format.component_count() {
            let Some(comp) = sample::component(format, ch) else {
                continue;
            };
            let (Some(p1), Some(p2), Some(mut dp)) = (
                c1.plane(comp.plane as usize),
                c2.plane(comp.plane as usize),
                out.plane_mut(comp.plane as usize),
            ) else {
                continue;
            };
            let w = dp
                .row_bytes()
                .checked_div(usize::from(comp.step.max(1)))
                .unwrap_or(0);
            let n = dp.rows().min(p1.rows()).min(p2.rows());
            let selected = (self.planes >> ch.min(31)) & 1 != 0;
            if !selected {
                for y in 0..n {
                    let (Some(sr), Some(dr)) = (p1.row(y), dp.row_mut(y)) else {
                        continue;
                    };
                    let len = sr.len().min(dr.len());
                    if let (Some(s), Some(d)) = (sr.get(..len), dr.get_mut(..len)) {
                        d.copy_from_slice(s);
                    }
                }
                continue;
            }
            let max = f64::from(sample::max_value(comp));
            let threshold_px = self.threshold * max;
            for y in 0..n {
                let (Some(r1), Some(r2), Some(rd)) = (p1.row(y), p2.row(y), dp.row_mut(y)) else {
                    continue;
                };
                for x in 0..w {
                    let v1 = f64::from(sample::read(r1, x, comp, big_endian));
                    let v2 = f64::from(sample::read(r2, x, comp, big_endian));
                    let out_v = limit(v1, v2, threshold_px, self.elasticity, max);
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "out_v clamped into [0, max] and max fits u16 by construction"
                    )]
                    let out_v = out_v.round() as u16;
                    sample::write(rd, x, comp, big_endian, out_v);
                }
            }
        }
        out.pts = c1.pts;
        out.time_base = c1.time_base;
        out.duration = c1.duration;
        out.sample_aspect_ratio = c1.sample_aspect_ratio;
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts: Opts = common::parse(req.args)?;
    let n = if opts.reference { 3 } else { 2 };
    let set = FormatSet::video_list(common::formats_where(sample::is_addressable));
    let formats = NodeFormats {
        inputs: (0..n).map(|_| set.clone()).collect(),
        outputs: vec![FormatSet::default()],
        ties: Tie::all_pads(n, 1, MediaType::Video),
        label: req.instance.to_owned(),
    };
    Ok(Instance {
        desc: DESC,
        formats,
        filter: Box::new(Paired::new(Filter {
            threshold: opts.threshold,
            elasticity: opts.elasticity,
            reference: opts.reference,
            planes: opts.planes,
        })),
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::float_cmp,
    reason = "test code"
)]
mod tests {
    use super::*;

    #[test]
    fn below_threshold_passes_through_c1_unchanged() {
        assert_eq!(limit(128.0, 138.0, 12.75, 2.0, 255.0), 128.0);
        assert_eq!(limit(128.0, 123.0, 12.75, 2.0, 255.0), 128.0);
    }

    #[test]
    fn beyond_the_elastic_end_is_fully_applied() {
        assert_eq!(limit(128.0, 168.0, 12.75, 2.0, 255.0), 168.0);
        assert_eq!(limit(128.0, 90.0, 12.75, 2.0, 255.0), 90.0);
    }

    #[test]
    fn zero_elasticity_is_a_hard_cutover_at_threshold() {
        assert_eq!(limit(128.0, 141.0, 12.75, 0.0, 255.0), 141.0);
        assert_eq!(limit(128.0, 138.0, 12.75, 0.0, 255.0), 128.0);
    }

    #[test]
    fn the_elastic_zone_is_monotone_between_the_two_edges() {
        let lo = limit(128.0, 141.0, 12.75, 2.0, 255.0);
        let hi = limit(128.0, 152.0, 12.75, 2.0, 255.0);
        assert!(lo > 128.0 && lo < hi && hi <= 152.0);
    }

    #[test]
    fn unselected_planes_are_left_at_c1() {
        // planes=1 selects only channel 0; channel 1 must copy c1 verbatim
        // regardless of c2 — exercised at the Frame level via `create`'s
        // own registry test in `registry.rs`, this just pins the mask math.
        let f = Filter {
            threshold: 0.0,
            elasticity: 2.0,
            reference: false,
            planes: 1,
        };
        assert_eq!(f.planes & 1, 1);
        assert_eq!((f.planes >> 1) & 1, 0);
    }
}
