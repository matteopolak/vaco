//! `overlay` — composite a second video input onto the first at a
//! per-frame position.
//!
//! `ffmpeg -h filter=overlay` documents `x`, `y`, `eof_action`, `eval`,
//! `shortest`, `format`, `repeatlast`, `alpha`, plus the shared
//! `vaco-filter-framesync` surface (`eof_action`/`shortest`/`repeatlast`
//! again, and `ts_sync_mode`), defaulting to `"0"`, `"0"`, `repeat`,
//! `frame`, `false`, `yuv420`, `true`, `auto`.
//!
//! # Two inputs, two independent timelines: this is what `vaco-filter-framesync` is for
//!
//! `overlay` is the dual-input family's worked example in
//! `vaco-filter-framesync`'s own docs (`mock::Stamp`'s doc literally
//! describes probing `overlay`), so this filter is a thin
//! [`FrameSyncFilter`] over the roles [`FsInput::dual`] already provides:
//! input 0 (`main`) drives at `sync=2`, input 1 (`overlay`) is sampled at
//! `sync=1` and starts `Null` (invisible before its first frame).
//! `eof_action`/`shortest`/`repeatlast` are `apply_opts`'s truth table
//! unchanged — nothing here re-derives it. **`vaco-filter-framesync` needed
//! no changes and no workaround** for this filter; see this crate's report
//! for the one thing it does *not* provide (per-input latency into
//! `LinkStats`, already flagged as a signature gap in framesync's own docs).
//!
//! # Measured: `x`/`y` are evaluated per frame by default
//!
//! `eval=frame` is the reference's own default, not `init`: `overlay=x=4*n`
//! visibly moves the overlay by four columns every frame. `eval=init`
//! evaluates once, before the first frame — where `t` is undefined,
//! matching [`crate::rotate`]'s `ow`/`oh`, except here an indefinite result
//! degrades to "off screen" ([`crate::blend::to_pixel`]'s deterministic
//! `NaN -> 0`) rather than the hard configure error `rotate` raises, because
//! `x`/`y` are read every event regardless of `eval` and an out-of-range
//! placement is an ordinary, safe outcome for this filter — clipped away by
//! [`crate::blend::clip`], not a geometry that has to be valid for a link.
//!
//! # Measured: `x` and `y` can each use the other's freshly computed value
//!
//! `x=8:y=x/2` places the overlay at `(8, 4)`; `x=y/2:y=8` places it at
//! `(4, 8)` — both use the *other* option's value, not textual order. This
//! crate reproduces the shape of that (not a perfect fixed point, which the
//! reference itself does not reach either: a genuine mutual cycle,
//! `x=y:y=x+1`, produces no visible overlay in the reference, and this
//! crate does not attempt to match that specific pathological case) by
//! evaluating `x`, then `y` against the fresh `x`, then re-evaluating `x`
//! against the fresh `y` — see [`eval_xy`].
//!
//! # Measured: `w`/`h`/`W`/`H`/`overlay_w`/`overlay_h` are all the *overlay's*
//! own dimensions
//!
//! Not "output" and not "main" — placing at `x=W` moves the overlay exactly
//! one of its own widths to the right, confirmed against a non-square
//! overlay for `H`/`h`/`overlay_h` too. `main_w`/`main_h` are the only
//! spelling for the background's dimensions; `pos` and `main_t` are not
//! valid variables (probed and rejected by the reference).
//!
//! # `format=`/`alpha=`
//!
//! See [`crate::format_opt`] and [`crate::blend`] for the measured mapping
//! and formula. `format=yuv420` (the default) has **no** alpha plane, unlike
//! every wider or deeper variant — an overlay under the default `format=`
//! is therefore always fully opaque, matching the reference's own
//! opaque-by-default feel for the common case.

use vaco_core::{MediaType, Result};
use vaco_expr::{Bindings, Expr};
use vaco_filter_core::adapt::FrameOut;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat};
use vaco_filter_framesync::{FrameSyncEvent, FrameSyncFilter, FrameSyncOpts, FsInput, Synced};
use vaco_frame::{Frame, FrameData, FramePool};
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;
use vaco_scale::{ImageSpec, ScaleOptions, Scaler};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::blend::{self, AlphaMode};
use crate::format_opt::Format;
use crate::geom;

pub const DESC: FilterDesc = FilterDesc {
    name: "overlay",
    description: "Overlay a video source on top of the input",
    inputs: vaco_filter_framesync::mock::DUAL_VIDEO_PADS,
    outputs: vaco_filter_framesync::mock::VIDEO_PAD,
    flags: FilterFlags::empty(),
};

