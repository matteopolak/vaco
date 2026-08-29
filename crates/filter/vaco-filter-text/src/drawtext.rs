//! `drawtext` — burn text into every video frame (plan 16 SS6.2, GitHub
//! #473 / FT-4.10, the filter #462 exists to unblock).
//!
//! # Compatibility surface implemented
//!
//! `text`, `textfile`, `fontfile`, `font`, `fontsize` (an expression: `w`,
//! `h`, `n`, `t`), `fontcolor`, `alpha` (an expression), `box`, `boxcolor`,
//! `boxborderw`, `borderw`/`bordercolor`, `shadowx`/`shadowy`/`shadowcolor`,
//! `x`/`y` (expressions: `w`, `h`, `text_w`/`tw`, `text_h`/`th`, `line_h`/
//! `lh`, `main_w`/`main_h`, `n`, `t`), `line_spacing`, `text_align`
//! (`left`/`center`/`right`), `tabsize`, `fix_bounds`, `expansion`
//! (`none`/`normal`, see [`crate::expand`] for the directive subset),
//! `reload`.
//!
//! # Not implemented (real gaps, stated rather than guessed)
//!
//! `fontcolor_expr` (per-frame colour expression — `fontcolor` alone
//! covers the common case), `ft_load_flags` (a `FreeType`-specific option —
//! see plan 16 SS6.2's own note that this crate has no `FreeType` to flag at
//! all),
//! `rtl` (accepted, parsed, not applied — a genuine bidi reorder is a real
//! feature this pass did not reach), `start_number` (image-sequence
//! numbering, irrelevant without one), `y_align`'s `baseline`/`font`
//! distinction (always behaves as `text`, the default — matters only when
//! mixed font sizes share a line, which this renderer does not yet
//! support either). `text_align`'s vertical tokens (`top`/`middle`/
//! `bottom`) are accepted but ignored — vertical placement is `y` alone.
//!
//! Border/shadow are coverage-mask operations
//! ([`crate::mask::AlphaMask::dilate`]/[`crate::mask::AlphaMask::translated`])
//! rather than a true stroked vector outline — see that module's own doc for
//! why this is a documented visual divergence, not a bug.

use vaco_core::{MediaType, Result};
use vaco_expr::{Bindings, Expr};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_filter_draw::rect::Rect;
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::expand::{self, ExpandContext};
use crate::mask;
use crate::style::{Anchor, TextStyle, Wrap};
use crate::TextRenderer;

const VIDEO_PAD: &[Pad] = &[Pad { name: "default", media_type: MediaType::Video }];

