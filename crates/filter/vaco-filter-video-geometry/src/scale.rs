//! `scale` — resize, as a thin adapter over `vaco-scale`.
//!
//! This filter does **not** reimplement any resampling: it evaluates `w`/`h`
//! into concrete pixel dimensions and hands the actual work to
//! [`vaco_scale::Scaler`], per this crate's brief ("`scale` the *filter* is a
//! thin adapter... not a reimplementation").
//!
//! # Pixel format
//!
//! `scale`'s own `pix_fmts` option and the `in_color_matrix`/
//! `out_color_matrix` options are **not implemented**, and a plain
//! `-vf scale=W:H` still keeps the source's pixel format unchanged — `create`
//! still declares [`NodeFormats::passthrough`], tying the input and output
//! pads to the same value, exactly as before.
//!
//! What changed: [`Filter::filter_frame`] no longer *assumes* the tie means
//! the input and output formats are the same value it can read off the input
//! frame. It reads back whatever [`Filter::configure`] resolved the *output*
//! link to and asks [`vaco_scale::Scaler`] to convert straight to it in the
//! same pass that resizes. For a plain `-vf scale=W:H` the tie still forces
//! that value to equal the input's, so this is a no-op change there.
//!
//! It stops being a no-op for the node [`vaco_filter_graph::convert`] splices
//! in automatically to repair a pixel-format mismatch elsewhere in the graph
//! (`VIDEO_CONVERTER` names this same filter). That node is built through this
//! module's [`create`] too, but the negotiator supplies its own
//! [`vaco_filter_core::negotiate::NodeFormats`] for it — concrete, untied,
//! genuinely different formats on each pad — overriding what `create` would
//! have declared on its own. Before this fix, an auto-inserted `scale` node
//! reported success at a `yuv420p` → `rgb24` conversion while its
//! `filter_frame` silently built both `vaco_scale::ImageSpec`s from the
//! *input* frame's format, leaving the bytes unconverted — E2E-GAPS 1's
//! `-vf scale=…,-pix_fmt rgb24` case, which failed further downstream (a png
//! encoder refusing the still-`yuv420p` frame, or the negotiator's own round
//! bound, depending on how the mismatch was phrased).
//!
//! # Measured (ffmpeg 8.1): `w`/`h` are not symmetric
//!
//! ```text
//! ffmpeg -f lavfi -i color=red:s=100x60 -vf scale=w=50            # ERRORS
//! ffmpeg -f lavfi -i color=red:s=100x60 -vf scale=h=30            # -> 100x30, SAR 1/2
//! ```
//!
//! Giving `w` (or `width`) with `h`/`height` absent is a hard filter-init
//! error, `Invalid size '<w's value>'`, whatever `w` actually is — `-1`, `50`
//! and `200` all fail identically. Giving `h` alone works and keeps the
//! input's width unchanged. There is no width-only shorthand; write
//! `w=X:h=-1` to scale one axis and derive the other. Reproduced here: `w`
//! present with `h` absent is rejected before either expression is even
//! evaluated.
//!
//! # Measured: `-1` rounds to nearest, `-2` rounds to nearest *even*
//!
//! On a 101×61 input, `scale=w=51:h=-1` gives `51x31` (round(51·61/101) =
//! round(30.79) = 31); `scale=w=51:h=-2` gives `51x30` (nearest even to
//! 30.79 is 30, not 32). When *both* resolve to `-1`/`-2` simultaneously
//! (no anchor to compute from), the reference falls back to the input size
//! unchanged rather than erroring or looping.
//!
//! # Measured: SAR is always corrected to preserve DAR, not just for `-1`/`-2`
//!
//! Every probed resize — explicit `w`/`h`, `-1`, `-2`, an expression, or
//! omitting one dimension — comes out with
//! `sar_new = sar_old * (in_w*out_h) / (in_h*out_w)`, exactly. That is simply
//! "keep the display aspect ratio fixed across a resize", and it is not
//! special-cased to the rounding paths — it runs unconditionally.

use vaco_core::{MediaType, Rational, Result};
use vaco_expr::{Bindings, Expr};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_pixfmt::PixFmt;
use vaco_scale::{ImageSpec, ScaleOptions, Scaler};

