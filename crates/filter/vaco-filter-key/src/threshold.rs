//! `threshold` — pick `min` or `max` per sample, based on comparing the
//! source stream against a per-pixel threshold stream.
//!
//! `ffmpeg -h filter=threshold` documents only `planes` (bitmask, default
//! 15); no framesync surface, so (same reasoning as
//! [`crate::masked_pick`]) this is a lockstep four-input filter through
//! [`vaco_filter_core::adapt::Paired`].
//!
//! # Measured: the formula
//!
//! Three probes on `gray` inputs (`source`, `threshold`, `min`, `max`):
//!
//! ```text
//! out = max if source >  threshold else min
//! ```
//!
//! confirmed with `source` above the threshold (picks `max`), below it
//! (picks `min`), and exactly equal (`source == threshold` also picks
//! `min` — the comparison is strict `>`, not `>=`). Exact: integer
//! comparison and selection, no interpolation.

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
        name: "default",
        media_type: MediaType::Video,
    },
    Pad {
        name: "threshold",
        media_type: MediaType::Video,
    },
    Pad {
        name: "min",
        media_type: MediaType::Video,
    },
    Pad {
        name: "max",
        media_type: MediaType::Video,
    },
];
const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "threshold",
    description: "Threshold first video stream using other video streams",
    inputs: PADS,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(
    name = "threshold",
    help = "Threshold first video stream using other video streams"
)]
pub(crate) struct Opts {
    #[opt(name = "planes", help = "set planes to filter", default = 15, range = 0..=15, flags(video, filtering))]
    pub planes: i32,
}

#[derive(Debug)]
struct Filter {
    planes: i64,
}

impl PairedFilter for Filter {
    fn input_count(&self) -> usize {
        4
    }

    fn filter_frames(
        &mut self,
        ctx: &mut FilterContext<'_>,
        inputs: SmallVec<[Frame; 4]>,
    ) -> Result<FrameOut> {
        let mut it = inputs.into_iter();
        let (Some(source), Some(thr), Some(min), Some(max)) =
            (it.next(), it.next(), it.next(), it.next())
        else {
            return Ok(FrameOut::None);
        };
        let FrameData::Video {
            format,
            width,
            height,
            ..
        } = source.data
        else {
            return Ok(FrameOut::One(source));
        };
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        let big_endian = format.is_big_endian();
        for ch in 0..format.component_count() {
            let Some(comp) = sample::component(format, ch) else {
                continue;
            };
            let (Some(sp), Some(tp), Some(minp), Some(maxp), Some(mut dp)) = (
                source.plane(comp.plane as usize),
                thr.plane(comp.plane as usize),
                min.plane(comp.plane as usize),
                max.plane(comp.plane as usize),
                out.plane_mut(comp.plane as usize),
            ) else {
                continue;
            };
            let w = dp
                .row_bytes()
                .checked_div(usize::from(comp.step.max(1)))
                .unwrap_or(0);
            let n = dp
                .rows()
                .min(sp.rows())
                .min(tp.rows())
                .min(minp.rows())
                .min(maxp.rows());
            if !sample::plane_selected(self.planes, ch) {
                for y in 0..n {
                    let (Some(sr), Some(dr)) = (sp.row(y), dp.row_mut(y)) else {
                        continue;
                    };
                    let len = sr.len().min(dr.len());
                    if let (Some(s), Some(d)) = (sr.get(..len), dr.get_mut(..len)) {
                        d.copy_from_slice(s);
                    }
                }
                continue;
            }
            for y in 0..n {
                let (Some(sr), Some(tr), Some(minr), Some(maxr), Some(dr)) = (
                    sp.row(y),
                    tp.row(y),
                    minp.row(y),
                    maxp.row(y),
                    dp.row_mut(y),
                ) else {
                    continue;
                };
                for x in 0..w {
                    let sv = sample::read(sr, x, comp, big_endian);
                    let tv = sample::read(tr, x, comp, big_endian);
                    let minv = sample::read(minr, x, comp, big_endian);
                    let maxv = sample::read(maxr, x, comp, big_endian);
                    let out_v = if sv > tv { maxv } else { minv };
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
    let set = FormatSet::video_list(common::formats_where(sample::is_addressable));
    let formats = NodeFormats {
        inputs: vec![set.clone(), set.clone(), set.clone(), set],
        outputs: vec![FormatSet::default()],
        ties: Tie::all_pads(4, 1, MediaType::Video),
        label: req.instance.to_owned(),
    };
    Ok(Instance {
        desc: DESC,
        formats,
        filter: Box::new(Paired::new(Filter {
            planes: i64::from(opts.planes),
        })),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn hand_computed_pick_on_measured_cases() {
        let cases: &[(i32, i32, i32, i32, i32)] = &[
            (100, 80, 0, 255, 255),
            (100, 112, 0, 255, 0),
            (100, 100, 0, 255, 0),
        ];
        for &(source, threshold, min, max, expected) in cases {
            let out = if source > threshold { max } else { min };
            assert_eq!(out, expected, "source={source} threshold={threshold}");
        }
    }
}