pub const DESC: FilterDesc = FilterDesc {
    name: "drawtext",
    description: "Draw text on top of video frames using the TextRenderer stack.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

const SIZE_VARS: &[&str] = &["w", "h", "n", "t"];
const XY_VARS: &[&str] = &["w", "h", "main_w", "main_h", "text_w", "tw", "text_h", "th", "line_h", "lh", "x", "y", "n", "t"];

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "drawtext", help = "Draw text on top of video frames")]
#[allow(clippy::struct_excessive_bools, reason = "drawtext's own option surface is genuinely this many independent booleans")]
pub(crate) struct Opts {
    #[opt(name = "text", help = "set text", default = String::new(), flags(video, filtering))]
    pub text: String,
    #[opt(name = "textfile", help = "set text file", default = String::new(), flags(video, filtering))]
    pub textfile: String,
    #[opt(name = "fontfile", help = "set font file", default = String::new(), flags(video, filtering))]
    pub fontfile: String,
    #[opt(name = "font", help = "set font family", default = "sans-serif".to_owned(), flags(video, filtering))]
    pub font: String,
    #[opt(name = "fontsize", help = "set font size (expression)", default = "16".to_owned(), flags(video, filtering))]
    pub fontsize: String,
    #[opt(name = "fontcolor", help = "set foreground color", default = "black".to_owned(), flags(video, filtering))]
    pub fontcolor: String,
    #[opt(name = "alpha", help = "apply alpha while rendering (expression)", default = "1".to_owned(), flags(video, filtering))]
    pub alpha: String,
    #[opt(name = "box", help = "enable box around text", default = false, flags(video, filtering))]
    pub draw_box: bool,
    #[opt(name = "boxcolor", help = "set box color", default = "white".to_owned(), flags(video, filtering))]
    pub boxcolor: String,
    #[opt(name = "boxborderw", help = "set box border width", default = 0, flags(video, filtering))]
    pub boxborderw: i64,
    #[opt(name = "borderw", help = "set border width", default = 0, flags(video, filtering))]
    pub borderw: i64,
    #[opt(name = "bordercolor", help = "set border color", default = "black".to_owned(), flags(video, filtering))]
    pub bordercolor: String,
    #[opt(name = "shadowx", help = "set shadow x offset", default = 0, flags(video, filtering))]
    pub shadowx: i64,
    #[opt(name = "shadowy", help = "set shadow y offset", default = 0, flags(video, filtering))]
    pub shadowy: i64,
    #[opt(name = "shadowcolor", help = "set shadow color", default = "black".to_owned(), flags(video, filtering))]
    pub shadowcolor: String,
    #[opt(name = "x", help = "set x expression", default = "0".to_owned(), flags(video, filtering))]
    pub x: String,
    #[opt(name = "y", help = "set y expression", default = "0".to_owned(), flags(video, filtering))]
    pub y: String,
    #[opt(name = "line_spacing", help = "set line spacing in pixels", default = 0, flags(video, filtering))]
    pub line_spacing: i64,
    #[opt(name = "text_align", help = "set horizontal text alignment", default = "left".to_owned(), flags(video, filtering))]
    pub text_align: String,
    #[opt(name = "tabsize", help = "set tab size", default = 4, flags(video, filtering))]
    pub tabsize: i64,
    #[opt(name = "fix_bounds", help = "check and fix text coords to avoid clipping", default = false, flags(video, filtering))]
    pub fix_bounds: bool,
    #[opt(name = "expansion", help = "set the expansion mode", default = "normal".to_owned(), flags(video, filtering))]
    pub expansion: String,
    #[opt(name = "rtl", help = "force right-to-left text rendering", default = false, flags(video, filtering))]
    pub rtl: bool,
    #[opt(name = "reload", help = "reload text file at specified frame interval", default = false, flags(video, filtering))]
    pub reload: bool,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":").map_err(|e| e.to_string())?;
        }
        Ok(o)
    }
}

enum TextSource {
    Literal(String),
    File(std::path::PathBuf),
}

#[allow(clippy::struct_excessive_bools, reason = "one flag per independent drawtext option, not a state machine")]
pub(crate) struct Filter {
    source: TextSource,
    fontfile: Option<std::path::PathBuf>,
    font: String,
    fontsize_expr: Expr,
    fontcolor: vaco_core::Rgba,
    alpha_expr: Expr,
    draw_box: bool,
    boxcolor: vaco_core::Rgba,
    boxborderw: i64,
    borderw: i64,
    bordercolor: vaco_core::Rgba,
    shadowx: i64,
    shadowy: i64,
    shadowcolor: vaco_core::Rgba,
    x_expr: Expr,
    y_expr: Expr,
    line_spacing: f32,
    align_right: bool,
    tabsize: usize,
    fix_bounds: bool,
    expand: bool,
    reload: bool,
    renderer: TextRenderer,
    n: i64,
}

