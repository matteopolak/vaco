//! `lut`/`lutrgb`/`lutyuv` — apply a per-component lookup table.
//!
//! `ffmpeg -h filter=lut` (and `lutrgb`, `lutyuv`) documents `c0`..`c3`
//! (expression, default `"clipval"`) plus the aliases `y`/`u`/`v`/`r`/`g`/
//! `b`/`a` for `c0`/`c1`/`c2`/`c0`/`c1`/`c2`/`c3` respectively — the
//! reference lists all of them under one shared `lut/lutyuv/lutrgb
//! AVOptions` heading, and measurement confirms they really are one
//! implementation under three names, not three option surfaces:
//!
//! ```text
//! ffmpeg -f lavfi -i "color=red,format=yuv420p" -vf lutrgb=r=128 -f null -
//! # -> stays yuv420p; lutrgb does NOT force an RGB conversion.
//! ```
//!
//! `lutrgb=r=128` on `yuv420p` and `lutyuv=y=128` on `rgb24` both leave the
//! pixel format untouched — the "r"/"y" names are cosmetic aliases for
//! channel 0, applied positionally exactly like [`crate::colorchannelmixer`].
//! So all three registered names share one `Filter`/`Opts` here, differing
//! only in the alias table on `c0`/`c1`/`c2` (`c3`/`a` is shared by all
//! three, since alpha has no YUV/RGB-specific name).
//!
//! Probed by giving each candidate name to `c0` and checking whether the
//! reference errors "Invalid argument" (unbound) or accepts it: `val`,
//! `clipval`, `maxval`, `minval`, `negval`, `w`, `h` are all bound; `n`
//! (frame number) is not — confirmed separately, matching this filter
//! having no `t`/`n`-dependent behaviour to speak of, which is also why a
//! table indexed by input sample value can be precomputed once rather than
//! evaluated per pixel.
//!
//! `negval = minval + maxval - val`, confirmed by `lut=c0=negval` on a
//! constant `0x80` (128) gray frame producing `0x7f` (127) —
//! `255 + 0 - 128 = 127`.
//!
//! The identity table (`c0=val` on every channel, or the default
//! `"clipval"` since `minval<=val<=maxval` always) must be a no-op — this
//! crate's own implementation is not the oracle, the *definition* of a
//! lookup table is: applying `f(x)=x` cannot change any sample. Tested
//! below against non-trivial pixel data.

use vaco_core::{MediaType, Result};
use vaco_expr::{Bindings, Expr};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::common;
use crate::sample;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

const VARS: &[&str] = &["val", "clipval", "maxval", "minval", "negval", "w", "h"];

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(
    name = "lut",
    help = "Compute and apply a lookup table to the input video"
)]
pub(crate) struct Opts {
    #[opt(name = "c0", alias = "y", alias = "r", help = "set component #0 expression", default = "clipval".to_owned(), flags(video, filtering))]
    pub c0: String,
    #[opt(name = "c1", alias = "u", alias = "g", help = "set component #1 expression", default = "clipval".to_owned(), flags(video, filtering))]
    pub c1: String,
    #[opt(name = "c2", alias = "v", alias = "b", help = "set component #2 expression", default = "clipval".to_owned(), flags(video, filtering))]
    pub c2: String,
    #[opt(name = "c3", alias = "a", help = "set component #3 expression", default = "clipval".to_owned(), flags(video, filtering))]
    pub c3: String,
}

/// One channel's precomputed table, indexed by input sample value.
#[derive(Debug, Clone)]
struct Table(Vec<u16>);

impl Table {
    fn build(expr: &Expr, depth: u8, width: u32, height: u32) -> Self {
        let max = f64::from(sample::max_for_depth(depth));
        let n = (max as usize).saturating_add(1);
        let mut table = Vec::new();
        for val in 0..n {
            let v = val as f64;
            let clipval = v.clamp(0.0, max);
            let negval = max - v;
            let out = expr.eval(&[
                v,
                clipval,
                max,
                0.0,
                negval,
                f64::from(width),
                f64::from(height),
            ]);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped to [0, max] and max fits in u16 by construction"
            )]
            let out_v = out.clamp(0.0, max).round() as u16;
            table.push(out_v);
        }
        Self(table)
    }

    fn apply(&self, v: u16) -> u16 {
        self.0.get(v as usize).copied().unwrap_or(v)
    }
}

#[derive(Debug)]
pub(crate) struct Filter {
    exprs: [Expr; 4],
    tables: Vec<Option<Table>>,
}

impl Filter {
    fn new(o: &Opts) -> std::result::Result<Self, String> {
        let bindings = Bindings::new(VARS);
        let parse = |s: &str| {
            Expr::parse(s, &bindings).map_err(|e| format!("lut: bad expression `{s}`: {e}"))
        };
        Ok(Self {
            exprs: [parse(&o.c0)?, parse(&o.c1)?, parse(&o.c2)?, parse(&o.c3)?],
            tables: Vec::new(),
        })
    }