use vaco_filter_graph::registry::{Instance, Instantiate};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "scale",
    description: "Scale the input video size and/or convert the image format",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

const DIM_VARS: &[&str] = &[
    "in_w", "in_h", "iw", "ih", "a", "sar", "dar", "hsub", "vsub",
];

/// An axis expression: absent, or the raw text a user gave for `w`/`h`.
#[derive(Debug, Clone)]
enum Axis {
    /// The user did not mention this axis at all: keep the input's value.
    Unset,
    Given(Expr),
}

#[derive(Debug)]
pub(crate) struct Filter {
    w: Axis,
    h: Axis,
    out_wh: (u32, u32),
    /// The pixel format [`Filter::configure`] resolved the output link to.
    /// `None` until the first `configure`, which is the only state
    /// [`Filter::filter_frame`] needs to know whether a format conversion
    /// (as opposed to a pure resize) is required.
    out_format: Option<PixFmt>,
    /// Built once in [`create`] from whichever of `flags`/`param0`/`param1`/
    /// `in_range`/`out_range` the user actually gave — see [`create`]'s own
    /// doc for why this filter no longer always hands
    /// [`vaco_scale::ScaleOptions::default`] to the scaler regardless of
    /// what was asked for.
    scale_opts: ScaleOptions,
}

impl Filter {
    fn eval_axis(axis: &Axis, in_w: u32, in_h: u32, sar: Rational, hsub: u8, vsub: u8) -> f64 {
        match axis {
            Axis::Unset => f64::NAN, // resolved by the caller against `iw`/`ih`
            Axis::Given(e) => {
                let a = if in_h == 0 {
                    0.0
                } else {
                    f64::from(in_w) / f64::from(in_h)
                };
                let dar = a * sar.to_f64();
                e.eval(&[
                    f64::from(in_w),
                    f64::from(in_h),
                    f64::from(in_w),
                    f64::from(in_h),
                    a,
                    sar.to_f64(),
                    dar,
                    f64::from(hsub),
                    f64::from(vsub),
                ])
            }
        }
    }

    /// Resolve `w`/`h` to concrete pixel dimensions, per this module's
    /// measured rules.
    fn resolve(&self, format: PixFmt, in_w: u32, in_h: u32, sar: Rational) -> (u32, u32) {
        let (hsub, vsub) = format.log2_chroma();
        let w_raw = Self::eval_axis(&self.w, in_w, in_h, sar, hsub, vsub);
        let h_raw = Self::eval_axis(&self.h, in_w, in_h, sar, hsub, vsub);

        let w_sentinel = matches!(&self.w, Axis::Given(_)) && is_sentinel(w_raw);
        let h_sentinel = matches!(&self.h, Axis::Given(_)) && is_sentinel(h_raw);

        if w_sentinel && h_sentinel {
            // No anchor to compute either from: measured fallback is the
            // input size, unchanged.
            return (in_w.max(1), in_h.max(1));
        }

        let mut w = match &self.w {
            Axis::Unset => in_w,
            Axis::Given(_) if w_sentinel => 0, // filled in below, once `h` is known
            Axis::Given(_) => to_dim(w_raw, in_w),
        };
        let mut h = match &self.h {
            Axis::Unset => in_h,
            Axis::Given(_) if h_sentinel => 0,
            Axis::Given(_) => to_dim(h_raw, in_h),
        };
        if w_sentinel {
            w = from_ratio(f64::from(h), in_w, in_h, is_minus_two(w_raw));
        }
        if h_sentinel {
            h = from_ratio(f64::from(w), in_h, in_w, is_minus_two(h_raw));
        }
        (w.max(1), h.max(1))
    }
}

/// Whether `v` is the exact sentinel `-1` or `-2`. Sentinels only ever arrive
/// as small integer literals evaluated by `vaco-expr`, so exact `f64`
/// equality is the correct check, not an approximation of one.
#[allow(
    clippy::float_cmp,
    reason = "-1/-2 are exact small-integer sentinels, not computed values"
)]
fn is_sentinel(v: f64) -> bool {
    v == -1.0 || v == -2.0
}

