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
//! `\frz`/`\fr` rotates that mask counterclockwise around `\org`, or the
//! line's aligned position when no explicit rotation origin is present.

use vaco_core::{Duration, Error, MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_filter_text::{Anchor, TextRenderer, TextStyle, mask};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

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
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
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
        let bytes = std::fs::read(&opts.filename)
            .map_err(|e| format!("ass: could not read `{}`: {e}", opts.filename))?;
        let (utf8, _) = vaco_format_subtitle::encoding::decode_to_utf8_bytes(&bytes);
        let text = String::from_utf8_lossy(&utf8).into_owned();
        Ok(Self {
            script: vaco_ass::parse(&text),
            renderer: TextRenderer::new(),
        })
    }
}

/// Render every active event of `script` onto `frame` at time `t`, sharing
/// `renderer`'s caches — the one rendering path [`Filter::filter_frame`]
/// and `vaco-filter-subtitle`'s `subtitles` filter (for a `.ass`/`.ssa`
/// input) both call. `pub` (not `pub(crate)`) so this crate's own
/// differential tests can drive it directly without a full graph.
///
/// # Errors
/// [`Error::Unsupported`] if `frame` is not video; whatever
/// [`vaco_filter_text::mask::composite`] or rasterisation reports.
pub fn render_at(
    script: &vaco_ass::Script,
    renderer: &mut TextRenderer,
    frame: &mut Frame,
    t: Duration,
) -> Result<()> {
    let FrameData::Video { width, height, .. } = frame.data else {
        return Err(Error::Unsupported(
            "vaco-filter-subtitle::ass: not a video frame",
        ));
    };
    let scale_x = if script.info.play_res_x == 0 {
        1.0
    } else {
        f64::from(width) / f64::from(script.info.play_res_x)
    };
    let scale_y = if script.info.play_res_y == 0 {
        1.0
    } else {
        f64::from(height) / f64::from(script.info.play_res_y)
    };
    let events: Vec<_> = script.active_at(t).cloned().collect();
    let color_info = frame.color;

    for event in &events {
        let plan = vaco_ass::plan_event(script, event);
        let Some(first) = plan.runs.first() else {
            continue;
        };
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
            edge_position(
                plan.alignment,
                width,
                height,
                plan.margin_l,
                plan.margin_r,
                plan.margin_v,
                scale_x,
                scale_y,
            )
        };
        let (ox, oy) = anchor.place(
            target_x as f32,
            target_y as f32,
            layout.width as f32,
            layout.height as f32,
        );
        let origin = (ox.round() as i32, oy.round() as i32);

        let mut base_mask = renderer.rasterise(&layout, origin)?;
        if style.angle_z.is_finite() && style.angle_z.rem_euclid(360.0).abs() > f64::EPSILON {
            let rotation_origin = plan
                .origin
                .map(|(x, y)| (x * scale_x, y * scale_y))
                .filter(|(x, y)| x.is_finite() && y.is_finite())
                .unwrap_or((target_x, target_y));
            base_mask = rotate_mask(
                &base_mask,
                renderer.budget_mut(),
                rotation_origin,
                style.angle_z,
            )?;
        }
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

#[allow(
    clippy::too_many_arguments,
    reason = "one geometry resolution, matching drawbox's own precedent for this shape"
)]
#[allow(
    clippy::integer_division,
    reason = "an exact 3-column/3-row grid index from the numpad alignment code, not a lossy division"
)]
fn edge_position(
    alignment: i32,
    width: u32,
    height: u32,
    margin_l: i32,
    margin_r: i32,
    margin_v: i32,
    sx: f64,
    sy: f64,
) -> (f64, f64) {
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

fn apply_clip(
    mask: &mut vaco_filter_text::AlphaMask,
    clip: (f64, f64, f64, f64),
    sx: f64,
    sy: f64,
) {
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

/// Rotate coverage around a frame-space pivot. ASS's positive Z rotation
/// is counterclockwise on screen, whose Y axis points down, so the forward
/// transform negates the usual mathematical Y term. Sampling applies the
/// inverse transform at destination pixel centres to avoid holes.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "finite transformed bounds are clamped to i32 and interpolated coverage is clamped to u8"
)]
fn rotate_mask(
    source: &vaco_filter_text::AlphaMask,
    budget: &mut vaco_limits::Budget,
    pivot: (f64, f64),
    angle_degrees: f64,
) -> Result<vaco_filter_text::AlphaMask> {
    if source.w == 0
        || source.h == 0
        || !angle_degrees.is_finite()
        || !pivot.0.is_finite()
        || !pivot.1.is_finite()
    {
        return Ok(source.clone());
    }
    let angle = angle_degrees.rem_euclid(360.0);
    if angle.abs() <= f64::EPSILON {
        return Ok(source.clone());
    }
    let radians = angle.to_radians();
    let (sin, cos) = radians.sin_cos();
    let left = f64::from(source.x);
    let top = f64::from(source.y);
    let right = left + f64::from(source.w);
    let bottom = top + f64::from(source.h);
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for (x, y) in [(left, top), (right, top), (left, bottom), (right, bottom)] {
        let dx = x - pivot.0;
        let dy = y - pivot.1;
        let rotated_x = pivot.0 + cos * dx + sin * dy;
        let rotated_y = pivot.1 - sin * dx + cos * dy;
        min_x = min_x.min(rotated_x);
        min_y = min_y.min(rotated_y);
        max_x = max_x.max(rotated_x);
        max_y = max_y.max(rotated_y);
    }

    let clamp_i32 = |value: f64| value.clamp(f64::from(i32::MIN), f64::from(i32::MAX));
    let out_x = clamp_i32(min_x.floor()) as i32;
    let out_y = clamp_i32(min_y.floor()) as i32;
    let out_right = clamp_i32(max_x.ceil()) as i32;
    let out_bottom = clamp_i32(max_y.ceil()) as i32;
    let out_w = u32::try_from(i64::from(out_right) - i64::from(out_x)).unwrap_or(u32::MAX);
    let out_h = u32::try_from(i64::from(out_bottom) - i64::from(out_y)).unwrap_or(u32::MAX);
    let mut rotated = vaco_filter_text::AlphaMask::blank(budget, out_x, out_y, out_w, out_h)?;

    for row in 0..out_h {
        for col in 0..out_w {
            let dest_x = f64::from(out_x) + f64::from(col) + 0.5;
            let dest_y = f64::from(out_y) + f64::from(row) + 0.5;
            let dx = dest_x - pivot.0;
            let dy = dest_y - pivot.1;
            let source_x = pivot.0 + cos * dx - sin * dy;
            let source_y = pivot.1 + sin * dx + cos * dy;
            let coverage = sample_mask_bilinear(source, source_x, source_y);
            if let Some(slot) = rotated
                .coverage
                .get_mut(row as usize * out_w as usize + col as usize)
            {
                *slot = coverage;
            }
        }
    }
    Ok(rotated)
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "finite sample coordinates are clamped to i32 before accessing the bounded mask"
)]
fn sample_mask_bilinear(mask: &vaco_filter_text::AlphaMask, x: f64, y: f64) -> u8 {
    let local_x = x - f64::from(mask.x) - 0.5;
    let local_y = y - f64::from(mask.y) - 0.5;
    let clamp_i32 = |value: f64| value.clamp(f64::from(i32::MIN), f64::from(i32::MAX));
    let x0 = clamp_i32(local_x.floor()) as i32;
    let y0 = clamp_i32(local_y.floor()) as i32;
    let fx = local_x - f64::from(x0);
    let fy = local_y - f64::from(y0);
    let sample = |local_col: i32, local_row: i32| {
        let px = mask.x.saturating_add(local_col);
        let py = mask.y.saturating_add(local_row);
        f64::from(mask.coverage_at(px, py))
    };
    let top = sample(x0, y0) * (1.0 - fx) + sample(x0.saturating_add(1), y0) * fx;
    let bottom = sample(x0, y0.saturating_add(1)) * (1.0 - fx)
        + sample(x0.saturating_add(1), y0.saturating_add(1)) * fx;
    (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        if !matches!(input.data, FrameData::Video { .. }) {
            return Ok(FrameOut::One(input));
        }
        let t = input.pts.to_seconds(input.time_base).unwrap_or(0.0);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "microsecond precision from a real playback timestamp fits i64 for any real duration"
        )]
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
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code"
)]
mod tests {
    use super::*;