/// `x`/`y`'s shared variable list, measured (see this module's doc): the
/// background's own dimensions only under `main_w`/`main_h`; the overlay's
/// under five different aliases; `x`/`y` themselves, for the cross-reference
/// [`eval_xy`] implements.
const XY_VARS: &[&str] = &[
    "main_w",
    "main_h",
    "W",
    "H",
    "w",
    "h",
    "overlay_w",
    "overlay_h",
    "hsub",
    "vsub",
    "n",
    "t",
    "x",
    "y",
];

/// When `x`/`y` are (re-)evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Eval {
    Init,
    #[default]
    Frame,
}

impl Eval {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "init" | "0" => Some(Self::Init),
            "frame" | "1" => Some(Self::Frame),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "overlay", help = "Overlay a video source on top of the input")]
pub(crate) struct Opts {
    #[opt(name = "x", help = "set the x expression", default = "0".to_owned(), flags(video, filtering))]
    pub x: String,
    #[opt(name = "y", help = "set the y expression", default = "0".to_owned(), flags(video, filtering))]
    pub y: String,
    #[opt(
        name = "eof_action",
        help = "action to take when encountering EOF from secondary input",
        default = "repeat".to_owned(),
        flags(video, filtering)
    )]
    pub eof_action: String,
    #[opt(
        name = "eval",
        help = "specify when to evaluate expressions",
        default = "frame".to_owned(),
        flags(video, filtering)
    )]
    pub eval: String,
    #[opt(
        name = "shortest",
        help = "force termination when the shortest input terminates",
        default = false,
        flags(video, filtering)
    )]
    pub shortest: bool,
    #[opt(
        name = "format",
        help = "set output format",
        default = "yuv420".to_owned(),
        flags(video, filtering)
    )]
    pub format: String,
    #[opt(
        name = "repeatlast",
        help = "repeat overlay of the last overlay frame",
        default = true,
        flags(video, filtering)
    )]
    pub repeatlast: bool,
    #[opt(
        name = "alpha",
        help = "alpha format",
        default = "auto".to_owned(),
        flags(video, filtering)
    )]
    pub alpha: String,
    #[opt(
        name = "ts_sync_mode",
        help = "how strictly to sync streams based on secondary input timestamps",
        default = "default".to_owned(),
        flags(video, filtering)
    )]
    pub ts_sync_mode: String,
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
pub(crate) struct Overlay {
    x_expr: Expr,
    y_expr: Expr,
    eval: Eval,
    fs_opts: FrameSyncOpts,
    format_opt: Format,
    alpha_mode: AlphaMode,
    n: u64,
    /// Resolved once at `configure`. `None` until then.
    blend_format: Option<PixFmt>,
    /// `eval=init`'s cached placement, computed on the first event.
    cached_xy: Option<(i64, i64)>,
}

impl Overlay {
    pub(crate) fn new(opts: &Opts) -> std::result::Result<Self, String> {
        let bindings = Bindings::new(XY_VARS);
        let x_expr =
            Expr::parse(&opts.x, &bindings).map_err(|e| format!("overlay: bad `x` `{e}`"))?;
        let y_expr =
            Expr::parse(&opts.y, &bindings).map_err(|e| format!("overlay: bad `y` `{e}`"))?;
        let eval = Eval::from_name(&opts.eval)
            .ok_or_else(|| format!("overlay: bad `eval` `{}`", opts.eval))?;
        let eof_action = vaco_filter_framesync::EofAction::from_name(&opts.eof_action)
            .ok_or_else(|| format!("overlay: bad `eof_action` `{}`", opts.eof_action))?;
        let ts_sync = vaco_filter_framesync::TsSyncMode::from_name(&opts.ts_sync_mode)
            .ok_or_else(|| format!("overlay: bad `ts_sync_mode` `{}`", opts.ts_sync_mode))?;
        let format_opt = Format::from_name(&opts.format)
            .ok_or_else(|| format!("overlay: bad `format` `{}`", opts.format))?;
        let alpha_mode = AlphaMode::from_name(&opts.alpha)
            .ok_or_else(|| format!("overlay: bad `alpha` `{}`", opts.alpha))?;
        Ok(Self {
            x_expr,
            y_expr,
            eval,
            fs_opts: FrameSyncOpts {
                eof_action,
                shortest: opts.shortest,
                repeatlast: opts.repeatlast,
                ts_sync,
            },
            format_opt,
            alpha_mode,
            n: 0,
            blend_format: None,
            cached_xy: None,
        })
    }

    /// Boxed and wrapped in [`Synced`], ready for [`vaco_filter_core::Graph::add`].
    #[must_use]
    pub(crate) fn boxed(self) -> Box<Synced<Self>> {
        Box::new(Synced::new(self))
    }
}

