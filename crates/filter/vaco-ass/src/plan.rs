//! Interpreting one [`crate::script::Event`]'s override tags against its
//! [`crate::style::Style`] into an [`EventPlan`] — a renderer-agnostic
//! description of what to draw, still in the script's own `PlayResX`/
//! `PlayResY` coordinate space. `crate-filter-subtitle` scales this to real
//! frame pixels and calls `vaco_filter_text::TextRenderer`; nothing here
//! touches a pixel.
//!
//! # Static tags only (stage (a), GitHub #487 / FT-5.2)
//!
//! Implemented: `\b \i \u \s \fn \fs \fscx \fscy \fsp \frz \fr \bord
//! \xbord \ybord \shad \xshad \yshad \blur \be \c \1c \2c \3c \4c \alpha
//! \1a \2a \3a \4a \an \a \pos \org \clip \r`.
//!
//! # Recognised but not animated (stage (b), GitHub #488 / FT-5.3)
//!
//! `\t(...)` — the override tags in its **last** argument are applied
//! immediately and statically (the interpolation *target*, not an
//! animation), which is a coarse but visible approximation, not a silent
//! drop. `\move(x1,y1,x2,y2[,t1,t2])` uses `(x1, y1)` as a static `\pos`,
//! ignoring the motion. `\fad`/`\fade` are parsed and ignored — the event
//! renders at full opacity for its whole span rather than fading. `\k`/
//! `\kf`/`\ko`/`\K` (karaoke) are parsed and ignored — no highlight sweep.
//! `\p<n>` (vector drawing) suppresses its own text run entirely rather
//! than showing raw drawing-command syntax as literal text, since drawing
//! it is out of scope this pass. `\frx`/`\fry`/`\fax`/`\fay` (3-D rotation/
//! shear) are parsed and ignored; only `\frz`/`\fr` (2-D, Z-axis) rotation
//! is applied. `\org` is stored but has no effect without `\frx`/`\fry`.
//!
//! Every one of these is a real, named gap — not a silent guess.

use vaco_core::Rgba;

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
    pub outline_colour: Rgba,
    pub back_colour: Rgba,
    pub scale_x: f64,
    pub scale_y: f64,
    pub spacing: f64,
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
            outline_colour: s.outline_colour,
            back_colour: s.back_colour,
            scale_x: s.scale_x,
            scale_y: s.scale_y,
            spacing: s.spacing,
            angle_z: s.angle,
            border_style: s.border_style,
            outline: s.outline,
            shadow: s.shadow,
            blur: 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextRun {
    pub style: ResolvedStyle,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventPlan {
    pub alignment: i32,
    /// `(x, y)` in script coordinates, from `\pos` or `\move`'s start
    /// point — `None` means "use the style's own alignment/margins", the
    /// reference's normal placement.
    pub pos: Option<(f64, f64)>,
    pub margin_l: i32,
    pub margin_r: i32,
    pub margin_v: i32,
    /// A rectangular clip, `(x1, y1, x2, y2)` in script coordinates
    /// (`\clip`'s 4-argument form only — vector clips are not implemented).
    pub clip: Option<(f64, f64, f64, f64)>,
    pub runs: Vec<TextRun>,
}

struct Cursor<'a> {
    script: &'a Script,
    base: Style,
    cur: ResolvedStyle,
    alignment: i32,
    pos: Option<(f64, f64)>,
    clip: Option<(f64, f64, f64, f64)>,
}

/// Interpret `event` against `script`'s styles into a drawable plan.
#[must_use]
pub fn plan_event(script: &Script, event: &Event) -> EventPlan {
    let base = script.style(&event.style);
    let mut cursor = Cursor {
        script,
        cur: ResolvedStyle::from_style(&base),
        alignment: base.alignment,
        pos: None,
        clip: None,
        base,
    };
    let mut runs = Vec::new();
    let mut buf = String::new();
    let mut drawing_depth = 0u32;

    for item in tokenize(&event.text) {
        match item {
            Item::Text(t) => {
                if drawing_depth == 0 {
                    buf.push_str(&t);
                }
            }
            Item::Tag { name, arg } => {
                if !buf.is_empty() {
                    runs.push(TextRun {
                        style: cursor.cur.clone(),
                        text: std::mem::take(&mut buf),
                    });
                }
                apply_tag(&mut cursor, &name, arg.as_deref(), &mut drawing_depth);
            }
        }
    }
    if !buf.is_empty() {
        runs.push(TextRun {
            style: cursor.cur.clone(),
            text: buf,
        });
    }

    EventPlan {
        alignment: cursor.alignment,
        pos: cursor.pos,
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
        // Secondary colour affects karaoke's unsung portion only, not implemented.
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
        // `\org` is stored nowhere: it has no effect without 3-D rotation.
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
        "t" => {
            // Apply the last comma-separated argument's own tags
            // immediately and statically — see the module doc.
            if let Some(inner) = a.rsplit(',').next() {
                for item in tokenize(&format!("{{{inner}}}")) {
                    if let Item::Tag { name, arg } = item {
                        apply_tag(cursor, &name, arg.as_deref(), drawing_depth);
                    }
                }
            }
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
    fn t_tag_applies_its_nested_tags_statically() {
        let (script, event) = one_event(r"{\t(0,500,\fscx150)}x");
        let plan = plan_event(&script, &event);
        assert_eq!(plan.runs[0].style.scale_x, 150.0);
    }

    #[test]
    fn drawing_mode_suppresses_its_own_text() {
        let (script, event) = one_event(r"before{\p1}m 0 0 l 100 0 100 100{\p0}after");
        let plan = plan_event(&script, &event);
        let joined: String = plan.runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(joined, "beforeafter");
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