/// Whether `v` is exactly `-2` (the "round to even" sentinel, as opposed to
/// plain `-1`).
#[allow(
    clippy::float_cmp,
    reason = "-2 is an exact small-integer sentinel, not a computed value"
)]
fn is_minus_two(v: f64) -> bool {
    v == -2.0
}

/// `u32` to `i32`, saturating rather than wrapping — dimensions this large
/// never occur in practice, and `Rational`'s numerator is `i32`.
fn to_i32(v: u32) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

fn to_dim(v: f64, fallback: u32) -> u32 {
    if !v.is_finite() || v <= 0.0 {
        return fallback.max(1);
    }
    v.round() as u32
}

/// `other * target_in/other_in`, rounded to nearest (or nearest even when
/// `even`, the `-2` case). Measured: this is a plain nearest-integer round,
/// not a floor, and `-2`'s "even" is nearest-even, not floor-to-even.
fn from_ratio(other: f64, target_in: u32, other_in: u32, even: bool) -> u32 {
    if other_in == 0 {
        return target_in.max(1);
    }
    let exact = other * f64::from(target_in) / f64::from(other_in);
    let rounded = if even {
        (exact / 2.0).round() * 2.0
    } else {
        exact.round()
    };
    if rounded <= 0.0 { 1 } else { rounded as u32 }
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let Some(LinkFormat::Video {
            format,
            width,
            height,
            sample_aspect_ratio,
            ..
        }) = ctx.input_link(0).cloned()
        else {
            return Ok(());
        };
        let (out_w, out_h) = self.resolve(format, width, height, sample_aspect_ratio);
        self.out_wh = (out_w, out_h);
        let new_sar = if width == 0 || height == 0 || out_w == 0 || out_h == 0 {
            sample_aspect_ratio
        } else {
            Rational::new(to_i32(width), to_i32(height))
                .checked_div(Rational::new(to_i32(out_w), to_i32(out_h)))
                .and_then(|r| sample_aspect_ratio.checked_mul(r))
                .unwrap_or(sample_aspect_ratio)
        };
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video {
                format: out_format,
                width: w,
                height: h,
                sample_aspect_ratio: sar,
                ..
            } = &mut out
            {
                // Read back what negotiation resolved the *output* link to,
                // rather than assuming it must equal the input: a plain
                // `-vf scale=W:H` still ties the two pads together (see this
                // module's doc), so `*out_format` equals the input's format
                // there and this is a no-op read. An auto-inserted converter
                // node — spliced in with no such tie, to bridge a real
                // pixel-format mismatch — resolves its output pad to
                // something concretely different, and `filter_frame` below
                // now actually converts to it instead of silently keeping
                // the source format.
                self.out_format = Some(*out_format);
                *w = out_w;
                *h = out_h;
                *sar = new_sar;
            }
            ctx.set_output_link(0, out);
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
        let (out_w, out_h) = self.out_wh;
        let out_format = self.out_format.unwrap_or(format);
        if out_w == width && out_h == height && out_format == format {
            return Ok(FrameOut::One(input));
        }
        let mut out = ctx.pool().acquire_video(out_format, out_w, out_h)?;
        let src_spec = ImageSpec::new(format, width, height).with_color(input.color);
        let dst_spec = ImageSpec::new(out_format, out_w, out_h).with_color(input.color);
        let mut scaler = Scaler::new(&src_spec, &dst_spec, &self.scale_opts)?;
        scaler.scale_frame(&input, &mut out)?;
        out.pts = input.pts;
        out.time_base = input.time_base;
        out.duration = input.duration;
        out.color = input.color;
        out.flags = input.flags;
        Ok(FrameOut::One(out))
    }
}

fn parse_axis(text: Option<&str>) -> std::result::Result<Axis, String> {
    match text {
        None => Ok(Axis::Unset),
        Some(t) => {
            let bindings = Bindings::new(DIM_VARS);
            Expr::parse(t, &bindings)
                .map(Axis::Given)
                .map_err(|e| format!("scale: bad dimension expression `{t}`: {e}"))
        }
    }
}