/// `x`, then `y` against the fresh `x`, then `x` again against the fresh
/// `y` — see this module's doc for the two probes this reproduces.
fn eval_xy(x_expr: &Expr, y_expr: &Expr, base: &mut [f64; 14]) -> (f64, f64) {
    if let Some(slot) = base.get_mut(12) {
        *slot = 0.0;
    }
    let x0 = x_expr.eval(base);
    if let Some(slot) = base.get_mut(12) {
        *slot = x0;
    }
    let y0 = y_expr.eval(base);
    if let Some(slot) = base.get_mut(13) {
        *slot = y0;
    }
    let x1 = x_expr.eval(base);
    (x1, y0)
}

impl FrameSyncFilter for Overlay {
    fn inputs(&self, n: usize) -> Vec<FsInput> {
        FsInput::dual(n)
    }

    fn opts(&self) -> FrameSyncOpts {
        self.fs_opts
    }

    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let Some(LinkFormat::Video {
            format: main_format,
            ..
        }) = ctx.input_link(0).cloned()
        else {
            return Ok(());
        };
        let blend_format = self.format_opt.resolve(main_format)?;
        geom::ensure_addressable_8bit(blend_format)?;
        self.blend_format = Some(blend_format);
        // `overlay` keeps the *main* input's time base on the output, unlike
        // `blend`/the stack family which keep the common one `Synced`
        // installs by default — measured via `vaco-filter-framesync`'s own
        // probe (see that crate's docs), reproduced here by overriding it
        // back to main's after `Synced::configure` has run.
        if let (Some(main_tb), Some(mut out)) = (
            ctx.input_link(0).map(LinkFormat::time_base),
            ctx.output_link(0).cloned(),
        ) {
            if let LinkFormat::Video {
                format, time_base, ..
            } = &mut out
            {
                *format = blend_format;
                *time_base = main_tb;
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn on_event(
        &mut self,
        ctx: &mut FilterContext<'_>,
        event: &mut FrameSyncEvent<'_>,
    ) -> Result<FrameOut> {
        let Some(main) = event.take(0) else {
            return Ok(FrameOut::None);
        };
        let Some(blend_format) = self.blend_format else {
            return Ok(FrameOut::One(main));
        };
        let FrameData::Video {
            width: main_w,
            height: main_h,
            ..
        } = main.data
        else {
            return Ok(FrameOut::One(main));
        };
        let n = self.n;
        self.n = self.n.saturating_add(1);

        let mut main = reformat(ctx.pool(), main, blend_format)?;

        let Some(overlay_frame) = event.get(1) else {
            // No secondary frame at this event (not started yet, or gone
            // under `eof_action=pass`/`repeatlast=0`): the reference passes
            // the main frame through untouched.
            main.pts = event.timestamp();
            main.time_base = event.time_base();
            return Ok(FrameOut::One(main));
        };
        let FrameData::Video {
            format: ov_format,
            width: ov_w,
            height: ov_h,
            ..
        } = overlay_frame.data
        else {
            main.pts = event.timestamp();
            main.time_base = event.time_base();
            return Ok(FrameOut::One(main));
        };

        let (x, y) = if self.eval == Eval::Init {
            if let Some(xy) = self.cached_xy {
                xy
            } else {
                let xy = self.placement(main_w, main_h, ov_w, ov_h, ov_format, 0, f64::NAN);
                self.cached_xy = Some(xy);
                xy
            }
        } else {
            let t = event
                .timestamp()
                .to_seconds(event.time_base())
                .unwrap_or(f64::NAN);
            self.placement(main_w, main_h, ov_w, ov_h, ov_format, n, t)
        };

        if let Some(rect) = blend::clip(x, y, ov_w, ov_h, main_w, main_h) {
            let overlay_reformatted = reformat(ctx.pool(), overlay_frame.clone(), blend_format)?;
            blend::composite(
                &mut main,
                &overlay_reformatted,
                blend_format,
                rect,
                self.alpha_mode,
            )?;
        }
        main.pts = event.timestamp();
        main.time_base = event.time_base();
        Ok(FrameOut::One(main))
    }
}

impl Overlay {
    #[allow(
        clippy::too_many_arguments,
        reason = "every argument names a distinct measured variable"
    )]
    fn placement(
        &self,
        main_w: u32,
        main_h: u32,
        ov_w: u32,
        ov_h: u32,
        ov_format: PixFmt,
        n: u64,
        t: f64,
    ) -> (i64, i64) {
        let (hsub, vsub) = ov_format.log2_chroma();
        let mut vars = [
            f64::from(main_w),
            f64::from(main_h),
            f64::from(ov_w),
            f64::from(ov_h),
            f64::from(ov_w),
            f64::from(ov_h),
            f64::from(ov_w),
            f64::from(ov_h),
            f64::from(hsub),
            f64::from(vsub),
            n as f64,
            t,
            0.0,
            0.0,
        ];
        let (x, y) = eval_xy(&self.x_expr, &self.y_expr, &mut vars);
        (blend::to_pixel(x), blend::to_pixel(y))
    }
}

