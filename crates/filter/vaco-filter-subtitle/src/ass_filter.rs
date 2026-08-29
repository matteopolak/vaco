//! `ass` — burn an ASS/SSA script onto video, self-contained like the
//! reference's own filter (`ass=filename=...`, no second input pad): the
//! whole script is read and parsed once at construction, since ASS files
//! are small, complete documents rather than a stream.
//!
//! GitHub #487/#488 (FT-5.2/5.3) build the parsing and tag-interpretation
//! library (`vaco-ass`); this module is the filter that drives it, per
//! plan 16 SS6.3's own crate split.
//!
//! # A real, stated simplification: one style per event line
//!
//! [`vaco_ass::plan_event`] correctly splits a line into several
//! [`vaco_ass::TextRun`]s, each with its own resolved style, wherever an
//! override tag mid-line changes something (`plain{\b1}bold`). This filter
//! concatenates every run's text and renders the **whole line in the
//! first run's style** — mixed formatting within one line is not applied.
//! The overwhelming majority of authored dialogue puts its override tags
//! at the *start* of the line (`{\an8\c&H00FF00&}text`), which this
//! handles correctly; the mid-line case is a real, named gap; see this
//! crate's own docs for what a proper multi-run inline layout would need
//! (positioning several `TextRenderer::layout` calls left-to-right, which
//! `vaco-filter-text`'s current API does not yet expose a primitive for).
//!
//! `\clip`'s rectangle is applied by zeroing mask coverage outside it
//! after rasterisation. `BorderStyle=3` (opaque box) is not implemented —
//! every event renders as outline+shadow (`BorderStyle=1`) regardless.

