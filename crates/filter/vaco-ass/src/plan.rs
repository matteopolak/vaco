//! Interpreting one [`crate::script::Event`]'s override tags against its
//! [`crate::style::Style`] into an [`EventPlan`] — a renderer-agnostic
//! description of what to draw, still in the script's own `PlayResX`/
//! `PlayResY` coordinate space. `crate-filter-subtitle` scales this to real
//! frame pixels and calls `vaco_filter_text::TextRenderer`; nothing here
//! touches a pixel.
//!
//! # Static tags and style transforms (GitHub #487 / #488)
//!
//! Implemented: `\b \i \u \s \fn \fs \fscx \fscy \fsp \frx \fry \frz \fr \bord
//! \xbord \ybord \shad \xshad \yshad \blur \be \c \1c \2c \3c \4c \alpha
//! \1a \2a \3a \4a \an \a \pos \org \clip \r`.
//!
//! `\t(...)` evaluates its supported numeric and colour style tags at a
//! requested event-relative time. The four standard timing/acceleration forms
//! are supported; nested transforms and line-level tags inside a transform are
//! ignored so evaluation remains non-recursive and cannot change placement.
//!
//! # Recognised but not animated (stage (b), GitHub #488 / FT-5.3)
//!
//! `\move(x1,y1,x2,y2[,t1,t2])` uses `(x1, y1)` as a static `\pos`,
//! ignoring the motion. `\fad`/`\fade` are parsed and ignored — the event
//! renders at full opacity for its whole span rather than fading. `\k`/
//! `\kf`/`\ko`/`\K` retain centisecond syllable intervals and `\p<n>`
//! retains drawing-command text, its scale, and `\pbo` baseline offset.
//! `\fax`/`\fay` (shear) are parsed and
//! ignored. `\frx`/`\fry`/`\frz`/`\fr` carry static 3-D Euler angles on
//! each run, and `\org` carries their optional line origin for the
//! downstream subtitle renderer.
//!
//! Every one of these is a real, named gap — not a silent guess.

use vaco_core::{Duration, Rgba};

use crate::script::{Event, Script};
use crate::style::Style;
use crate::tags::{Item, tokenize};

#[derive(Debug, Clone, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one flag per independent glyph-formatting override tag, not a state machine"
)]
pub struct ResolvedStyle {
    pub fontname: String,
    pub fontsize: f64,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikeout: bool,
    pub primary: Rgba,
    /// The pre-highlight fill colour used by ASS karaoke syllables.
    pub secondary: Rgba,
    pub outline_colour: Rgba,
    pub back_colour: Rgba,
    pub scale_x: f64,
    pub scale_y: f64,
    pub spacing: f64,
    /// X-axis rotation in degrees (`\frx`).
    pub angle_x: f64,
    /// Y-axis rotation in degrees (`\fry`).
    pub angle_y: f64,
    /// Z-axis rotation in degrees (`\frz`/`\fr`).
    pub angle_z: f64,
    pub border_style: i32,
    pub outline: f64,
    pub shadow: f64,
    pub blur: f64,
}

impl ResolvedStyle {
    #[must_use]
    pub fn from_style(s: &Style) -> Self {
        Self {
            fontname: s.fontname.clone(),
            fontsize: s.fontsize,
            bold: s.bold,
            italic: s.italic,
            underline: s.underline,
            strikeout: s.strikeout,
            primary: s.primary_colour,
            secondary: s.secondary_colour,
            outline_colour: s.outline_colour,
            back_colour: s.back_colour,
            scale_x: s.scale_x,
            scale_y: s.scale_y,
            spacing: s.spacing,
            angle_x: 0.0,
            angle_y: 0.0,
            angle_z: s.angle,
            border_style: s.border_style,
            outline: s.outline,
            shadow: s.shadow,
            blur: 0.0,
        }
    }
}

/// How a karaoke syllable changes from its secondary to primary appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KaraokeMode {
    /// Change the complete fill as soon as the syllable starts (`\\k`).
    Instant,
    /// Reveal the primary fill from left to right over the syllable (`\\K`/`\\kf`).
    Sweep,
    /// Like [`Self::Instant`], but suppress the pre-highlight outline (`\\ko`).
    Outline,
}

/// One event-relative karaoke interval, expressed in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KaraokeTiming {
    /// The event-relative start time of this syllable.
    pub start_ms: f64,
    /// The duration declared by the tag, converted from centiseconds.
    pub duration_ms: f64,
    /// The visible transition selected by the tag.
    pub mode: KaraokeMode,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextRun {
    pub style: ResolvedStyle,
    pub text: String,
    /// Karaoke timing for this run, if its preceding override tag declared one.
    pub karaoke: Option<KaraokeTiming>,
}