/// Convert `frame` to `target`, if it is not already. Same-size, colour-only
/// conversion — `vaco-scale` handles the colour matrix; this crate never
/// hand-derives one.
fn reformat(pool: &FramePool, frame: Frame, target: PixFmt) -> Result<Frame> {
    let FrameData::Video {
        format,
        width,
        height,
        ..
    } = frame.data
    else {
        return Ok(frame);
    };
    if format == target {
        return Ok(frame);
    }
    let mut out = pool.acquire_video(target, width, height)?;
    let src_spec = ImageSpec::new(format, width, height).with_color(frame.color);
    let dst_spec = ImageSpec::new(target, width, height).with_color(frame.color);
    let mut scaler = Scaler::new(&src_spec, &dst_spec, &ScaleOptions::default())?;
    scaler.scale_frame(&frame, &mut out)?;
    out.pts = frame.pts;
    out.time_base = frame.time_base;
    out.duration = frame.duration;
    out.color = frame.color;
    out.flags = frame.flags;
    out.sample_aspect_ratio = frame.sample_aspect_ratio;
    Ok(out)
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let overlay = Overlay::new(&opts)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(2, 1, MediaType::Video, req.instance),
        filter: overlay.boxed(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn opts(x: &str, y: &str) -> Opts {
        Opts {
            x: x.to_owned(),
            y: y.to_owned(),
            eof_action: "repeat".to_owned(),
            eval: "frame".to_owned(),
            shortest: false,
            format: "yuv420".to_owned(),
            repeatlast: true,
            alpha: "auto".to_owned(),
            ts_sync_mode: "default".to_owned(),
        }
    }

    #[test]
    fn x_can_use_ys_fresh_value() {
        let o = Overlay::new(&opts("8", "x/2")).unwrap();
        let (x, y) = o.placement(20, 20, 4, 4, PixFmt::Rgb24, 0, 0.0);
        assert_eq!((x, y), (8, 4));
    }

    #[test]
    fn y_can_use_xs_fresh_value() {
        let o = Overlay::new(&opts("y/2", "8")).unwrap();
        let (x, y) = o.placement(20, 20, 4, 4, PixFmt::Rgb24, 0, 0.0);
        assert_eq!((x, y), (4, 8));
    }

    #[test]
    fn w_h_and_aliases_are_the_overlays_own_dimensions() {
        let o = Overlay::new(&opts("W", "h")).unwrap();
        let (x, y) = o.placement(20, 20, 5, 8, PixFmt::Rgb24, 0, 0.0);
        assert_eq!((x, y), (5, 8));
    }

    #[test]
    fn main_w_is_the_background_dimension() {
        let o = Overlay::new(&opts("main_w", "main_h")).unwrap();
        let (x, y) = o.placement(20, 30, 5, 5, PixFmt::Rgb24, 0, 0.0);
        assert_eq!((x, y), (20, 30));
    }

    #[test]
    fn n_advances_x_per_frame() {
        let o = Overlay::new(&opts("4*n", "0")).unwrap();
        assert_eq!(o.placement(20, 20, 4, 4, PixFmt::Rgb24, 0, 0.0).0, 0);
        assert_eq!(o.placement(20, 20, 4, 4, PixFmt::Rgb24, 1, 0.0).0, 4);
        assert_eq!(o.placement(20, 20, 4, 4, PixFmt::Rgb24, 2, 0.0).0, 8);
    }

    #[test]
    fn fractional_x_truncates_toward_zero() {
        let o = Overlay::new(&opts("5.9", "-1.5")).unwrap();
        assert_eq!(o.placement(20, 20, 4, 4, PixFmt::Rgb24, 0, 0.0), (5, -1));
    }

    #[test]
    fn format_and_alpha_options_parse_the_full_measured_vocabulary() {
        for f in [
            "yuv420",
            "yuv420p10",
            "yuv422",
            "yuv422p10",
            "yuv444",
            "yuv444p10",
            "rgb",
            "gbrp",
            "auto",
        ] {
            let o = opts("0", "0");
            let mut o = o;
            o.format = f.to_owned();
            assert!(Overlay::new(&o).is_ok(), "format={f}");
        }
        for a in ["auto", "unknown", "straight", "premultiplied"] {
            let mut o = opts("0", "0");
            o.alpha = a.to_owned();
            assert!(Overlay::new(&o).is_ok(), "alpha={a}");
        }
    }
}