use vaco_core::{Duration, Error, MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_filter_text::{mask, Anchor, TextRenderer, TextStyle};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

const VIDEO_PAD: &[Pad] = &[Pad { name: "default", media_type: MediaType::Video }];

pub const DESC: FilterDesc = FilterDesc {
    name: "ass",
    description: "Render ASS/SSA subtitles onto the input video.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "ass", help = "Render ASS/SSA subtitles onto the input video")]
pub(crate) struct Opts {
    #[opt(name = "filename", alias = "f", help = "set the ASS/SSA file to render", default = String::new(), flags(video, filtering))]
    pub filename: String,
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

pub(crate) struct Filter {
    script: vaco_ass::Script,
    renderer: TextRenderer,
}

impl Filter {
    fn new(opts: &Opts) -> std::result::Result<Self, String> {
        if opts.filename.is_empty() {
            return Err("ass: filename is required".to_owned());
        }
        let bytes = std::fs::read(&opts.filename).map_err(|e| format!("ass: could not read `{}`: {e}", opts.filename))?;
        let (utf8, _) = vaco_format_subtitle::encoding::decode_to_utf8_bytes(&bytes);
        let text = String::from_utf8_lossy(&utf8).into_owned();
        Ok(Self { script: vaco_ass::parse(&text), renderer: TextRenderer::new() })
    }
}

/// Render every active event of `script` onto `frame` at time `t`, sharing
/// `renderer`'s caches — the one rendering path [`Filter::filter_frame`]
/// and `vaco-filter-subtitle`'s `subtitles` filter (for a `.ass`/`.ssa`
/// input) both call.
pub(crate) fn render_at(script: &vaco_ass::Script, renderer: &mut TextRenderer, frame: &mut Frame, t: Duration) -> Result<()> {
    let FrameData::Video { width, height, .. } = frame.data else {
        return Err(Error::Unsupported("vaco-filter-subtitle::ass: not a video frame"));
    };
    let scale_x = if script.info.play_res_x == 0 { 1.0 } else { f64::from(width) / f64::from(script.info.play_res_x) };
    let scale_y = if script.info.play_res_y == 0 { 1.0 } else { f64::from(height) / f64::from(script.info.play_res_y) };
    let events: Vec<_> = script.active_at(t).cloned().collect();
    let color_info = frame.color;

    for event in &events {
        let plan = vaco_ass::plan_event(script, event);
        let Some(first) = plan.runs.first() else { continue };
        let text: String = plan.runs.iter().map(|r| r.text.as_str()).collect();
        if text.trim().is_empty() {
            continue;
        }
        let style = &first.style;
        let text_style = TextStyle {
            family: style.fontname.clone(),
            size_px: (style.fontsize * scale_y).max(1.0) as f32,
            bold: style.bold,
            italic: style.italic,
            color: style.primary,
            ..TextStyle::default()
        };
        let layout = renderer.layout(&text, &text_style, vaco_filter_text::Wrap::None);
        if layout.is_empty() {
            continue;
        }

        let anchor = Anchor::from_ass_code(plan.alignment);
        let (target_x, target_y) = if let Some((px, py)) = plan.pos {
            (px * scale_x, py * scale_y)
        } else {
            edge_position(plan.alignment, width, height, plan.margin_l, plan.margin_r, plan.margin_v, scale_x, scale_y)
        };
        let (ox, oy) = anchor.place(target_x as f32, target_y as f32, layout.width as f32, layout.height as f32);
        let origin = (ox.round() as i32, oy.round() as i32);

        let mut base_mask = renderer.rasterise(&layout, origin)?;
        if let Some(clip) = plan.clip {
            apply_clip(&mut base_mask, clip, scale_x, scale_y);
        }

        if style.shadow > 0.0 {
            let shad = shadow_px(style.shadow, scale_x, scale_y);
            let mut shadow_mask = base_mask.translated(shad.0, shad.1);
            if let Some(clip) = plan.clip {
                apply_clip(&mut shadow_mask, clip, scale_x, scale_y);
            }
            mask::composite(frame, &shadow_mask, style.back_colour, color_info)?;
        }
        if style.outline > 0.0 {
            let bord = outline_px(style.outline, scale_x, scale_y);
            let mut dilated = base_mask.dilate(renderer.budget_mut(), bord)?;
            if let Some(clip) = plan.clip {
                apply_clip(&mut dilated, clip, scale_x, scale_y);
            }
            mask::composite(frame, &dilated, style.outline_colour, color_info)?;
        }
        mask::composite(frame, &base_mask, style.primary, color_info)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments, reason = "one geometry resolution, matching drawbox's own precedent for this shape")]
#[allow(clippy::integer_division, reason = "an exact 3-column/3-row grid index from the numpad alignment code, not a lossy division")]
fn edge_position(alignment: i32, width: u32, height: u32, margin_l: i32, margin_r: i32, margin_v: i32, sx: f64, sy: f64) -> (f64, f64) {
    let col = ((alignment - 1) % 3).max(0);
    let row = (alignment - 1) / 3;
    let x = match col {
        0 => f64::from(margin_l) * sx,
        2 => f64::from(width) - f64::from(margin_r) * sx,
        _ => f64::from(width) / 2.0,
    };
    let y = match row {
        0 => f64::from(height) - f64::from(margin_v) * sy,
        2 => f64::from(margin_v) * sy,
        _ => f64::from(height) / 2.0,
    };
    (x, y)
}

fn outline_px(outline: f64, sx: f64, sy: f64) -> u32 {
    ((outline * (sx + sy) / 2.0).round().max(0.0) as u32).min(64)
}

fn shadow_px(shadow: f64, sx: f64, sy: f64) -> (i32, i32) {
    let v = (shadow * (sx + sy) / 2.0).round() as i32;
    (v, v)
}

fn apply_clip(mask: &mut vaco_filter_text::AlphaMask, clip: (f64, f64, f64, f64), sx: f64, sy: f64) {
    let (x1, y1, x2, y2) = clip;
    let (fx1, fy1, fx2, fy2) = (x1 * sx, y1 * sy, x2 * sx, y2 * sy);
    for row in 0..mask.h {
        for col in 0..mask.w {
            let px = f64::from(mask.x) + f64::from(col);
            let py = f64::from(mask.y) + f64::from(row);
            if (px < fx1 || px >= fx2 || py < fy1 || py >= fy2)
                && let Some(slot) = mask.coverage.get_mut((row * mask.w + col) as usize)
            {
                *slot = 0;
            }
        }
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        if !matches!(input.data, FrameData::Video { .. }) {
            return Ok(FrameOut::One(input));
        }
        let t = input.pts.to_seconds(input.time_base).unwrap_or(0.0);
        #[allow(clippy::cast_possible_truncation, reason = "microsecond precision from a real playback timestamp fits i64 for any real duration")]
        let dur = Duration::from_micros((t * 1_000_000.0).round() as i64);
        let mut out = input;
        render_at(&self.script, &mut self.renderer, &mut out, dur)?;
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter::new(&opts)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, "ass"),
        filter: Box::new(Simple::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn missing_filename_is_a_clean_error() {
        let req = Instantiate { name: "ass", instance: "ass", args: None, arguments: &[] };
        assert!(create(&req).is_err());
    }

    #[test]
    fn nonexistent_file_is_a_clean_error() {
        let req = Instantiate {
            name: "ass",
            instance: "ass",
            args: Some("filename=/no/such/file.ass"),
            arguments: &[],
        };
        assert!(create(&req).is_err());
    }

    #[test]
    fn renders_a_real_script_onto_a_real_frame() {
        use vaco_frame::FramePool;
        use vaco_pixfmt::PixFmt;

        let dir = std::env::temp_dir();
        let path = dir.join("vaco_ass_filter_test.ass");
        std::fs::write(
            &path,
            "[Script Info]\nPlayResX: 640\nPlayResY: 480\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, Alignment, MarginL, MarginR, MarginV\nStyle: Default,Arial,32,&H00FFFFFF,2,10,10,20\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.00,0:00:05.00,Default,,0,0,0,,Hello ASS\n",
        )
        .unwrap();

        let script = vaco_ass::parse(&std::fs::read_to_string(&path).unwrap());
        let mut renderer = TextRenderer::new();
        let pool = FramePool::default();
        let mut frame = pool.acquire_video(PixFmt::Yuv420p, 640, 480).unwrap();
        render_at(&script, &mut renderer, &mut frame, Duration::from_micros(1_000_000)).unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn edge_position_bottom_center_matches_the_frame_bottom() {
        let (x, y) = edge_position(2, 640, 480, 10, 10, 20, 1.0, 1.0);
        assert!((x - 320.0).abs() < f64::EPSILON);
        assert!((y - 460.0).abs() < f64::EPSILON);
    }
}