/// One `\\p` drawing payload and the style state that was active when it began.
#[derive(Debug, Clone, PartialEq)]
pub struct DrawingRun {
    /// The raw ASS drawing-command sequence; rasterisers interpret its `m`,
    /// `n`, `l`, and `b` commands in script coordinates.
    pub commands: String,
    /// `\\p`'s power-of-two coordinate divisor.
    pub scale: u32,
    /// The style used for fill, outline, shadow, scaling, and rotation.
    pub style: ResolvedStyle,
    /// `\\pbo`'s script-space Y offset.
    pub baseline_offset: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventPlan {
    pub alignment: i32,
    /// `(x, y)` in script coordinates, from `\pos` or `\move`'s start
    /// point — `None` means "use the style's own alignment/margins", the
    /// reference's normal placement.
    pub pos: Option<(f64, f64)>,
    /// Explicit rotation origin from `\org`, in script coordinates. When
    /// absent, the renderer rotates around the line's aligned position.
    pub origin: Option<(f64, f64)>,
    pub margin_l: i32,
    pub margin_r: i32,
    pub margin_v: i32,
    /// A rectangular clip, `(x1, y1, x2, y2)` in script coordinates
    /// (`\clip`'s 4-argument form only — vector clips are not implemented).
    pub clip: Option<(f64, f64, f64, f64)>,
    pub runs: Vec<TextRun>,
    /// Vector drawings embedded between `\\p<n>` and `\\p0` overrides.
    pub drawings: Vec<DrawingRun>,
}

struct Cursor<'a> {
    script: &'a Script,
    base: Style,
    cur: ResolvedStyle,
    alignment: i32,
    pos: Option<(f64, f64)>,
    origin: Option<(f64, f64)>,
    clip: Option<(f64, f64, f64, f64)>,
    elapsed_ms: f64,
    duration_ms: f64,
    karaoke: Option<KaraokeTiming>,
    karaoke_clock_ms: f64,
    drawing_baseline_offset: f64,
}

/// Interpret `event` at its start time against `script`'s styles.
///
/// This compatibility entry point resolves transforms to their initial state.
/// Renderers with a frame timestamp should call [`plan_event_at`].
#[must_use]
pub fn plan_event(script: &Script, event: &Event) -> EventPlan {
    plan_event_at(script, event, event.start)
}

/// Interpret `event` against `script`'s styles at absolute timestamp `now`.
///
/// ASS transform times are relative to the event start. Times before or after
/// a transform interval clamp to its start or target style respectively.
#[must_use]
pub fn plan_event_at(script: &Script, event: &Event, now: Duration) -> EventPlan {
    let base = script.style(&event.style);
    let elapsed_micros = now.as_micros().saturating_sub(event.start.as_micros());
    let duration_micros = event
        .end
        .as_micros()
        .saturating_sub(event.start.as_micros());
    let mut cursor = Cursor {
        script,
        cur: ResolvedStyle::from_style(&base),
        alignment: base.alignment,
        pos: None,
        origin: None,
        clip: None,
        elapsed_ms: elapsed_micros as f64 / 1_000.0,
        duration_ms: duration_micros.max(0) as f64 / 1_000.0,
        karaoke: None,
        karaoke_clock_ms: 0.0,
        drawing_baseline_offset: 0.0,
        base,
    };
    let mut runs = Vec::new();
    let mut buf = String::new();
    let mut drawing_depth = 0u32;
    let mut drawings = Vec::new();
    let mut drawing_text = String::new();
    let mut drawing_style: Option<(ResolvedStyle, u32, f64)> = None;

    for item in tokenize(&event.text) {
        match item {
            Item::Text(t) => {
                if drawing_depth == 0 {
                    buf.push_str(&t);
                } else {
                    drawing_text.push_str(&t);
                }
            }
            Item::Tag { name, arg } => {
                if !buf.is_empty() {
                    runs.push(TextRun {
                        style: cursor.cur.clone(),
                        text: std::mem::take(&mut buf),
                        karaoke: cursor.karaoke,
                    });
                }
                let was_drawing = drawing_depth != 0;
                let next_drawing = if name == "p" {
                    arg.as_deref()
                        .and_then(|value| value.trim().parse::<u32>().ok())
                        .unwrap_or(0)
                } else {
                    drawing_depth
                };
                if was_drawing
                    && next_drawing == 0
                    && !drawing_text.is_empty()
                    && let Some((style, scale, baseline_offset)) = drawing_style.take()
                {
                    drawings.push(DrawingRun {
                        commands: std::mem::take(&mut drawing_text),
                        scale,
                        style,
                        baseline_offset,
                    });
                }
                apply_tag(&mut cursor, &name, arg.as_deref(), &mut drawing_depth);
                if !was_drawing && drawing_depth != 0 {
                    drawing_style = Some((
                        cursor.cur.clone(),
                        drawing_depth,
                        cursor.drawing_baseline_offset,
                    ));
                }
            }
        }
    }
    if !buf.is_empty() {
        runs.push(TextRun {
            style: cursor.cur.clone(),
            text: buf,
            karaoke: cursor.karaoke,
        });
    }
    if drawing_depth != 0
        && !drawing_text.is_empty()
        && let Some((style, scale, baseline_offset)) = drawing_style
    {
        drawings.push(DrawingRun {
            commands: drawing_text,
            scale,
            style,
            baseline_offset,
        });
    }

    EventPlan {
        alignment: cursor.alignment,
        pos: cursor.pos,
        origin: cursor.origin,
        margin_l: if event.margin_l != 0 {
            event.margin_l
        } else {
            cursor.base.margin_l
        },
        margin_r: if event.margin_r != 0 {
            event.margin_r
        } else {
            cursor.base.margin_r
        },
        margin_v: if event.margin_v != 0 {
            event.margin_v
        } else {
            cursor.base.margin_v
        },
        clip: cursor.clip,
        runs,
        drawings,
    }
}

