//! `perspective` — remap the frame through a projective transform defined by
//! where its four corners land (or come from).
//!
//! `ffmpeg -h filter=perspective` documents `x0,y0` (top left, default
//! `"0","0"`), `x1,y1` (top right, default `"W","0"`), `x2,y2` (bottom left,
//! default `"0","H"`), `x3,y3` (bottom right, default `"W","H"`),
//! `interpolation` (`linear`=0 default, `cubic`=1), `sense` (`source`=0
//! default, `destination`=1) and `eval` (`init`=0 default, `frame`=1). All
//! four corner pairs are `vaco-expr` expressions, evaluated once at
//! `configure` (this crate's `eval=init`-only precedent — see `swaprect`).
//! `interpolation=cubic` used to silently run bilinear (documented as a
//! simplification: this crate has no bicubic kernel yet) — accepted,
//! wrong, no error. Verified concretely: real `ffmpeg 8.1` produces
//! genuinely different output for `interpolation=linear` vs. `cubic` on
//! the same input (a byte-level `cmp` disagrees), so the reference does
//! not itself collapse the two — this is a real, unimplemented value, not
//! a reference-matching approximation. [`Filter::new`] now rejects
//! `interpolation=cubic` with a named error instead of silently
//! substituting bilinear. `sense` is fully implemented (see below); this
//! is the one option in this filter actually load-bearing for
//! correctness, not a simplification.
//!
//! # Measured: identity and `sense`'s two directions
//!
//! Default options (`x0=0,y0=0,x1=W,y1=0,x2=0,y2=H,x3=W,y3=H`, `sense=source`)
//! reproduced a 4x4 ramp frame byte-for-byte — confirms `W`/`H` bind to the
//! *input* width/height and that `sense=source` corners are read directly as
//! "where in the source does this destination corner's content come from",
//! i.e. exactly the inverse map this filter needs with no extra inversion.
//! `sense=destination` was not separately probed; per the option's own text
//! ("specify locations in destination to send corners of source") it names
//! the *forward* map (source corner -> given point), so this implementation
//! inverts the fitted homography for that case — the only self-consistent
//! reading given `sense=source`'s confirmed meaning is direct-inverse.
//!
//! The homography itself ([`crate::warp::Homography`]) is standard 4-point
//! projective-transform algebra, not reference-specific; see that module.

use vaco_core::{Error, MediaType, Result};
use vaco_expr::{Bindings, Expr};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData, FramePool};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::fill::FillPattern;
use crate::geom;
use crate::sample::sample_plane_pixel;
use crate::warp::Homography;

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "perspective",
    description: "Correct the perspective of video",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

const VARS: &[&str] = &["W", "H", "in_w", "in_h", "iw", "ih"];

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "perspective", help = "Correct the perspective of video")]
pub(crate) struct Opts {
    #[opt(name = "x0", help = "set top left x coordinate", default = "0".to_owned(), flags(video, filtering))]
    pub x0: String,
    #[opt(name = "y0", help = "set top left y coordinate", default = "0".to_owned(), flags(video, filtering))]
    pub y0: String,
    #[opt(name = "x1", help = "set top right x coordinate", default = "W".to_owned(), flags(video, filtering))]
    pub x1: String,
    #[opt(name = "y1", help = "set top right y coordinate", default = "0".to_owned(), flags(video, filtering))]
    pub y1: String,
    #[opt(name = "x2", help = "set bottom left x coordinate", default = "0".to_owned(), flags(video, filtering))]
    pub x2: String,
    #[opt(name = "y2", help = "set bottom left y coordinate", default = "H".to_owned(), flags(video, filtering))]
    pub y2: String,
    #[opt(name = "x3", help = "set bottom right x coordinate", default = "W".to_owned(), flags(video, filtering))]
    pub x3: String,
    #[opt(name = "y3", help = "set bottom right y coordinate", default = "H".to_owned(), flags(video, filtering))]
    pub y3: String,
    #[opt(name = "interpolation", help = "set interpolation", unit = "interp", consts = PERSPECTIVE_INTERP_CONSTS, default = 0, range = 0..=1, flags(video, filtering))]
    pub interpolation: i32,
    #[opt(name = "sense", help = "specify the sense of the coordinates", unit = "sense", consts = PERSPECTIVE_SENSE_CONSTS, default = 0, range = 0..=1, flags(video, filtering))]
    pub sense: i32,
    #[opt(name = "eval", help = "specify when to evaluate expressions", unit = "eval_mode", consts = PERSPECTIVE_EVAL_CONSTS, default = 0, range = 0..=1, flags(video, filtering))]
    pub eval: i32,
}