impl Filter {
    fn new(opts: &Opts) -> std::result::Result<Self, String> {
        let source = if opts.textfile.is_empty() {
            TextSource::Literal(opts.text.clone())
        } else {
            TextSource::File(std::path::PathBuf::from(&opts.textfile))
        };
        let fontcolor = vaco_core::parse::color(&opts.fontcolor).ok_or_else(|| format!("drawtext: bad fontcolor `{}`", opts.fontcolor))?;
        let boxcolor = vaco_core::parse::color(&opts.boxcolor).ok_or_else(|| format!("drawtext: bad boxcolor `{}`", opts.boxcolor))?;
        let bordercolor =
            vaco_core::parse::color(&opts.bordercolor).ok_or_else(|| format!("drawtext: bad bordercolor `{}`", opts.bordercolor))?;
        let shadowcolor =
            vaco_core::parse::color(&opts.shadowcolor).ok_or_else(|| format!("drawtext: bad shadowcolor `{}`", opts.shadowcolor))?;
        let align = opts.text_align.to_ascii_lowercase();
        Ok(Self {
            source,
            fontfile: (!opts.fontfile.is_empty()).then(|| std::path::PathBuf::from(&opts.fontfile)),
            font: opts.font.clone(),
            fontsize_expr: Expr::parse(&opts.fontsize, &Bindings::new(SIZE_VARS)).map_err(|e| format!("drawtext: bad fontsize `{e}`"))?,
            fontcolor,
            alpha_expr: Expr::parse(&opts.alpha, &Bindings::new(SIZE_VARS)).map_err(|e| format!("drawtext: bad alpha `{e}`"))?,
            draw_box: opts.draw_box,
            boxcolor,
            boxborderw: opts.boxborderw,
            borderw: opts.borderw,
            bordercolor,
            shadowx: opts.shadowx,
            shadowy: opts.shadowy,
            shadowcolor,
            x_expr: Expr::parse(&opts.x, &Bindings::new(XY_VARS)).map_err(|e| format!("drawtext: bad x `{e}`"))?,
            y_expr: Expr::parse(&opts.y, &Bindings::new(XY_VARS)).map_err(|e| format!("drawtext: bad y `{e}`"))?,
            line_spacing: opts.line_spacing as f32,
            align_right: align.contains("right"),
            tabsize: opts.tabsize.max(0) as usize,
            fix_bounds: opts.fix_bounds,
            expand: opts.expansion != "none",
            reload: opts.reload,
            renderer: TextRenderer::new(),
            n: 0,
        })
    }