fn parse_num(s: &str) -> Option<f64> {
    s.trim()
        .trim_start_matches('&')
        .trim_end_matches('&')
        .parse()
        .ok()
}

fn apply_tag(cursor: &mut Cursor<'_>, name: &str, arg: Option<&str>, drawing_depth: &mut u32) {
    let a = arg.unwrap_or("");
    match name {
        "b" => cursor.cur.bold = parse_num(a).unwrap_or(0.0) != 0.0,
        "i" => cursor.cur.italic = parse_num(a).unwrap_or(0.0) != 0.0,
        "u" => cursor.cur.underline = parse_num(a).unwrap_or(0.0) != 0.0,
        "s" => cursor.cur.strikeout = parse_num(a).unwrap_or(0.0) != 0.0,
        "fn" => {
            cursor.cur.fontname = if a.is_empty() {
                cursor.base.fontname.clone()
            } else {
                a.to_owned()
            }
        }
        "fs" => {
            if let Some(rel) = a.strip_prefix('+') {
                cursor.cur.fontsize += parse_num(rel).unwrap_or(0.0);
            } else if let Some(rel) = a.strip_prefix('-') {
                cursor.cur.fontsize -= parse_num(rel).unwrap_or(0.0);
            } else if let Some(v) = parse_num(a) {
                cursor.cur.fontsize = v;
            }
        }
        "fscx" => cursor.cur.scale_x = parse_num(a).unwrap_or(cursor.cur.scale_x),
        "fscy" => cursor.cur.scale_y = parse_num(a).unwrap_or(cursor.cur.scale_y),
        "fsp" => cursor.cur.spacing = parse_num(a).unwrap_or(cursor.cur.spacing),
        "frx" => cursor.cur.angle_x = parse_num(a).unwrap_or(cursor.cur.angle_x),
        "fry" => cursor.cur.angle_y = parse_num(a).unwrap_or(cursor.cur.angle_y),
        "frz" | "fr" => cursor.cur.angle_z = parse_num(a).unwrap_or(cursor.cur.angle_z),
        "bord" => {
            let v = parse_num(a).unwrap_or(cursor.cur.outline);
            cursor.cur.outline = v;
        }
        "xbord" | "ybord" => cursor.cur.outline = parse_num(a).unwrap_or(cursor.cur.outline),
        "shad" | "xshad" | "yshad" => cursor.cur.shadow = parse_num(a).unwrap_or(cursor.cur.shadow),
        "blur" | "be" => cursor.cur.blur = parse_num(a).unwrap_or(cursor.cur.blur),
        "c" | "1c" => {
            cursor.cur.primary = crate::color::parse_color(a).unwrap_or(cursor.cur.primary);
        }
        "2c" => {
            cursor.cur.secondary = crate::color::parse_color(a).unwrap_or(cursor.cur.secondary);
        }
        "3c" => {
            cursor.cur.outline_colour =
                crate::color::parse_color(a).unwrap_or(cursor.cur.outline_colour);
        }
        "4c" => {
            cursor.cur.back_colour = crate::color::parse_color(a).unwrap_or(cursor.cur.back_colour);
        }
        "alpha" => {
            if let Some(av) = crate::color::parse_alpha_only(a) {
                cursor.cur.primary.a = av;
                cursor.cur.outline_colour.a = av;
                cursor.cur.back_colour.a = av;
            }
        }
        "1a" => {
            if let Some(av) = crate::color::parse_alpha_only(a) {
                cursor.cur.primary.a = av;
            }
        }
        "2a" => {
            if let Some(av) = crate::color::parse_alpha_only(a) {
                cursor.cur.secondary.a = av;
            }
        }
        "3a" => {
            if let Some(av) = crate::color::parse_alpha_only(a) {
                cursor.cur.outline_colour.a = av;
            }
        }
        "4a" => {
            if let Some(av) = crate::color::parse_alpha_only(a) {
                cursor.cur.back_colour.a = av;
            }
        }
        "an" => {
            if let Some(v) = a.trim().parse::<i32>().ok().filter(|v| (1..=9).contains(v)) {
                cursor.alignment = v;
            }
        }
        "a" => {
            if let Ok(v) = a.trim().parse::<i32>() {
                cursor.alignment = legacy_alignment(v).unwrap_or(cursor.alignment);
            }
        }
        "pos" => {
            if let Some((x, y)) = parse_pair(a) {
                cursor.pos = Some((x, y));
            }
        }
        "move" => {
            let nums: Vec<f64> = a.split(',').filter_map(parse_num).collect();
            if let (Some(&x), Some(&y)) = (nums.first(), nums.get(1)) {
                cursor.pos = Some((x, y));
            }
        }
        "org" => {
            if let Some((x, y)) = parse_pair(a) {
                cursor.origin = Some((x, y));
            }
        }
        "clip" => {
            let nums: Vec<f64> = a.split(',').filter_map(parse_num).collect();
            if let [x1, y1, x2, y2] = nums.as_slice() {
                cursor.clip = Some((*x1, *y1, *x2, *y2));
            }
            // The vector-clip form (a scale plus drawing commands) is not
            // implemented, same reasoning as `\p`.
        }
        "p" => {
            let level: u32 = a.trim().parse().unwrap_or(0);
            *drawing_depth = level;
        }
        "pbo" => {
            cursor.drawing_baseline_offset = parse_num(a).unwrap_or(cursor.drawing_baseline_offset);
        }
        "k" | "K" | "kf" | "ko" => {
            let mode = match name {
                "K" | "kf" => KaraokeMode::Sweep,
                "ko" => KaraokeMode::Outline,
                _ => KaraokeMode::Instant,
            };
            let duration_ms = parse_num(a)
                .filter(|value| value.is_finite() && *value >= 0.0)
                .map_or(0.0, |centiseconds| centiseconds * 10.0);
            cursor.karaoke = Some(KaraokeTiming {
                start_ms: cursor.karaoke_clock_ms,
                duration_ms,
                mode,
            });
            cursor.karaoke_clock_ms += duration_ms;
        }
        "t" => {
            apply_transform(cursor, a);
        }
        // `\fad`/`\fade` (parsed, not applied) and `\k`/`\kf`/`\ko` (karaoke
        // timing, not applied) fall through to the wildcard arm below.
        "r" => {
            let target = if a.is_empty() {
                cursor.base.clone()
            } else {
                cursor.script.style(a)
            };
            cursor.cur = ResolvedStyle::from_style(&target);
        }
        _ => {}
    }
}