    fn visible_bounds(frame: &Frame) -> Option<(u32, u32, u32, u32)> {
        let FrameData::Video { width, height, .. } = frame.data else {
            return None;
        };
        let plane = frame.plane(0)?;
        let (mut min_x, mut min_y) = (width, height);
        let (mut max_x, mut max_y) = (0, 0);
        let mut found = false;
        for y in 0..height {
            let row = plane.row(usize::try_from(y).ok()?)?;
            for x in 0..width {
                if row.get(usize::try_from(x).ok()?).copied().unwrap_or(0) > 24 {
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                    found = true;
                }
            }
        }
        found.then(|| (min_x, min_y, max_x - min_x + 1, max_y - min_y + 1))
    }

    fn render_bounds(script: &str) -> (u32, u32, u32, u32) {
        use vaco_frame::FramePool;
        use vaco_pixfmt::PixFmt;

        let script = vaco_ass::parse(script);
        let pool = FramePool::default();
        let mut frame = pool.acquire_video(PixFmt::Gray8, 320, 240).unwrap();
        vaco_filter_draw::fill::fill(
            &mut frame,
            vaco_filter_draw::rect::Rect::full(320, 240),
            vaco_core::Rgba::BLACK,
        )
        .unwrap();
        let mut renderer = TextRenderer::new();
        render_at(&script, &mut renderer, &mut frame, Duration::ZERO).unwrap();
        visible_bounds(&frame).expect("the rotated text must be visible")
    }

