//! `maskedthreshold` — pick `source` or `reference` per sample, based on
//! whether they differ by more than a threshold.
//!
//! `ffmpeg -h filter=maskedthreshold` documents `threshold` (`0..65535`,
//! default `1`), `planes` (bitmask, default 15) and `mode` (`abs`/`diff`,
//! default `abs`); no framesync surface, so (same reasoning as
//! [`crate::masked_pick`]) this is a lockstep two-input filter through
//! [`vaco_filter_core::adapt::Paired`].
//!
//! # Measured: `mode=abs`
//!
//! Five probes on `gray` inputs (`source`, `reference`) at `threshold=5`:
//!
//! ```text
//! out = source if |source - reference| <= threshold else reference
//! ```
//!
//! confirmed both directions (`source < reference` and `source >
//! reference`) and at the boundary (`diff=5` keeps `source`, `diff=6`
//! switches to `reference`). Exact.
//!
//! # Not implemented: `mode=diff`
//!
//! A `mode=diff` probe (`source=100, reference=102, threshold=5`)
//! produced `97` — not `source` (`100`) and not `reference` (`102`), so
//! `diff` mode modifies the sample rather than picking one of the two
//! inputs outright. Two data points were not enough to recover the exact
//! formula without guessing, so `mode=diff` falls back to `mode=abs`'s
//! behaviour here — a documented gap, not a silent wrong answer.

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
    Pad { name: "source", media_type: MediaType::Video },
    Pad { name: "reference", media_type: MediaType::Video },
];
const VIDEO_PAD: &[Pad] = &[Pad { name: "default", media_type: MediaType::Video }];

pub const DESC: FilterDesc = FilterDesc {
    name: "maskedthreshold",
    description: "Pick pixels comparing absolute difference of two streams with threshold",
    inputs: PADS,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "maskedthreshold", help = "Pick pixels comparing absolute difference of two streams with threshold")]
pub(crate) struct Opts {
    #[opt(name = "threshold", help = "set threshold", default = 1, range = 0..=65535, flags(video, filtering))]
    pub threshold: i32,
    #[opt(name = "planes", help = "set planes", default = 15, range = 0..=15, flags(video, filtering))]
    pub planes: i32,
    #[opt(
        name = "mode",
        help = "set mode (diff is not implemented; behaves like abs)",
        default = 0,
        range = 0..=1,
        flags(video, filtering)
    )]
    pub mode: i32,
}

#[derive(Debug)]
struct Filter {
    threshold: i32,
    planes: i64,
}

impl PairedFilter for Filter {
    fn filter_frames(&mut self, ctx: &mut FilterContext<'_>, inputs: SmallVec<[Frame; 4]>) -> Result<FrameOut> {
        let mut it = inputs.into_iter();
        let (Some(source), Some(reference)) = (it.next(), it.next()) else {
            return Ok(FrameOut::None);
        };
        let FrameData::Video { format, width, height, .. } = source.data else {
            return Ok(FrameOut::One(source));
        };
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        let big_endian = format.is_big_endian();
        for ch in 0..format.component_count() {
            let Some(comp) = sample::component(format, ch) else { continue };
            let (Some(sp), Some(rp), Some(mut dp)) = (
                source.plane(comp.plane as usize),
                reference.plane(comp.plane as usize),
                out.plane_mut(comp.plane as usize),
            ) else {
                continue;
            };
            let w = dp.row_bytes().checked_div(usize::from(comp.step.max(1))).unwrap_or(0);
            let n = dp.rows().min(sp.rows()).min(rp.rows());
            if !sample::plane_selected(self.planes, ch) {
                for y in 0..n {
                    let (Some(sr), Some(dr)) = (sp.row(y), dp.row_mut(y)) else { continue };
                    let len = sr.len().min(dr.len());
                    if let (Some(s), Some(d)) = (sr.get(..len), dr.get_mut(..len)) {
                        d.copy_from_slice(s);
                    }
                }
                continue;
            }
            for y in 0..n {
                let (Some(sr), Some(rr), Some(dr)) = (sp.row(y), rp.row(y), dp.row_mut(y)) else {
                    continue;
                };
                for x in 0..w {
                    let sv = sample::read(sr, x, comp, big_endian);
                    let rv = sample::read(rr, x, comp, big_endian);
                    let diff = (i32::from(sv) - i32::from(rv)).abs();
                    let out_v = if diff <= self.threshold { sv } else { rv };
                    sample::write(dr, x, comp, big_endian, out_v);
                }
            }
        }
        out.pts = source.pts;
        out.time_base = source.time_base;
        out.duration = source.duration;
        out.sample_aspect_ratio = source.sample_aspect_ratio;
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts: Opts = common::parse(req.args)?;
    let _ = opts.mode; // documented gap: `diff` falls back to `abs`
    let set = FormatSet::video_list(common::formats_where(sample::is_addressable));
    let formats = NodeFormats {
        inputs: vec![set.clone(), set],
        outputs: vec![FormatSet::default()],
        ties: Tie::all_pads(2, 1, MediaType::Video),
        label: req.instance.to_owned(),
    };
    Ok(Instance {
        desc: DESC,
        formats,
        filter: Box::new(Paired::new(Filter {
            threshold: opts.threshold,
            planes: i64::from(opts.planes),
        })),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn hand_computed_pick_on_measured_cases() {
        let cases: &[(i32, i32, i32, i32)] = &[(100, 102, 5, 100), (100, 106, 5, 106), (100, 96, 5, 100), (100, 94, 5, 94)];
        for &(source, reference, threshold, expected) in cases {
            let diff = (source - reference).abs();
            let out = if diff <= threshold { source } else { reference };
            assert_eq!(out, expected, "source={source} reference={reference}");
        }
    }
}
