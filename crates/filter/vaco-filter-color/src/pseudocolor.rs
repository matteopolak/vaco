//! `pseudocolor` — recolour a frame from one selected component's value.
//!
//! `ffmpeg -h filter=pseudocolor` documents `c0`..`c3` (expression, default
//! `"val"`), `index`/`i` (which input component supplies `val`, default 0),
//! `preset`/`p` (21 named colour ramps, default none) and `opacity`
//! (0-1, default 1).
//!
//! # Measured: `c0`..`c3` all see the *same* value, unlike `lut`
//!
//! ```text
//! ffmpeg -f lavfi -i "color=gray,format=yuv420p" -vf pseudocolor=c0=200:c1=50:c2=90 -f rawvideo -
//! # -> every pixel is exactly (200, 50, 90): literal constants, output
//! #    unchanged format (still yuv420p, no forced RGB conversion).
//! ```
//!
//! So this is [`crate::lut`]'s structure with one difference: `lut` binds
//! each output channel to *its own* input sample; `pseudocolor` binds every
//! output channel to the *same* input sample — the `index`-selected
//! channel's value — which is what makes it a false-colour remap rather
//! than a per-channel curve.
//!
//! # Not implemented: named `preset=`s
//!
//! The 21 presets (`magma`, `inferno`, `turbo`, ...) are each a fixed
//! colour ramp the reference does not describe in `-h` output, and
//! reproducing 21 gradients exactly needs either the reference's source
//! (D7 forbids that) or a per-preset, per-sample-value probing pass this
//! crate's time budget did not allow. `preset`/`p` are parsed and
//! validated (rejecting an out-of-range value) but have no effect —
//! matching [`crate::colorchannelmixer`]'s `pc`/`pa` precedent for a
//! parsed-but-inert option.
//!
//! # `opacity`
//!
//! Not documented beyond its name and range. Implemented as the natural
//! reading: `final = original*(1-opacity) + pseudocolored*opacity`, so
//! `opacity=0` is an exact no-op and `opacity=1` (the default) is the full
//! effect — both ends are independently verifiable without trusting this
//! crate's own arithmetic in between.

use vaco_core::{MediaType, Result};
use vaco_expr::{Bindings, Expr};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::sample;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "pseudocolor",
    description: "Make pseudocolored video frames",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::TIMELINE_GENERIC,
};

const VARS: &[&str] = &[
    "val", "ymin", "ymax", "umin", "umax", "vmin", "vmax", "w", "h",
];

/// `ffmpeg -h filter=pseudocolor`'s own named constants for `preset`/`p`
/// (one field, two names via `alias`). Not implemented (see the field's
/// own doc) -- only the name needs to parse for `option_consts_gate.rs`.
const PRESET_CONSTS: &[vaco_opts::ConstDesc] = &[
    vaco_opts::ConstDesc {
        name: "none",
        help: "",
        unit: "preset",
        value: vaco_opts::ConstValue::Int(-1),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "magma",
        help: "",
        unit: "preset",
        value: vaco_opts::ConstValue::Int(0),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "inferno",
        help: "",
        unit: "preset",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "plasma",
        help: "",
        unit: "preset",
        value: vaco_opts::ConstValue::Int(2),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "viridis",
        help: "",
        unit: "preset",
        value: vaco_opts::ConstValue::Int(3),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "turbo",
        help: "",
        unit: "preset",
        value: vaco_opts::ConstValue::Int(4),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "cividis",
        help: "",
        unit: "preset",
        value: vaco_opts::ConstValue::Int(5),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "range1",
        help: "",
        unit: "preset",
        value: vaco_opts::ConstValue::Int(6),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "range2",
        help: "",
        unit: "preset",
        value: vaco_opts::ConstValue::Int(7),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "shadows",
        help: "",
        unit: "preset",
        value: vaco_opts::ConstValue::Int(8),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "highlights",
        help: "",
        unit: "preset",
        value: vaco_opts::ConstValue::Int(9),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "solar",
        help: "",
        unit: "preset",
        value: vaco_opts::ConstValue::Int(10),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "nominal",
        help: "",
        unit: "preset",
        value: vaco_opts::ConstValue::Int(11),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "preferred",
        help: "",
        unit: "preset",
        value: vaco_opts::ConstValue::Int(12),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "total",
        help: "",
        unit: "preset",
        value: vaco_opts::ConstValue::Int(13),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "spectral",
        help: "",
        unit: "preset",
        value: vaco_opts::ConstValue::Int(14),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "cool",
        help: "",
        unit: "preset",
        value: vaco_opts::ConstValue::Int(15),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "heat",
        help: "",
        unit: "preset",
        value: vaco_opts::ConstValue::Int(16),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "fiery",
        help: "",
        unit: "preset",
        value: vaco_opts::ConstValue::Int(17),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "blues",
        help: "",
        unit: "preset",
        value: vaco_opts::ConstValue::Int(18),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "green",
        help: "",
        unit: "preset",
        value: vaco_opts::ConstValue::Int(19),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "helix",
        help: "",
        unit: "preset",
        value: vaco_opts::ConstValue::Int(20),
        flags: vaco_opts::OptFlags::NONE,
    },
];

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "pseudocolor", help = "Make pseudocolored video frames")]
pub(crate) struct Opts {
    #[opt(name = "c0", help = "set component #0 expression", default = "val".to_owned(), flags(video, filtering))]
    pub c0: String,
    #[opt(name = "c1", help = "set component #1 expression", default = "val".to_owned(), flags(video, filtering))]
    pub c1: String,
    #[opt(name = "c2", help = "set component #2 expression", default = "val".to_owned(), flags(video, filtering))]
    pub c2: String,
    #[opt(name = "c3", help = "set component #3 expression", default = "val".to_owned(), flags(video, filtering))]
    pub c3: String,
    #[opt(name = "index", alias = "i", help = "set component as base", default = 0, range = 0..=3, flags(video, filtering))]
    pub index: i32,
    #[opt(name = "preset", alias = "p", help = "set preset (not implemented; parsed only)", unit = "preset", consts = PRESET_CONSTS, default = -1, range = -1..=20, flags(video, filtering))]
    pub preset: i32,
    #[opt(name = "opacity", help = "set pseudocolor opacity", default = 1.0, range = 0.0..=1.0, flags(video, filtering))]
    pub opacity: f64,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        if o.preset != -1 {
            return Err("pseudocolor: `preset` is parsed but not applied by this crate; refusing rather than silently ignoring it".to_string());
        }
        Ok(o)
    }
}

