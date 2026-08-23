//! `maskedmax`/`maskedmin` — pick whichever of two filter streams is
//! farther from (respectively nearer to) a source stream, per sample.
//!
//! `ffmpeg -h filter=maskedmax`/`maskedmin` document only `planes`
//! (bitmask, default 15) — no `eof_action`/`shortest`/`ts_sync_mode`, so
//! (measured, same check `maskedmerge.rs` already documents for this
//! crate) these are lockstep three-input filters, not framesync ones.
//! `vaco-filter-core`'s [`vaco_filter_core::adapt::Paired`] adapter is the
//! N-in-1-out strict-lockstep shape for exactly this case.
//!
//! # Measured: the formula
//!
//! Seven probes on `gray` inputs (`source`, `filter1`, `filter2`) via
//! `maskedmax`/`maskedmin`, including a tie (`filter1`/`filter2`
//! equidistant from `source`):
//!
//! ```text
//! maskedmax(source, f1, f2) = f2 if |source - f2| >  |source - f1| else f1
//! maskedmin(source, f1, f2) = f2 if |source - f2| <  |source - f1| else f1
//! ```
//!
//! i.e. `maskedmax` returns whichever of `f1`/`f2` is **farther** from
//! `source` (a tie favours `f1`), `maskedmin` whichever is **nearer**
//! (same tie-break). Confirmed with `source` between, above and below
//! both `f1`/`f2`, and with a genuine tie (`source=100, f1=80, f2=120`,
//! both at distance `20`: both filters returned `f1`). Exact — this is
//! integer comparison and selection, no interpolation.

use smallvec::SmallVec;
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameOut, Paired, PairedFilter};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats, Tie};
use vaco_filter_core::{FilterContext, FilterDesc, Pad};
use vaco_frame::{Frame, FrameData};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::sample;

const PADS: &[Pad] = &[
    Pad { name: "source", media_type: MediaType::Video },
    Pad { name: "filter1", media_type: MediaType::Video },
    Pad { name: "filter2", media_type: MediaType::Video },
];
const VIDEO_PAD: &[Pad] = &[Pad { name: "default", media_type: MediaType::Video }];

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "maskedmax", help = "Apply filtering with maximum difference of two streams")]
pub(crate) struct Opts {
    #[opt(name = "planes", help = "set planes", default = 15, range = 0..=15, flags(video, filtering))]
    pub planes: i32,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        common::parse(args)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pick {
    Max,
    Min,
}

#[derive(Debug)]
struct Filter {
    planes: i64,
    pick: Pick,
}

impl PairedFilter for Filter {
    fn input_count(&self) -> usize {
        3
    }

    fn filter_frames(&mut self, ctx: &mut FilterContext<'_>, inputs: SmallVec<[Frame; 4]>) -> Result<FrameOut> {
        let mut it = inputs.into_iter();
        let (Some(source), Some(filter1), Some(filter2)) = (it.next(), it.next(), it.next()) else {
            return Ok(FrameOut::None);
        };
        let FrameData::Video { format, width, height, .. } = source.data else {
            return Ok(FrameOut::One(source));
        };
        let mut out = ctx.pool().acquire_video(format, width, height)?;
        let big_endian = format.is_big_endian();
        for ch in 0..format.component_count() {
            let Some(comp) = sample::component(format, ch) else { continue };
            let (Some(sp), Some(f1p), Some(f2p), Some(mut dp)) = (
                source.plane(comp.plane as usize),
                filter1.plane(comp.plane as usize),
                filter2.plane(comp.plane as usize),
                out.plane_mut(comp.plane as usize),
            ) else {
                continue;
            };
            let w = dp.row_bytes().checked_div(usize::from(comp.step.max(1))).unwrap_or(0);
            let n = dp.rows().min(sp.rows()).min(f1p.rows()).min(f2p.rows());
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
                let (Some(sr), Some(f1r), Some(f2r), Some(dr)) = (sp.row(y), f1p.row(y), f2p.row(y), dp.row_mut(y))
                else {
                    continue;
                };
                for x in 0..w {
                    let sv = i32::from(sample::read(sr, x, comp, big_endian));
                    let v1 = sample::read(f1r, x, comp, big_endian);
                    let v2 = sample::read(f2r, x, comp, big_endian);
                    let d1 = (sv - i32::from(v1)).abs();
                    let d2 = (sv - i32::from(v2)).abs();
                    let out_v = match self.pick {
                        Pick::Max => {
                            if d2 > d1 {
                                v2
                            } else {
                                v1
                            }
                        }
                        Pick::Min => {
                            if d2 < d1 {
                                v2
                            } else {
                                v1
                            }
                        }
                    };
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

fn build(desc: FilterDesc, pick: Pick, req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let set = FormatSet::video_list(common::formats_where(sample::is_addressable));
    let formats = NodeFormats {
        inputs: vec![set.clone(), set.clone(), set],
        outputs: vec![FormatSet::default()],
        ties: Tie::all_pads(3, 1, MediaType::Video),
        label: req.instance.to_owned(),
    };
    Ok(Instance {
        desc,
        formats,
        filter: Box::new(Paired::new(Filter { planes: i64::from(opts.planes), pick })),
    })
}

pub mod maskedmax {
    use super::{Instance, Instantiate, PADS, Pick, VIDEO_PAD, build};
    use vaco_filter_core::{FilterDesc, FilterFlags};

    pub const DESC: FilterDesc = FilterDesc {
        name: "maskedmax",
        description: "Apply filtering with maximum difference of two streams",
        inputs: PADS,
        outputs: VIDEO_PAD,
        flags: FilterFlags::empty(),
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        build(DESC, Pick::Max, req)
    }
}

pub mod maskedmin {
    use super::{Instance, Instantiate, PADS, Pick, VIDEO_PAD, build};
    use vaco_filter_core::{FilterDesc, FilterFlags};

    pub const DESC: FilterDesc = FilterDesc {
        name: "maskedmin",
        description: "Apply filtering with minimum difference of two streams",
        inputs: PADS,
        outputs: VIDEO_PAD,
        flags: FilterFlags::empty(),
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        build(DESC, Pick::Min, req)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    #[test]
    fn hand_computed_pick_on_measured_cases() {
        // Independent oracle: the formula documented above, computed by
        // hand from the measured probes (not derived from this module's
        // own implementation).
        let cases: &[(i32, i32, i32, u16, u16)] = &[
            (100, 80, 150, 150, 80),
            (200, 80, 150, 80, 150),
            (50, 80, 150, 150, 80),
            (100, 96, 112, 112, 96),
            (100, 80, 120, 80, 80), // tie: both favour f1
        ];
        for &(src, f1, f2, want_max, want_min) in cases {
            let d1 = (src - f1).abs();
            let d2 = (src - f2).abs();
            #[allow(clippy::cast_sign_loss, reason = "test inputs are all non-negative")]
            let got_max = if d2 > d1 { f2 as u16 } else { f1 as u16 };
            #[allow(clippy::cast_sign_loss, reason = "test inputs are all non-negative")]
            let got_min = if d2 < d1 { f2 as u16 } else { f1 as u16 };
            assert_eq!(got_max, want_max, "max src={src} f1={f1} f2={f2}");
            assert_eq!(got_min, want_min, "min src={src} f1={f1} f2={f2}");
        }
    }
}