#[derive(Debug, Clone, Copy)]
struct TransformTiming {
    start_ms: f64,
    end_ms: f64,
    acceleration: f64,
}

fn apply_transform(cursor: &mut Cursor<'_>, argument: &str) {
    let Some(modifier_start) = argument.find('\\') else {
        return;
    };
    let (timing_text, modifiers) = argument.split_at(modifier_start);
    let Some(timing) = parse_transform_timing(timing_text, cursor.duration_ms) else {
        return;
    };
    let Some(progress) = transform_progress(timing, cursor.elapsed_ms) else {
        return;
    };

    let start = cursor.cur.clone();
    let mut target = start.clone();
    for item in tokenize(&format!("{{{modifiers}}}")) {
        if let Item::Tag { name, arg } = item {
            apply_transform_style_tag(&mut target, &name, arg.as_deref());
        }
    }
    cursor.cur = interpolate_style(&start, &target, progress);
}

fn parse_transform_timing(text: &str, duration_ms: f64) -> Option<TransformTiming> {
    let text = text.trim().trim_end_matches(',').trim();
    let mut numbers = [0.0; 3];
    let mut count = 0usize;
    if !text.is_empty() {
        for number in text.split(',') {
            let slot = numbers.get_mut(count)?;
            *slot = parse_num(number)?;
            count += 1;
        }
    }
    let timing = match count {
        0 => TransformTiming {
            start_ms: 0.0,
            end_ms: duration_ms,
            acceleration: 1.0,
        },
        1 => TransformTiming {
            start_ms: 0.0,
            end_ms: duration_ms,
            acceleration: numbers[0],
        },
        2 => TransformTiming {
            start_ms: numbers[0],
            end_ms: numbers[1],
            acceleration: 1.0,
        },
        3 => TransformTiming {
            start_ms: numbers[0],
            end_ms: numbers[1],
            acceleration: numbers[2],
        },
        _ => return None,
    };
    [timing.start_ms, timing.end_ms, timing.acceleration]
        .iter()
        .all(|value| value.is_finite())
        .then_some(timing)
}