#[derive(Debug)]
pub(crate) struct Filter {
    exprs: [Expr; 4],
    index: usize,
    opacity: f64,
    tables: Vec<Option<Vec<u16>>>,
}

impl Filter {
    fn new(o: &Opts) -> std::result::Result<Self, String> {
        let bindings = Bindings::new(VARS);
        let parse = |s: &str| {
            Expr::parse(s, &bindings).map_err(|e| format!("pseudocolor: bad expression `{s}`: {e}"))
        };
        Ok(Self {
            exprs: [parse(&o.c0)?, parse(&o.c1)?, parse(&o.c2)?, parse(&o.c3)?],
            index: o.index as usize,
            opacity: o.opacity,
            tables: Vec::new(),
        })
    }

    fn rebuild_tables(&mut self, format: vaco_pixfmt::PixFmt, width: u32, height: u32) {
        let Some(base_comp) = sample::component(format, self.index) else {
            self.tables = Vec::new();
            return;
        };
        let base_max = f64::from(sample::max_value(base_comp));
        let n = format.component_count().min(4);
        self.tables = (0..n)
            .map(|ch| {
                let comp = sample::component(format, ch)?;
                let out_max = f64::from(sample::max_value(comp));
                let expr = self.exprs.get(ch)?;
                let table = (0..=(base_max as usize))
                    .map(|val| {
                        let v = val as f64;
                        let out = expr.eval(&[
                            v,
                            0.0,
                            base_max,
                            0.0,
                            base_max,
                            0.0,
                            base_max,
                            f64::from(width),
                            f64::from(height),
                        ]);
                        #[allow(
                            clippy::cast_possible_truncation,
                            clippy::cast_sign_loss,
                            reason = "clamped to [0, out_max] and out_max fits in u16 by construction"
                        )]
                        let out_v = out.clamp(0.0, out_max).round() as u16;
                        out_v
                    })
                    .collect();
                Some(table)
            })
            .collect();
    }

    fn apply_frame(&self, input: &mut Frame) {
        let FrameData::Video { format, .. } = input.data else {
            return;
        };
        if !sample::is_addressable(format) {
            return;
        }
        let Some(base_comp) = sample::component(format, self.index) else {
            return;
        };
        let big_endian = format.is_big_endian();
        let Some(base_plane) = input.plane(base_comp.plane as usize) else {
            return;
        };
        let base_w = base_plane
            .row_bytes()
            .checked_div(usize::from(base_comp.step.max(1)))
            .unwrap_or(0);
        let base_rows: Vec<Vec<u16>> = (0..base_plane.rows())
            .map(|y| {
                base_plane
                    .row(y)
                    .map(|r| {
                        (0..base_w)
                            .map(|x| sample::read(r, x, base_comp, big_endian))
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .collect();
        for (ch, table) in self.tables.iter().enumerate() {
            let Some(table) = table else { continue };
            let Some(comp) = sample::component(format, ch) else {
                continue;
            };
            let Some(mut plane) = input.plane_mut(comp.plane as usize) else {
                continue;
            };
            let max = f64::from(sample::max_value(comp));
            let w = plane
                .row_bytes()
                .checked_div(usize::from(comp.step.max(1)))
                .unwrap_or(0);
            for y in 0..plane.rows() {
                let Some(base_row) = base_rows.get(y) else {
                    continue;
                };
                let Some(row) = plane.row_mut(y) else {
                    continue;
                };
                for x in 0..w {
                    let base_v = base_row.get(x).copied().unwrap_or(0);
                    let base_idx = base_v as usize;
                    let out = table.get(base_idx).copied().unwrap_or(base_v);
                    let orig = sample::read(row, x, comp, big_endian);
                    let blended =
                        f64::from(orig) * (1.0 - self.opacity) + f64::from(out) * self.opacity;
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "clamped to [0, max] and max fits in u16 by construction"
                    )]
                    let blended = blended.clamp(0.0, max).round() as u16;
                    if blended != orig {
                        sample::write(row, x, comp, big_endian, blended);
                    }
                }
            }
        }
    }
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Video {
            format,
            width,
            height,
            ..
        }) = ctx.input_link(0).cloned()
        {
            self.rebuild_tables(format, width, height);
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, mut input: Frame) -> Result<FrameOut> {
        input.make_writable();
        self.apply_frame(&mut input);
        Ok(FrameOut::One(input))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter::new(&opts)?;
    let set = FormatSet::video_list(common::formats_where(sample::is_addressable));
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::uniform(1, 1, MediaType::Video, &set, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_pixfmt::PixFmt;

    fn opts(c0: &str, c1: &str, c2: &str) -> Opts {
        Opts {
            c0: c0.to_owned(),
            c1: c1.to_owned(),
            c2: c2.to_owned(),
            c3: "val".to_owned(),
            index: 0,
            preset: -1,
            opacity: 1.0,
        }
    }

    #[test]
    fn constants_flatten_every_output_channel() {
        let mut f = Filter::new(&opts("200", "50", "90")).unwrap();
        f.rebuild_tables(PixFmt::Yuv420p, 2, 1);
        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Yuv420p, 2, 1).unwrap();
        frame.plane_mut(0).unwrap().fill(100);
        f.apply_frame(&mut frame);
        assert_eq!(frame.plane(0).unwrap().row(0).unwrap()[0], 200);
        assert_eq!(frame.plane(1).unwrap().row(0).unwrap()[0], 50);
        assert_eq!(frame.plane(2).unwrap().row(0).unwrap()[0], 90);
    }

    #[test]
    fn identity_expressions_flatten_every_channel_to_the_index_channels_value() {
        // `c0=c1=c2="val"` does NOT mean "leave each channel alone" —
        // pseudocolor's whole point is that every output channel sees the
        // *same* index-selected input value (measured in this module's
        // doc: `c0=200:c1=50:c2=90` painted every channel a literal
        // constant, regardless of the original per-channel data). With
        // `index=0` (the default) and identity expressions, every channel
        // must come out equal to the *original Y value*, not to its own
        // original value.
        let mut f = Filter::new(&opts("val", "val", "val")).unwrap();
        f.rebuild_tables(PixFmt::Yuv420p, 2, 1);
        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Yuv420p, 2, 1).unwrap();
        frame.plane_mut(0).unwrap().fill(77);
        frame.plane_mut(1).unwrap().fill(88);
        frame.plane_mut(2).unwrap().fill(99);
        f.apply_frame(&mut frame);
        assert_eq!(frame.plane(0).unwrap().row(0).unwrap()[0], 77);
        assert_eq!(frame.plane(1).unwrap().row(0).unwrap()[0], 77);
        assert_eq!(frame.plane(2).unwrap().row(0).unwrap()[0], 77);
    }

    #[test]
    fn zero_opacity_is_an_exact_no_op() {
        let mut f = Filter::new(&opts("200", "50", "90")).unwrap();
        f.opacity = 0.0;
        f.rebuild_tables(PixFmt::Yuv420p, 2, 1);
        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Yuv420p, 2, 1).unwrap();
        frame.plane_mut(0).unwrap().fill(11);
        frame.plane_mut(1).unwrap().fill(22);
        frame.plane_mut(2).unwrap().fill(33);
        f.apply_frame(&mut frame);
        assert_eq!(frame.plane(0).unwrap().row(0).unwrap()[0], 11);
        assert_eq!(frame.plane(1).unwrap().row(0).unwrap()[0], 22);
        assert_eq!(frame.plane(2).unwrap().row(0).unwrap()[0], 33);
    }
}