fn parse_size(text: &str) -> std::result::Result<(Axis, Axis), String> {
    let (w, h) = text
        .split_once('x')
        .or_else(|| text.split_once('X'))
        .ok_or_else(|| format!("scale: bad `size` `{text}`"))?;
    Ok((parse_axis(Some(w))?, parse_axis(Some(h))?))
}

/// Names this filter neither implements nor refuses today would otherwise be
/// silently accepted and ignored — `ensure_known_options`
/// (`registry.rs`) only refuses a name the reference does not document *at
/// all* under `scale`, and every one of these is real, documented ffmpeg
/// `scale` syntax. Refusing them by name, matching what an undocumented name
/// like `sws_flags` already gets, is this project's own stated preference
/// over silently producing something the caller did not ask for.
const NOT_IMPLEMENTED: &[&str] = &[
    "interl",
    "in_color_matrix",
    "out_color_matrix",
    "in_chroma_loc",
    "out_chroma_loc",
    "in_primaries",
    "out_primaries",
    "in_transfer",
    "out_transfer",
    "in_v_chr_pos",
    "in_h_chr_pos",
    "out_v_chr_pos",
    "out_h_chr_pos",
    "force_original_aspect_ratio",
    "force_divisible_by",
    "reset_sar",
    "eval",
];

/// `in_range`/`out_range`'s own accepted spellings, the same convention
/// `vaco-filter-video-format::setrange`'s `Mode::parse` already uses for the
/// general-purpose `setrange`/`setparams` filters. Unlike that filter's own
/// three-way `Auto`/`Unspecified`/`Limited`/`Full`, [`ScaleOptions`]'s own
/// `src_range_full`/`dst_range_full` are plain "force full, or don't"
/// booleans — there is no way to represent "force limited" distinctly from
/// "leave whatever the frame signals alone" with the fields this crate
/// exposes today, so an explicit `limited`/`tv`/`mpeg` request collapses to
/// the same "don't force full" value `auto` already gets. That is a real,
/// stated gap (matches every practical case except overriding an
/// already-full-range source, e.g. `yuvj420p`, back down to limited) rather
/// than a silent one.
fn parse_range_full(name: &str, s: &str) -> std::result::Result<bool, String> {
    match s {
        "-1" | "auto" | "0" | "unspecified" | "unknown" | "1" | "limited" | "tv" | "mpeg" => {
            Ok(false)
        }
        "2" | "full" | "pc" | "jpeg" => Ok(true),
        other => Err(format!("scale: bad `{name}` `{other}`")),
    }
}

/// Builds the [`ScaleOptions`] [`Filter::filter_frame`] hands to
/// [`vaco_scale::Scaler`] from whichever of `flags`/`param0`/`param1`/
/// `in_range`/`out_range` the caller gave — see this module's own "Measured"
/// doc sections for the dimension-side options, none of which this function
/// touches.
///
/// # Errors
/// The first unimplemented option name actually supplied (see
/// [`NOT_IMPLEMENTED`]), or a `flags`/`in_range`/`out_range` value this
/// crate's own parsers do not recognise.
fn parse_scale_opts(req: &Instantiate<'_>) -> std::result::Result<ScaleOptions, String> {
    for &name in NOT_IMPLEMENTED {
        if req.named(name).is_some() {
            return Err(format!(
                "scale: option `{name}` is not implemented (refused by name rather than silently ignored)"
            ));
        }
    }
    let mut opts = ScaleOptions::default();
    if let Some(flags) = req.named("flags") {
        opts.parse(&format!("sws_flags={flags}")).map_err(|e| e.to_string())?;
    }
    if let Some(p0) = req.named("param0") {
        let v: f64 = p0
            .parse()
            .map_err(|_| format!("scale: bad `param0` `{p0}`"))?;
        opts.param0 = v;
    }
    if let Some(p1) = req.named("param1") {
        let v: f64 = p1
            .parse()
            .map_err(|_| format!("scale: bad `param1` `{p1}`"))?;
        opts.param1 = v;
    }
    if let Some(v) = req.named("in_range") {
        opts.src_range_full = parse_range_full("in_range", &v)?;
    }
    if let Some(v) = req.named("out_range") {
        opts.dst_range_full = parse_range_full("out_range", &v)?;
    }
    Ok(opts)
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let (w_axis, h_axis) = if let Some(size) = req.named("size").or_else(|| req.named("s")) {
        parse_size(&size)?
    } else {
        let mut w_text = req.named("w").or_else(|| req.named("width"));
        let mut h_text = req.named("h").or_else(|| req.named("height"));
        if w_text.is_none() && h_text.is_none() {
            w_text = req.positional(0);
            h_text = req.positional(1);
        }
        // Measured: `w` alone (h absent) is a hard error in the reference,
        // whatever `w`'s value is. `h` alone is fine and keeps `iw`.
        if let (Some(w), None) = (&w_text, &h_text) {
            return Err(format!("Invalid size '{w}'"));
        }
        (
            parse_axis(w_text.as_deref())?,
            parse_axis(h_text.as_deref())?,
        )
    };
    let scale_opts = parse_scale_opts(req)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter {
            w: w_axis,
            h: h_axis,
            out_wh: (0, 0),
            out_format: None,
            scale_opts,
        })),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp, reason = "test code")]