    fn rebuild_tables(&mut self, format: vaco_pixfmt::PixFmt, width: u32, height: u32) {
        let n = format.component_count().min(4);
        self.tables = (0..n)
            .map(|ch| {
                let comp = sample::component(format, ch)?;
                let expr = self.exprs.get(ch)?;
                Some(Table::build(expr, comp.depth, width, height))
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
        let big_endian = format.is_big_endian();
        for (ch, table) in self.tables.iter().enumerate() {
            let Some(table) = table else { continue };
            let Some(comp) = sample::component(format, ch) else {
                continue;
            };
            let Some(mut plane) = input.plane_mut(comp.plane as usize) else {
                continue;
            };
            let width = plane
                .row_bytes()
                .checked_div(usize::from(comp.step.max(1)))
                .unwrap_or(0);
            for y in 0..plane.rows() {
                let Some(row) = plane.row_mut(y) else {
                    continue;
                };
                for x in 0..width {
                    let v = sample::read(row, x, comp, big_endian);
                    let out = table.apply(v);
                    if out != v {
                        sample::write(row, x, comp, big_endian, out);
                    }
                }
            }
        }
    }
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(vaco_filter_core::LinkFormat::Video {
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

fn build(desc: FilterDesc, req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts: Opts = common::parse(req.args)?;
    let filter = Filter::new(&opts)?;
    let set = FormatSet::video_list(common::formats_where(sample::is_addressable));
    Ok(Instance {
        desc,
        formats: NodeFormats::uniform(1, 1, MediaType::Video, &set, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

#[allow(
    clippy::module_inception,
    reason = "the module name is the registered filter name, alongside sibling `lutrgb`/`lutyuv` submodules"
)]
pub mod lut {
    use super::{FilterDesc, FilterFlags, Instance, Instantiate, VIDEO_PAD, build};

    pub const DESC: FilterDesc = FilterDesc {
        name: "lut",
        description: "Compute and apply a lookup table to the RGB/YUV input video",
        inputs: VIDEO_PAD,
        outputs: VIDEO_PAD,
        flags: FilterFlags::TIMELINE_GENERIC,
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        build(DESC, req)
    }
}

pub mod lutrgb {
    use super::{FilterDesc, FilterFlags, Instance, Instantiate, VIDEO_PAD, build};

    pub const DESC: FilterDesc = FilterDesc {
        name: "lutrgb",
        description: "Compute and apply a lookup table to the RGB input video",
        inputs: VIDEO_PAD,
        outputs: VIDEO_PAD,
        flags: FilterFlags::TIMELINE_GENERIC,
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        build(DESC, req)
    }
}

pub mod lutyuv {
    use super::{FilterDesc, FilterFlags, Instance, Instantiate, VIDEO_PAD, build};

    pub const DESC: FilterDesc = FilterDesc {
        name: "lutyuv",
        description: "Compute and apply a lookup table to the YUV input video",
        inputs: VIDEO_PAD,
        outputs: VIDEO_PAD,
        flags: FilterFlags::TIMELINE_GENERIC,
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        build(DESC, req)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_limits::{Budget, Limits};
    use vaco_pixfmt::PixFmt;

    fn identity_opts() -> Opts {
        Opts {
            c0: "clipval".to_owned(),
            c1: "clipval".to_owned(),
            c2: "clipval".to_owned(),
            c3: "clipval".to_owned(),
        }
    }

    #[test]
    fn identity_table_is_a_no_op() {
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Rgb24, 2, 1).unwrap();
        {
            let mut p = frame.plane_mut(0).unwrap();
            let row = p.row_mut(0).unwrap();
            row[0] = 7;
            row[1] = 200;
            row[2] = 42;
            row[3] = 1;
            row[4] = 254;
            row[5] = 0;
        }
        let mut f = Filter::new(&identity_opts()).unwrap();
        f.rebuild_tables(PixFmt::Rgb24, 2, 1);
        let before = frame.plane(0).unwrap().row(0).unwrap().to_vec();
        f.apply_frame(&mut frame);
        let after = frame.plane(0).unwrap().row(0).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn negval_matches_the_measured_formula() {
        // Independent oracle: negval = minval + maxval - val = 255 - val
        // for 8-bit, measured against the reference in this module's doc.
        let mut opts = identity_opts();
        opts.c0 = "negval".to_owned();
        let mut f = Filter::new(&opts).unwrap();
        f.rebuild_tables(PixFmt::Gray8, 1, 1);
        let table = f.tables[0].as_ref().unwrap();
        assert_eq!(table.apply(128), 127);
        assert_eq!(table.apply(0), 255);
        assert_eq!(table.apply(255), 0);
    }

    #[test]
    fn constant_expression_flattens_every_sample() {
        let mut opts = identity_opts();
        opts.c0 = "128".to_owned();
        let mut f = Filter::new(&opts).unwrap();
        f.rebuild_tables(PixFmt::Gray8, 1, 1);
        let mut budget = Budget::new(Limits::strict());
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Gray8, 3, 1).unwrap();
        {
            let mut p = frame.plane_mut(0).unwrap();
            let row = p.row_mut(0).unwrap();
            row[0] = 0;
            row[1] = 50;
            row[2] = 255;
        }
        f.apply_frame(&mut frame);
        assert_eq!(frame.plane(0).unwrap().row(0).unwrap(), &[128, 128, 128]);
    }

    #[test]
    fn bad_expression_is_a_clean_error() {
        let mut opts = identity_opts();
        opts.c0 = "not_a_real_var".to_owned();
        assert!(Filter::new(&opts).is_err());
    }
}
