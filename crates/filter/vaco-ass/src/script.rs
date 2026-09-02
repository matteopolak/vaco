//! ASS/SSA script parsing: `[Script Info]`, `[V4+ Styles]`/`[V4 Styles]`,
//! `[Events]` — stage (a) of GitHub #487 (FT-5.2). Deliberately lenient:
//! subtitle files are untrusted input a real file might have hand-edited
//! into a slightly malformed shape, and a demuxer-style "recover what
//! parses, skip what does not" reads more of them than a strict grammar
//! would (the same reasoning `vaco-format-subtitle`'s own `Cue` model
//! documents for the sibling text-subtitle demuxers).

use vaco_core::Duration;

use crate::style::{Style, parse_style};

#[derive(Debug, Clone, PartialEq)]
pub struct ScriptInfo {
    pub play_res_x: u32,
    pub play_res_y: u32,
    /// `0` smart-wrap (top line wider), `1` end-of-line only (no auto-wrap),
    /// `2` no word-wrapping (`\N` only), `3` smart-wrap (bottom line
    /// wider). This crate implements `\N` as the only line break in every
    /// mode — see `crate::plan`'s own doc for why automatic wrapping is out
    /// of scope this pass.
    pub wrap_style: i32,
    pub scaled_border_and_shadow: bool,
}