mod tests {
    use super::*;

    fn filter(w: Option<&str>, h: Option<&str>) -> Filter {
        Filter {
            w: parse_axis(w).unwrap(),
            h: parse_axis(h).unwrap(),
            out_wh: (0, 0),
            out_format: None,
            scale_opts: ScaleOptions::default(),
        }
    }

    #[test]
    fn neither_given_is_identity() {
        let f = filter(None, None);
        assert_eq!(
            f.resolve(PixFmt::Yuv420p, 100, 60, Rational::ONE),
            (100, 60)
        );
    }

    #[test]
    fn h_alone_keeps_input_width() {
        let f = filter(None, Some("30"));
        assert_eq!(
            f.resolve(PixFmt::Yuv420p, 100, 60, Rational::ONE),
            (100, 30)
        );
    }

    #[test]
    fn minus_one_rounds_to_nearest() {
        // Measured: 101x61, w=51,h=-1 -> 51x31.
        let f = filter(Some("51"), Some("-1"));
        assert_eq!(f.resolve(PixFmt::Yuv420p, 101, 61, Rational::ONE), (51, 31));
    }

    #[test]
    fn minus_two_rounds_to_nearest_even() {
        // Measured: 101x61, w=51,h=-2 -> 51x30 (not 32).
        let f = filter(Some("51"), Some("-2"));
        assert_eq!(f.resolve(PixFmt::Yuv420p, 101, 61, Rational::ONE), (51, 30));
    }

    #[test]
    fn both_sentinel_falls_back_to_input_size() {
        let f = filter(Some("-1"), Some("-1"));
        assert_eq!(
            f.resolve(PixFmt::Yuv420p, 100, 60, Rational::ONE),
            (100, 60)
        );
    }

    #[test]
    fn w_alone_is_rejected_by_create() {
        let ast = vaco_filter_graph::parse("scale=w=50").unwrap();
        let spec = ast.chains.first().and_then(|c| c.filters.first()).unwrap();
        let arguments = spec.arguments().unwrap();
        let req = Instantiate {
            name: &spec.name,
            instance: &spec.name,
            args: spec.args.as_deref(),
            arguments: &arguments,
        };
        assert!(create(&req).is_err());
    }

    #[test]
    fn h_alone_is_accepted_by_create() {
        let ast = vaco_filter_graph::parse("scale=h=30").unwrap();
        let spec = ast.chains.first().and_then(|c| c.filters.first()).unwrap();
        let arguments = spec.arguments().unwrap();
        let req = Instantiate {
            name: &spec.name,
            instance: &spec.name,
            args: spec.args.as_deref(),
            arguments: &arguments,
        };
        assert!(create(&req).is_ok());
    }

    fn create_str(src: &str) -> std::result::Result<Instance, String> {
        let ast = vaco_filter_graph::parse(src).unwrap();
        let spec = ast.chains.first().and_then(|c| c.filters.first()).unwrap();
        let arguments = spec.arguments().unwrap();
        let req = Instantiate {
            name: &spec.name,
            instance: &spec.name,
            args: spec.args.as_deref(),
            arguments: &arguments,
        };
        create(&req)
    }