fn transform_progress(timing: TransformTiming, elapsed_ms: f64) -> Option<f64> {
    if timing.acceleration <= 0.0 || !elapsed_ms.is_finite() {
        return None;
    }
    let linear = if timing.end_ms <= timing.start_ms {
        if elapsed_ms >= timing.end_ms {
            1.0
        } else {
            0.0
        }
    } else {
        ((elapsed_ms - timing.start_ms) / (timing.end_ms - timing.start_ms)).clamp(0.0, 1.0)
    };
    let accelerated = linear.powf(timing.acceleration);
    accelerated.is_finite().then_some(accelerated)
}

fn apply_transform_style_tag(style: &mut ResolvedStyle, name: &str, arg: Option<&str>) {
    let a = arg.unwrap_or("");
    match name {
        "fs" => {
            if let Some(relative) = a.strip_prefix('+') {
                style.fontsize += parse_num(relative).unwrap_or(0.0);
            } else if let Some(relative) = a.strip_prefix('-') {
                style.fontsize -= parse_num(relative).unwrap_or(0.0);
            } else if let Some(value) = parse_num(a) {
                style.fontsize = value;
            }
        }
        "fscx" => style.scale_x = parse_num(a).unwrap_or(style.scale_x),
        "fscy" => style.scale_y = parse_num(a).unwrap_or(style.scale_y),
        "fsp" => style.spacing = parse_num(a).unwrap_or(style.spacing),
        "frx" => style.angle_x = parse_num(a).unwrap_or(style.angle_x),
        "fry" => style.angle_y = parse_num(a).unwrap_or(style.angle_y),
        "frz" | "fr" => style.angle_z = parse_num(a).unwrap_or(style.angle_z),
        "bord" | "xbord" | "ybord" => {
            style.outline = parse_num(a).unwrap_or(style.outline);
        }
        "shad" | "xshad" | "yshad" => {
            style.shadow = parse_num(a).unwrap_or(style.shadow);
        }
        "blur" | "be" => style.blur = parse_num(a).unwrap_or(style.blur),
        "c" | "1c" => style.primary = crate::color::parse_color(a).unwrap_or(style.primary),
        "2c" => style.secondary = crate::color::parse_color(a).unwrap_or(style.secondary),
        "3c" => {
            style.outline_colour = crate::color::parse_color(a).unwrap_or(style.outline_colour);
        }
        "4c" => style.back_colour = crate::color::parse_color(a).unwrap_or(style.back_colour),
        "alpha" => {
            if let Some(alpha) = crate::color::parse_alpha_only(a) {
                style.primary.a = alpha;
                style.outline_colour.a = alpha;
                style.back_colour.a = alpha;
            }
        }
        "1a" => {
            if let Some(alpha) = crate::color::parse_alpha_only(a) {
                style.primary.a = alpha;
            }
        }
        "2a" => {
            if let Some(alpha) = crate::color::parse_alpha_only(a) {
                style.secondary.a = alpha;
            }
        }
        "3a" => {
            if let Some(alpha) = crate::color::parse_alpha_only(a) {
                style.outline_colour.a = alpha;
            }
        }
        "4a" => {
            if let Some(alpha) = crate::color::parse_alpha_only(a) {
                style.back_colour.a = alpha;
            }
        }
        // Nested transforms and line-level tags deliberately cannot recurse
        // or mutate placement, clipping, drawing mode, or reset state.
        _ => {}
    }
}