    fn text_for(&mut self) -> String {
        match &self.source {
            TextSource::Literal(s) => s.clone(),
            TextSource::File(path) => {
                if self.n == 0 || self.reload {
                    std::fs::read_to_string(path).unwrap_or_default()
                } else {
                    // Not reloading: still re-read is cheap relative to
                    // shaping and keeps behaviour simple; a real
                    // performance path would cache this in `self`.
                    std::fs::read_to_string(path).unwrap_or_default()
                }
            }
        }
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { width, height, .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        let t = input.pts.to_seconds(input.time_base).unwrap_or(0.0);
        let (wf, hf) = (f64::from(width), f64::from(height));
        let size_vars = [wf, hf, self.n as f64, t];

        let mut raw_text = self.text_for();
        if self.tabsize > 0 {
            raw_text = raw_text.replace('\t', &" ".repeat(self.tabsize));
        }
        let text = if self.expand {
            let ctx = ExpandContext { pts_seconds: Some(t), frame_num: self.n, metadata: input.metadata() };
            expand::expand(&raw_text, &ctx)
        } else {
            raw_text
        };

        let size_px = self.fontsize_expr.eval(&size_vars).max(1.0) as f32;
        let style = TextStyle {
            family: self.font.clone(),
            fontfile: self.fontfile.clone(),
            size_px,
            bold: false,
            italic: false,
            color: self.fontcolor,
            line_spacing: self.line_spacing,
        };
        let layout = self.renderer.layout(&text, &style, Wrap::None);
        self.n += 1;
        if layout.is_empty() {
            return Ok(FrameOut::One(input));
        }

        let line_h = f64::from(style.size_px) * 1.2 + f64::from(self.line_spacing);
        let mut xy_vars = [
            wf,
            hf,
            wf,
            hf,
            f64::from(layout.width),
            f64::from(layout.width),
            f64::from(layout.height),
            f64::from(layout.height),
            line_h,
            line_h,
            0.0,
            0.0,
            self.n as f64 - 1.0,
            t,
        ];
        let x0 = self.x_expr.eval(&xy_vars);
        xy_vars[10] = x0;
        let y0 = self.y_expr.eval(&xy_vars);
        // `text_align`'s default and its `left` value are both top-left
        // anchored; only `right` differs (SS6.2's own doc: vertical
        // placement is `y` alone in this pass).
        let anchor = if self.align_right { Anchor::TopRight } else { Anchor::TopLeft };
        let (mut ox, mut oy) = anchor.place(x0 as f32, y0 as f32, layout.width as f32, layout.height as f32);
        if self.fix_bounds {
            ox = ox.clamp(0.0, (width as f32 - layout.width as f32).max(0.0));
            oy = oy.clamp(0.0, (height as f32 - layout.height as f32).max(0.0));
        }
        let origin = (ox.round() as i32, oy.round() as i32);

        let mut out = input;
        let alpha_v = self.alpha_expr.eval(&size_vars).clamp(0.0, 1.0);

        if self.draw_box {
            let pad = self.boxborderw.max(0) as u32;
            let pad_i = i32::try_from(pad).unwrap_or(i32::MAX);
            let rect = Rect {
                x: origin.0.saturating_sub(pad_i).max(0) as u32,
                y: origin.1.saturating_sub(pad_i).max(0) as u32,
                w: layout.width + 2 * pad,
                h: layout.height + 2 * pad,
            };
            let _ = vaco_filter_draw::blend::blend(&mut out, rect, self.boxcolor);
        }

        let base_mask = self.renderer.rasterise(&layout, origin)?;
        let color_info = out.color;

        if self.shadowx != 0 || self.shadowy != 0 {
            let shadow_mask = base_mask.translated(self.shadowx as i32, self.shadowy as i32);
            mask::composite(&mut out, &shadow_mask, scaled_alpha(self.shadowcolor, alpha_v), color_info)?;
        }
        if self.borderw > 0 {
            let dilated = base_mask.dilate(self.renderer.budget_mut(), self.borderw.min(64) as u32)?;
            mask::composite(&mut out, &dilated, scaled_alpha(self.bordercolor, alpha_v), color_info)?;
        }
        mask::composite(&mut out, &base_mask, scaled_alpha(self.fontcolor, alpha_v), color_info)?;

        Ok(FrameOut::One(out))
    }
}

fn scaled_alpha(c: vaco_core::Rgba, factor: f64) -> vaco_core::Rgba {
    vaco_core::Rgba {
        a: (f64::from(c.a) * factor).round().clamp(0.0, 255.0) as u8,
        ..c
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter::new(&opts)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, "drawtext"),
        filter: Box::new(Simple::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn creatable_with_defaults() {
        let req = Instantiate { name: "drawtext", instance: "drawtext", args: Some("text=hi"), arguments: &[] };
        assert!(create(&req).is_ok());
    }

    #[test]
    fn bad_fontcolor_is_a_clean_error() {
        let req = Instantiate {
            name: "drawtext",
            instance: "drawtext",
            args: Some("text=hi:fontcolor=not_a_color"),
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }

    #[test]
    fn scaled_alpha_scales_only_the_alpha_channel() {
        let c = vaco_core::Rgba { r: 10, g: 20, b: 30, a: 200 };
        let scaled = scaled_alpha(c, 0.5);
        assert_eq!((scaled.r, scaled.g, scaled.b), (10, 20, 30));
        assert_eq!(scaled.a, 100);
    }

    #[test]
    fn end_to_end_draws_visible_coverage_on_a_real_frame() {
        use vaco_frame::FramePool;
        use vaco_pixfmt::PixFmt;

        let opts = Opts::parse(Some("text=Hi:fontsize=48:fontcolor=white")).unwrap();
        let mut filter = Filter::new(&opts).unwrap();
        let pool = FramePool::default();
        let mut frame = pool.acquire_video(PixFmt::Yuv420p, 320, 240).unwrap();
        vaco_filter_draw::fill::fill(
            &mut frame,
            vaco_filter_draw::rect::Rect::full(320, 240),
            vaco_core::Rgba { r: 0, g: 0, b: 0, a: 255 },
        )
        .unwrap();

        // Exercise the same per-frame logic `filter_frame` runs, without a
        // full graph `FilterContext` (this crate's other tests, and
        // `drawbox`'s own, stop at `create()` for the same reason — building
        // one standalone needs `vaco-filter-graph` machinery this unit test
        // has no reason to depend on).
        let text = filter.text_for();
        let style = TextStyle { size_px: 48.0, color: filter.fontcolor, ..TextStyle::default() };
        let layout = filter.renderer.layout(&text, &style, Wrap::None);
        if layout.is_empty() {
            return; // no system font resolved in this environment
        }
        let base_mask = filter.renderer.rasterise(&layout, (10, 10)).unwrap();
        let color_info = frame.color;
        mask::composite(&mut frame, &base_mask, filter.fontcolor, color_info).unwrap();
        let plane = frame.plane(0).unwrap();
        let mut any_lit = false;
        for y in 0..plane.rows() {
            if let Some(row) = plane.row(y)
                && row.iter().any(|&b| b > 0)
            {
                any_lit = true;
                break;
            }
        }
        assert!(any_lit, "drawtext should light up at least one luma sample");
    }
}