impl Default for ScriptInfo {
    fn default() -> Self {
        Self {
            play_res_x: 384,
            play_res_y: 288,
            wrap_style: 0,
            scaled_border_and_shadow: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub layer: i32,
    pub start: Duration,
    pub end: Duration,
    pub style: String,
    pub margin_l: i32,
    pub margin_r: i32,
    pub margin_v: i32,
    /// The raw `Text` field, override tags and all — [`crate::tags`]'s job,
    /// not this module's.
    pub text: String,
    pub is_comment: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Script {
    pub info: ScriptInfo,
    pub styles: Vec<Style>,
    pub events: Vec<Event>,
}

impl Script {
    #[must_use]
    pub fn style(&self, name: &str) -> Style {
        self.styles
            .iter()
            .find(|s| s.name == name)
            .cloned()
            .unwrap_or_default()
    }

    /// Events (in file order) whose `[start, end)` interval contains `t`,
    /// comments excluded.
    pub fn active_at(&self, t: Duration) -> impl Iterator<Item = &Event> {
        self.events.iter().filter(move |e| {
            !e.is_comment
                && e.start.as_micros() <= t.as_micros()
                && t.as_micros() < e.end.as_micros()
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    None,
    ScriptInfo,
    Styles,
    Events,
}

/// Parse a whole `.ass`/`.ssa` script. Never fails: an unrecognised line,
/// section, or field is skipped rather than aborting the parse, so a
/// script with one damaged line still yields every other event.
#[must_use]
pub fn parse(text: &str) -> Script {
    let mut script = Script::default();
    let mut section = Section::None;
    let mut styles_format: Vec<String> = Vec::new();
    let mut events_format: Vec<String> = Vec::new();

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('!') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = match name.to_ascii_lowercase().as_str() {
                "script info" => Section::ScriptInfo,
                "v4+ styles" | "v4 styles" => Section::Styles,
                "events" => Section::Events,
                _ => Section::None,
            };
            continue;
        }
        match section {
            Section::ScriptInfo => parse_info_line(line, &mut script.info),
            Section::Styles => parse_styles_line(line, &mut styles_format, &mut script.styles),
            Section::Events => parse_events_line(line, &mut events_format, &mut script.events),
            Section::None => {}
        }
    }
    script
}

fn split_key_value(line: &str) -> Option<(&str, &str)> {
    line.split_once(':').map(|(k, v)| (k.trim(), v.trim()))
}

fn parse_info_line(line: &str, info: &mut ScriptInfo) {
    let Some((key, value)) = split_key_value(line) else {
        return;
    };
    match key {
        "PlayResX" => info.play_res_x = value.parse().unwrap_or(info.play_res_x),
        "PlayResY" => info.play_res_y = value.parse().unwrap_or(info.play_res_y),
        "WrapStyle" => info.wrap_style = value.parse().unwrap_or(info.wrap_style),
        "ScaledBorderAndShadow" => {
            info.scaled_border_and_shadow = value.eq_ignore_ascii_case("yes");
        }
        _ => {}
    }
}

fn parse_styles_line(line: &str, format: &mut Vec<String>, styles: &mut Vec<Style>) {
    if let Some((key, value)) = split_key_value(line) {
        if key.eq_ignore_ascii_case("Format") {
            *format = value.split(',').map(|s| s.trim().to_owned()).collect();
            return;
        }
        if key.eq_ignore_ascii_case("Style") {
            let cols: Vec<&str> = value.split(',').map(str::trim).collect();
            let format_refs: Vec<&str> = format.iter().map(String::as_str).collect();
            styles.push(parse_style(&cols, &format_refs));
        }
    }
}

fn parse_events_line(line: &str, format: &mut Vec<String>, events: &mut Vec<Event>) {
    let Some((key, value)) = split_key_value(line) else {
        return;
    };
    if key.eq_ignore_ascii_case("Format") {
        *format = value.split(',').map(|s| s.trim().to_owned()).collect();
        return;
    }
    let is_comment = key.eq_ignore_ascii_case("Comment");
    if !is_comment && !key.eq_ignore_ascii_case("Dialogue") {
        return;
    }
    if format.is_empty() {
        return;
    }
    // The `Text` field is the last format column and the only one allowed
    // to contain commas, so split into exactly `format.len()` pieces.
    let cols: Vec<&str> = value.splitn(format.len(), ',').map(str::trim).collect();
    let get = |name: &str| -> Option<&str> {
        format
            .iter()
            .position(|f| f.eq_ignore_ascii_case(name))
            .and_then(|i| cols.get(i).copied())
    };
    let event = Event {
        layer: get("Layer").and_then(|v| v.parse().ok()).unwrap_or(0),
        start: get("Start")
            .and_then(vaco_format_subtitle::time::parse_ass_time)
            .unwrap_or(Duration::ZERO),
        end: get("End")
            .and_then(vaco_format_subtitle::time::parse_ass_time)
            .unwrap_or(Duration::ZERO),
        style: get("Style").unwrap_or("Default").to_owned(),
        margin_l: get("MarginL").and_then(|v| v.parse().ok()).unwrap_or(0),
        margin_r: get("MarginR").and_then(|v| v.parse().ok()).unwrap_or(0),
        margin_v: get("MarginV").and_then(|v| v.parse().ok()).unwrap_or(0),
        text: get("Text").unwrap_or("").to_owned(),
        is_comment,
    };
    events.push(event);
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

    const SAMPLE: &str = r"[Script Info]
PlayResX: 1920
PlayResY: 1080
WrapStyle: 0

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, Bold, Alignment, MarginL, MarginR, MarginV
Style: Default,Arial,48,&H00FFFFFF,0,2,10,10,10

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:01.00,0:00:03.50,Default,,0,0,0,,Hello, world!
Comment: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,not shown
";

    #[test]
    fn parses_script_info() {
        let s = parse(SAMPLE);
        assert_eq!(s.info.play_res_x, 1920);
        assert_eq!(s.info.play_res_y, 1080);
    }

    #[test]
    fn parses_one_style() {
        let s = parse(SAMPLE);
        assert_eq!(s.styles.len(), 1);
        assert_eq!(s.styles[0].fontsize, 48.0);
    }

    #[test]
    fn dialogue_text_keeps_its_internal_comma() {
        let s = parse(SAMPLE);
        let dlg = s.events.iter().find(|e| !e.is_comment).unwrap();
        assert_eq!(dlg.text, "Hello, world!");
    }

    #[test]
    fn comment_lines_are_excluded_from_active_at() {
        let s = parse(SAMPLE);
        let t = Duration::from_micros(500_000);
        assert_eq!(
            s.active_at(t).count(),
            0,
            "the comment line must not render"
        );
    }

    #[test]
    fn active_at_finds_the_dialogue_inside_its_span() {
        let s = parse(SAMPLE);
        let t = Duration::from_micros(2_000_000);
        assert_eq!(s.active_at(t).count(), 1);
    }

    #[test]
    fn garbage_input_does_not_panic_and_yields_an_empty_script() {
        let s = parse("this is not an ass file\nat all{{{\x00\x01");
        assert!(s.events.is_empty());
    }

    #[test]
    fn style_lookup_falls_back_to_default_for_an_unknown_name() {
        let s = parse(SAMPLE);
        assert_eq!(s.style("DoesNotExist"), Style::default());
    }
}