fn interpolate_style(
    start: &ResolvedStyle,
    target: &ResolvedStyle,
    progress: f64,
) -> ResolvedStyle {
    let mut result = start.clone();
    result.fontsize = interpolate_number(start.fontsize, target.fontsize, progress);
    result.scale_x = interpolate_number(start.scale_x, target.scale_x, progress);
    result.scale_y = interpolate_number(start.scale_y, target.scale_y, progress);
    result.spacing = interpolate_number(start.spacing, target.spacing, progress);
    result.angle_x = interpolate_number(start.angle_x, target.angle_x, progress);
    result.angle_y = interpolate_number(start.angle_y, target.angle_y, progress);
    result.angle_z = interpolate_number(start.angle_z, target.angle_z, progress);
    result.outline = interpolate_number(start.outline, target.outline, progress);
    result.shadow = interpolate_number(start.shadow, target.shadow, progress);
    result.blur = interpolate_number(start.blur, target.blur, progress);
    result.primary = interpolate_colour(start.primary, target.primary, progress);
    result.secondary = interpolate_colour(start.secondary, target.secondary, progress);
    result.outline_colour =
        interpolate_colour(start.outline_colour, target.outline_colour, progress);
    result.back_colour = interpolate_colour(start.back_colour, target.back_colour, progress);
    result
}

fn interpolate_number(start: f64, target: f64, progress: f64) -> f64 {
    if start.is_finite() && target.is_finite() {
        start + (target - start) * progress
    } else {
        start
    }
}