    fn scale_opts_for(src: &str) -> std::result::Result<ScaleOptions, String> {
        let ast = vaco_filter_graph::parse(src).unwrap();
        let spec = ast.chains.first().and_then(|c| c.filters.first()).unwrap();
        let arguments = spec.arguments().unwrap();
        let req = Instantiate {
            name: &spec.name,
            instance: &spec.name,
            args: spec.args.as_deref(),
            arguments: &arguments,
        };
        parse_scale_opts(&req)
    }

    /// The exact bug the coordinator reported: `flags=neighbor` used to be
    /// silently accepted and ignored (`ScaleOptions::default()` always went
    /// to the scaler regardless), so every `flags=` value produced the same
    /// output. It must now actually select [`vaco_scale::filter::Kernel::Point`].
    #[test]
    fn flags_neighbor_selects_the_point_kernel() {
        let opts = scale_opts_for("scale=w=80:h=60:flags=neighbor").unwrap();
        assert_eq!(opts.luma_kernel(), vaco_scale::filter::Kernel::Point);
    }

    #[test]
    fn flags_lanczos_selects_the_lanczos_kernel() {
        let opts = scale_opts_for("scale=w=80:h=60:flags=lanczos").unwrap();
        assert!(matches!(
            opts.luma_kernel(),
            vaco_scale::filter::Kernel::Lanczos { .. }
        ));
    }

    #[test]
    fn param0_and_param1_reach_scale_options() {
        let opts = scale_opts_for("scale=w=80:h=60:param0=1:param1=-2.5").unwrap();
        assert_eq!(
            opts.luma_kernel(),
            vaco_scale::filter::Kernel::Bicubic { b: 1.0, c: -2.5 }
        );
    }

    #[test]
    fn in_range_full_forces_source_full_range() {
        let opts = scale_opts_for("scale=w=80:h=60:in_range=full").unwrap();
        assert!(opts.src_range_full);
        let opts = scale_opts_for("scale=w=80:h=60").unwrap();
        assert!(!opts.src_range_full);
    }

    /// A documented `scale` option this crate does not implement must be
    /// refused by name, matching what an undocumented name like `sws_flags`
    /// already gets — not silently accepted and ignored, which is the
    /// defect class this whole change removes.
    #[test]
    fn unimplemented_options_are_refused_by_name() {
        for opt in NOT_IMPLEMENTED {
            let src = format!("scale=w=80:h=60:{opt}=1");
            let result = create_str(&src);
            assert!(result.is_err(), "{opt} should be refused");
            let err = result.unwrap_err();
            assert!(err.contains(opt), "{err} should name {opt}");
        }
    }

    #[test]
    fn sar_is_corrected_to_preserve_dar() {
        let f = filter(Some("51"), Some("-1"));
        let (out_w, out_h) = f.resolve(PixFmt::Yuv420p, 101, 61, Rational::ONE);
        let new_sar = Rational::new(101, 61)
            .checked_div(Rational::new(to_i32(out_w), to_i32(out_h)))
            .unwrap();
        // Measured: sar:3131/3111.
        assert_eq!(new_sar.reduced(), Rational::new(3131, 3111).reduced());
    }

    /// The brief's own example of a real finding: `scale=w=99999999:
    /// h=99999999` must be refused, not attempted. This filter does not add
    /// its own bound — `vaco_frame::FramePool`'s default budget
    /// (`vaco-limits`-sized, 1 GiB live bytes) already rejects a single
    /// buffer this large with a clean error, before any allocation happens.
    #[test]
    fn an_outrageous_size_is_refused_by_the_frame_pool_not_attempted() {
        let f = filter(Some("99999999"), Some("99999999"));
        let (out_w, out_h) = f.resolve(PixFmt::Yuv420p, 100, 100, Rational::ONE);
        assert_eq!((out_w, out_h), (99_999_999, 99_999_999));
        let pool = vaco_frame::FramePool::default();
        let result = pool.acquire_video(PixFmt::Yuv420p, out_w, out_h);
        assert!(
            result.is_err(),
            "a ~10^16-byte plane must be rejected by the pool's budget, not allocated"
        );
    }
}