/// `ffmpeg -h filter=perspective`'s own named constants, confirmed
/// directly. Plain hand-written `ConstDesc` lists on the existing `i32`
/// fields (not `#[derive(OptEnum)]`) so `opts.sense == 1`-style
/// comparisons at the call sites below need no change -- only parsing
/// gains the reference's named spelling, which used to fail outright.
const PERSPECTIVE_INTERP_CONSTS: &[vaco_opts::ConstDesc] = &[
    vaco_opts::ConstDesc {
        name: "linear",
        help: "",
        unit: "interp",
        value: vaco_opts::ConstValue::Int(0),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "cubic",
        help: "",
        unit: "interp",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
];

const PERSPECTIVE_SENSE_CONSTS: &[vaco_opts::ConstDesc] = &[
    vaco_opts::ConstDesc {
        name: "source",
        help: "",
        unit: "sense",
        value: vaco_opts::ConstValue::Int(0),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "destination",
        help: "",
        unit: "sense",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
];

const PERSPECTIVE_EVAL_CONSTS: &[vaco_opts::ConstDesc] = &[
    vaco_opts::ConstDesc {
        name: "init",
        help: "",
        unit: "eval_mode",
        value: vaco_opts::ConstValue::Int(0),
        flags: vaco_opts::OptFlags::NONE,
    },
    vaco_opts::ConstDesc {
        name: "frame",
        help: "",
        unit: "eval_mode",
        value: vaco_opts::ConstValue::Int(1),
        flags: vaco_opts::OptFlags::NONE,
    },
];

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
pub(crate) struct Filter {
    exprs: [Expr; 8],
    bilinear: bool,
    destination_sense: bool,
    inverse: Option<Homography>,
}

impl Filter {
    pub(crate) fn new(opts: &Opts) -> std::result::Result<Self, String> {
        // The reference offers `linear`(0)/`cubic`(1), no nearest-neighbour
        // choice; this crate has no bicubic kernel, so `cubic` is rejected
        // by name rather than silently run as `linear` -- see module doc.
        if opts.interpolation == 1 {
            return Err(
                "perspective: interpolation=cubic is not implemented — see this module's doc"
                    .to_owned(),
            );
        }
        let b = Bindings::new(VARS);
        let parse = |s: &str, name: &str| {
            Expr::parse(s, &b).map_err(|e| format!("perspective: bad `{name}` `{e}`"))
        };
        Ok(Self {
            exprs: [
                parse(&opts.x0, "x0")?,
                parse(&opts.y0, "y0")?,
                parse(&opts.x1, "x1")?,
                parse(&opts.y1, "y1")?,
                parse(&opts.x2, "x2")?,
                parse(&opts.y2, "y2")?,
                parse(&opts.x3, "x3")?,
                parse(&opts.y3, "y3")?,
            ],
            bilinear: true,
            destination_sense: opts.sense == 1,
            inverse: None,
        })
    }

    fn compute(&self, in_w: u32, in_h: u32) -> Option<Homography> {
        let vars = [
            f64::from(in_w),
            f64::from(in_h),
            f64::from(in_w),
            f64::from(in_h),
            f64::from(in_w),
            f64::from(in_h),
        ];
        let mut pts = [(0.0, 0.0); 4];
        for i in 0..4 {
            let x = self.exprs.get(2 * i)?.eval(&vars);
            let y = self.exprs.get(2 * i + 1)?.eval(&vars);
            *pts.get_mut(i)? = (x, y);
        }
        let h = Homography::from_rect(f64::from(in_w), f64::from(in_h), pts)?;
        if self.destination_sense {
            // The given points are where each *source* corner lands, i.e.
            // the forward map; invert it to get dst->src.
            h.invert()
        } else {
            Some(h)
        }
    }
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let Some(LinkFormat::Video {
            format,
            width,
            height,
            ..
        }) = ctx.input_link(0).cloned()
        else {
            return Ok(());
        };
        geom::ensure_addressable(format)?;
        let (sw, sh) = format.log2_chroma();
        if sw != sh {
            return Err(Error::Unsupported(
                "perspective: asymmetric chroma subsampling (e.g. 4:2:2) is not supported",
            ));
        }
        self.inverse = self.compute(width, height);
        if self.inverse.is_none() {
            return Err(Error::InvalidData(
                "perspective: the four corners do not define a valid transform",
            ));
        }
        Ok(())
    }

    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video {
            format,
            width,
            height,
            ..
        } = input.data
        else {
            return Ok(FrameOut::One(input));
        };
        let Some(hom) = self.inverse else {
            return Ok(FrameOut::One(input));
        };
        let pool: &FramePool = ctx.pool();
        let fill = FillPattern::build(pool, format, (0, 0, 0), input.color)?;
        let mut out = pool.acquire_video(format, width, height)?;
        for p in 0..format.plane_count() {
            let plane_idx = p as u8;
            let unit = geom::plane_unit_bytes(format, plane_idx)?;
            let (sw, _) = format.log2_chroma();
            let factor = if p == 0 { 1.0 } else { f64::from(1u32 << sw) };
            let pw = format.plane_width(width, plane_idx);
            let ph = format.plane_height(height, plane_idx);
            let Some(src_plane) = input.plane(p) else {
                continue;
            };
            let Some(mut dst_plane) = out.plane_mut(p) else {
                continue;
            };
            let fill_px = fill.plane_pixel(p);
            for oy in 0..ph {
                let Some(row) = dst_plane.row_mut(oy as usize) else {
                    continue;
                };
                for ox in 0..pw {
                    // Evaluate the homography in full-resolution coordinates
                    // (consistent across planes for symmetric subsampling),
                    // then scale back down to this plane's own grid.
                    let full_dx = (f64::from(ox) + 0.5) * factor;
                    let full_dy = (f64::from(oy) + 0.5) * factor;
                    let (full_sx, full_sy) = hom.apply(full_dx, full_dy);
                    let src_x = full_sx / factor;
                    let src_y = full_sy / factor;
                    let start = (ox as usize).saturating_mul(unit);
                    if let Some(dst_px) = row.get_mut(start..start.saturating_add(unit)) {
                        sample_plane_pixel(
                            &src_plane,
                            unit,
                            pw,
                            ph,
                            src_x,
                            src_y,
                            self.bilinear,
                            fill_px,
                            dst_px,
                        );
                    }
                }
            }
        }
        out.pts = input.pts;
        out.time_base = input.time_base;
        out.duration = input.duration;
        out.flags = input.flags;
        out.sample_aspect_ratio = input.sample_aspect_ratio;
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter::new(&opts)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn default_options_are_the_identity_transform() {
        let opts = Opts::default();
        let filter = Filter::new(&opts).unwrap();
        let hom = filter.compute(4, 4).unwrap();
        let (x, y) = hom.apply(2.0, 3.0);
        assert!((x - 2.0).abs() < 1e-9);
        assert!((y - 3.0).abs() < 1e-9);
    }

    #[test]
    fn destination_sense_inverts_the_fitted_map() {
        let opts = Opts {
            sense: 1,
            ..Opts::default()
        };
        let filter = Filter::new(&opts).unwrap();
        let hom = filter.compute(4, 4).unwrap();
        // Still identity: inverting the identity is the identity.
        let (x, y) = hom.apply(1.0, 1.0);
        assert!((x - 1.0).abs() < 1e-9);
        assert!((y - 1.0).abs() < 1e-9);
    }

    /// Pinned against the reference's own named spelling
    /// (`ffmpeg -h filter=perspective`): the named form of each of the
    /// three enumerated options must parse, not just the bare integer.
    #[test]
    fn named_option_values_parse() {
        let opts = Opts::parse(Some("interpolation=cubic")).unwrap();
        assert_eq!(opts.interpolation, 1);
        let opts = Opts::parse(Some("sense=destination")).unwrap();
        assert_eq!(opts.sense, 1);
        let opts = Opts::parse(Some("eval=frame")).unwrap();
        assert_eq!(opts.eval, 1);
    }

    /// `interpolation=cubic` used to silently run bilinear -- a real,
    /// verified-against-the-reference divergence, not a rounding
    /// difference. `Filter::new` now rejects it by name.
    #[test]
    fn cubic_interpolation_is_a_named_error_not_a_silent_substitution() {
        let opts = Opts::parse(Some("interpolation=cubic")).unwrap();
        let err = Filter::new(&opts).unwrap_err();
        assert!(
            err.contains("perspective") && err.contains("not implemented"),
            "unexpected error text: {err}"
        );
    }

    #[test]
    fn linear_interpolation_still_creates() {
        let opts = Opts::parse(Some("interpolation=linear")).unwrap();
        assert!(Filter::new(&opts).is_ok());
    }
}