fn interpolate_colour(start: Rgba, target: Rgba, progress: f64) -> Rgba {
    let channel = |start: u8, target: u8| {
        interpolate_number(f64::from(start), f64::from(target), progress)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Rgba {
        r: channel(start.r, target.r),
        g: channel(start.g, target.g),
        b: channel(start.b, target.b),
        a: channel(start.a, target.a),
    }
}

fn parse_pair(a: &str) -> Option<(f64, f64)> {
    let mut it = a.split(',');
    let x = parse_num(it.next()?)?;
    let y = parse_num(it.next()?)?;
    Some((x, y))
}

/// SSA's legacy 11-value `\a` alignment, mapped to the numpad `\an` code
/// `\an` itself uses (bottom row `1..=3`, `+4` selects top, `+8` selects
/// middle).
const fn legacy_alignment(v: i32) -> Option<i32> {
    Some(match v {
        1 => 1,
        2 => 2,
        3 => 3,
        5 => 7,
        6 => 8,
        7 => 9,
        9 => 4,
        10 => 5,
        11 => 6,
        _ => return None,
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::float_cmp,
    reason = "test code"
)]
mod tests {
    use super::*;
    use crate::script::parse;
    use vaco_core::Duration;

    fn one_event(text: &str) -> (Script, Event) {
        let doc = format!(
            "[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, Bold, Alignment, MarginL, MarginR, MarginV\nStyle: Default,Arial,20,&H00FFFFFF,0,2,10,10,10\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.00,0:00:05.00,Default,,0,0,0,,{text}\n"
        );
        let script = parse(&doc);
        let event = script.events[0].clone();
        (script, event)
    }

    #[test]
    fn plain_text_is_one_run_with_the_style_defaults() {
        let (script, event) = one_event("hello");
        let plan = plan_event(&script, &event);
        assert_eq!(plan.runs.len(), 1);
        assert_eq!(plan.runs[0].text, "hello");
        assert!(!plan.runs[0].style.bold);
    }

    #[test]
    fn bold_tag_starts_a_new_run() {
        let (script, event) = one_event(r"plain{\b1}bold");
        let plan = plan_event(&script, &event);
        assert_eq!(plan.runs.len(), 2);
        assert!(!plan.runs[0].style.bold);
        assert!(plan.runs[1].style.bold);
    }

    #[test]
    fn pos_tag_sets_an_explicit_position() {
        let (script, event) = one_event(r"{\pos(100,200)}x");
        let plan = plan_event(&script, &event);
        assert_eq!(plan.pos, Some((100.0, 200.0)));
    }

    #[test]
    fn org_tag_sets_an_explicit_rotation_origin() {
        let (script, event) = one_event(r"{\org(120,210)}x");
        let plan = plan_event(&script, &event);
        assert_eq!(plan.origin, Some((120.0, 210.0)));
    }

    #[test]
    fn frx_and_fry_are_resolved_and_reset() {
        let (script, event) = one_event(r"{\frx30\fry-45\fr15}tilt{\r}flat");
        let plan = plan_event(&script, &event);

        assert_eq!(plan.runs[0].style.angle_x, 30.0);
        assert_eq!(plan.runs[0].style.angle_y, -45.0);
        assert_eq!(plan.runs[0].style.angle_z, 15.0);
        assert_eq!(plan.runs[1].style.angle_x, 0.0);
        assert_eq!(plan.runs[1].style.angle_y, 0.0);
        assert_eq!(plan.runs[1].style.angle_z, 0.0);
    }

    #[test]
    fn an_alignment_overrides_the_style() {
        let (script, event) = one_event(r"{\an7}x");
        let plan = plan_event(&script, &event);
        assert_eq!(plan.alignment, 7);
    }

    #[test]
    fn legacy_a_alignment_maps_to_numpad() {
        let (script, event) = one_event(r"{\a6}x");
        let plan = plan_event(&script, &event);
        assert_eq!(plan.alignment, 8);
    }

    #[test]
    fn color_tag_changes_primary_colour() {
        let (script, event) = one_event(r"{\c&H0000FF&}x");
        let plan = plan_event(&script, &event);
        assert_eq!(
            plan.runs[0].style.primary,
            Rgba {
                r: 255,
                g: 0,
                b: 0,
                a: 255
            }
        );
    }

    #[test]
    fn move_uses_its_start_point_as_a_static_position() {
        let (script, event) = one_event(r"{\move(10,20,300,400)}x");
        let plan = plan_event(&script, &event);
        assert_eq!(plan.pos, Some((10.0, 20.0)));
    }

    #[test]
    fn t_tag_four_legal_forms_interpolate_at_event_time() {
        let cases = [
            (r"{\frz0\t(\frz100)}x", 2_500_000, 50.0),
            (r"{\frz0\t(2,\frz100)}x", 2_500_000, 25.0),
            (r"{\frz0\t(1000,3000,\frz100)}x", 2_000_000, 50.0),
            (r"{\frz0\t(1000,3000,2,\frz100)}x", 2_000_000, 25.0),
        ];
        for (text, time_micros, expected) in cases {
            let (script, event) = one_event(text);
            let plan = plan_event_at(&script, &event, Duration::from_micros(time_micros));
            assert_eq!(plan.runs[0].style.angle_z, expected, "{text}");
        }
    }

    #[test]
    fn t_tag_clamps_time_and_steps_across_zero_duration() {
        let (script, event) = one_event(r"{\frz0\t(1000,3000,\frz100)}x");
        let before = plan_event_at(&script, &event, Duration::from_micros(500_000));
        let after = plan_event_at(&script, &event, Duration::from_micros(4_000_000));
        assert_eq!(before.runs[0].style.angle_z, 0.0);
        assert_eq!(after.runs[0].style.angle_z, 100.0);

        let (script, event) = one_event(r"{\frz0\t(1000,1000,\frz100)}x");
        let before = plan_event_at(&script, &event, Duration::from_micros(999_000));
        let at_end = plan_event_at(&script, &event, Duration::from_micros(1_000_000));
        assert_eq!(before.runs[0].style.angle_z, 0.0);
        assert_eq!(at_end.runs[0].style.angle_z, 100.0);
    }

    #[test]
    fn t_tag_invalid_acceleration_keeps_the_snapshot() {
        for acceleration in ["0", "-1", "NaN"] {
            let text = format!(r"{{\frz10\t(0,5000,{acceleration},\frz100)}}x");
            let (script, event) = one_event(&text);
            let plan = plan_event_at(&script, &event, Duration::from_micros(2_500_000));
            assert_eq!(plan.runs[0].style.angle_z, 10.0, "{acceleration}");
        }
    }

    #[test]
    fn t_tag_interpolates_supported_numeric_and_colour_fields() {
        let (script, event) = one_event(
            r"{\t(0,5000,\fs40\fscx200\fscy50\fsp10\frx20\fry40\frz60\bord6\shad10\blur4\1c&H0000FF&\3c&H00FF00&\4c&HFF0000&\alpha&H80&)}x",
        );
        let plan = plan_event_at(&script, &event, Duration::from_micros(2_500_000));
        let style = &plan.runs[0].style;

        assert_eq!(style.fontsize, 30.0);
        assert_eq!(
            (style.scale_x, style.scale_y, style.spacing),
            (150.0, 75.0, 5.0)
        );
        assert_eq!(
            (style.angle_x, style.angle_y, style.angle_z),
            (10.0, 20.0, 30.0)
        );
        assert_eq!((style.outline, style.shadow, style.blur), (4.0, 6.0, 2.0));
        assert_eq!(
            style.primary,
            Rgba {
                r: 255,
                g: 128,
                b: 128,
                a: 191
            }
        );
        assert_eq!(
            style.outline_colour,
            Rgba {
                r: 0,
                g: 128,
                b: 0,
                a: 191
            }
        );
        assert_eq!(
            style.back_colour,
            Rgba {
                r: 0,
                g: 0,
                b: 128,
                a: 191
            }
        );
    }

    #[test]
    fn t_tag_preserves_nested_commas_but_not_line_level_changes() {
        let (script, event) = one_event(r"{\frz0\t(0,5000,\clip(0,0,100,50)\frz100)}x");
        let plan = plan_event_at(&script, &event, Duration::from_micros(2_500_000));
        assert_eq!(plan.runs[0].style.angle_z, 50.0);
        assert_eq!(plan.clip, None);
    }

    #[test]
    fn t_tag_does_not_recurse_and_reset_starts_a_clean_run() {
        let (script, event) = one_event(r"{\frz0\t(0,5000,\frz100\t(0,5000,\frz200))}spin{\r}flat");
        let plan = plan_event_at(&script, &event, Duration::from_micros(2_500_000));
        assert_eq!(plan.runs[0].style.angle_z, 50.0);
        assert_eq!(plan.runs[1].style.angle_z, 0.0);
    }

    #[test]
    fn plan_event_compatibility_wrapper_uses_the_event_start() {
        let (script, event) = one_event(r"{\frz0\t(0,5000,\frz100)}x");
        let plan = plan_event(&script, &event);
        assert_eq!(plan.runs[0].style.angle_z, 0.0);
    }

    #[test]
    fn drawing_mode_suppresses_its_own_text() {
        let (script, event) = one_event(r"before{\p1}m 0 0 l 100 0 100 100{\p0}after");
        let plan = plan_event(&script, &event);
        let joined: String = plan.runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(joined, "beforeafter");
    }

    #[test]
    fn drawing_mode_preserves_commands_scale_and_baseline_offset() {
        let (script, event) = one_event(r"{\pbo-12\p2}m 0 0 l 200 0 200 200{\p0}");
        let plan = plan_event(&script, &event);
        assert_eq!(plan.drawings.len(), 1);
        assert_eq!(plan.drawings[0].scale, 2);
        assert_eq!(plan.drawings[0].baseline_offset, -12.0);
        assert_eq!(plan.drawings[0].commands, "m 0 0 l 200 0 200 200");
    }

    #[test]
    fn karaoke_tags_assign_cumulative_centisecond_intervals_to_syllables() {
        let (script, event) = one_event(r"{\k50}one{\kf25}two{\ko75}three");
        let plan = plan_event(&script, &event);

        assert_eq!(plan.runs.len(), 3);
        assert_eq!(plan.runs[0].karaoke.unwrap().start_ms, 0.0);
        assert_eq!(plan.runs[0].karaoke.unwrap().duration_ms, 500.0);
        assert_eq!(plan.runs[1].karaoke.unwrap().start_ms, 500.0);
        assert_eq!(plan.runs[1].karaoke.unwrap().duration_ms, 250.0);
        assert_eq!(plan.runs[2].karaoke.unwrap().start_ms, 750.0);
        assert_eq!(plan.runs[2].karaoke.unwrap().duration_ms, 750.0);
    }

    #[test]
    fn reset_tag_returns_to_the_named_styles_own_values() {
        let (script, event) = one_event(r"{\b1}bold{\r}plain");
        let plan = plan_event(&script, &event);
        assert!(plan.runs[0].style.bold);
        assert!(!plan.runs[1].style.bold);
    }

    #[test]
    fn clip_rectangle_is_captured() {
        let (script, event) = one_event(r"{\clip(0,0,100,50)}x");
        let plan = plan_event(&script, &event);
        assert_eq!(plan.clip, Some((0.0, 0.0, 100.0, 50.0)));
    }

    #[test]
    fn malformed_tag_arguments_do_not_panic() {
        let (script, event) = one_event(r"{\pos(not,numbers)}{\fs}{\c}x");
        let _ = plan_event(&script, &event);
    }
}