    #[test]
    fn missing_filename_is_a_clean_error() {
        let req = Instantiate {
            name: "ass",
            instance: "ass",
            args: None,
            arguments: &[],
        };
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
        render_at(
            &script,
            &mut renderer,
            &mut frame,
            Duration::from_micros(1_000_000),
        )
        .unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn edge_position_bottom_center_matches_the_frame_bottom() {
        let (x, y) = edge_position(2, 640, 480, 10, 10, 20, 1.0, 1.0);
        assert!((x - 320.0).abs() < f64::EPSILON);
        assert!((y - 460.0).abs() < f64::EPSILON);
    }

    #[test]
    fn positive_quarter_turn_rotates_mask_counterclockwise() {
        use vaco_limits::{Budget, Limits};

        let mut budget = Budget::new(Limits::default());
        let mut mask = vaco_filter_text::AlphaMask::blank(&mut budget, 4, 4, 5, 5).unwrap();
        mask.coverage[8] = 255;

        let rotated = rotate_mask(&mask, &mut budget, (6.5, 6.5), 90.0).unwrap();

        assert_eq!(rotated.coverage_at(5, 5), 255);
        assert_eq!(rotated.coverage_at(7, 5), 0);
    }

    #[test]
    fn frz90_real_render_matches_the_reference_geometry() {
        let (x, y, w, h) = render_bounds(include_str!("../tests/data/frz90.ass"));
        // ffmpeg-full 9.0.1 + libass 0.17.5 reports crop=30:150:144:54
        // for this exact script; vaco's independent font stack reports
        // 37:182:140:29. The extents differ with rasterisation and font
        // metrics, while the centre and tall orientation must agree.
        assert!(x.abs_diff(144) <= 12, "left edge {x} is not near 144");
        assert!(y.abs_diff(54) <= 30, "top edge {y} is not near 54");
        assert!(w.abs_diff(30) <= 16, "width {w} is not near 30");
        assert!(h.abs_diff(150) <= 40, "height {h} is not near 150");
        assert!((2 * x + w).abs_diff(318) <= 16, "horizontal centre drifted");
        assert!((2 * y + h).abs_diff(258) <= 24, "vertical centre drifted");
        assert!(h > w.saturating_mul(3), "rotation must make the line tall");
    }

    #[test]
    fn org_moves_the_rotation_pivot_in_frame_space() {
        let centered = render_bounds(include_str!("../tests/data/frz90.ass"));
        let shifted = render_bounds(include_str!("../tests/data/frz90-org.ass"));

        // Moving only \org from (160,120) to (160,180) moves libass's
        // crop from (144,54) to (84,114): exactly 60 px left and down.
        assert_eq!(shifted.0.saturating_add(60), centered.0);
        assert_eq!(shifted.1, centered.1.saturating_add(60));
    }
}
