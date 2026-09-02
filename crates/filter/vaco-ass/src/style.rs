//! `[V4+ Styles]` (and the legacy `[V4 Styles]`, same shape minus
//! `ScaleX`/`ScaleY`/`Angle`/`Encoding`'s v4 defaults): one named,
//! reusable formatting preset every `Dialogue:` line references by name.

use vaco_core::Rgba;

#[derive(Debug, Clone, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the V4+ style format's own four independent boolean fields, not a state machine"
)]
pub struct Style {
    pub name: String,
    pub fontname: String,
    pub fontsize: f64,
    pub primary_colour: Rgba,
    pub secondary_colour: Rgba,
    pub outline_colour: Rgba,
    pub back_colour: Rgba,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikeout: bool,
    pub scale_x: f64,
    pub scale_y: f64,
    pub spacing: f64,
    pub angle: f64,
    /// `1` = outline + drop shadow, `3` = opaque box.
    pub border_style: i32,
    pub outline: f64,
    pub shadow: f64,
    /// The numpad `\an` alignment code (`1..=9`).
    pub alignment: i32,
    pub margin_l: i32,
    pub margin_r: i32,
    pub margin_v: i32,
}

impl Default for Style {
    /// libass's own documented defaults for a script with no `Default`
    /// style entry — real files always define their own, so this mainly
    /// matters as the base a malformed `Style:` line's missing fields fall
    /// back to.
    fn default() -> Self {
        Self {
            name: "Default".to_owned(),
            fontname: "Arial".to_owned(),
            fontsize: 20.0,
            primary_colour: Rgba {
                r: 255,
                g: 255,
                b: 255,
                a: 255,
            },
            secondary_colour: Rgba {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            },
            outline_colour: Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 255,
            },
            back_colour: Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 255,
            },
            bold: false,
            italic: false,
            underline: false,
            strikeout: false,
            scale_x: 100.0,
            scale_y: 100.0,
            spacing: 0.0,
            angle: 0.0,
            border_style: 1,
            outline: 2.0,
            shadow: 2.0,
            alignment: 2,
            margin_l: 10,
            margin_r: 10,
            margin_v: 10,
        }
    }
}

fn field<'a>(cols: &[&'a str], format: &[&str], name: &str) -> Option<&'a str> {
    format
        .iter()
        .position(|f| f.eq_ignore_ascii_case(name))
        .and_then(|i| cols.get(i).copied())
}

fn parse_bool_flag(s: &str) -> bool {
    // ASS booleans are `-1`/`1` for true, `0` for false (a signed C `BOOL`,
    // where any nonzero value is true) — `-1` is what real files write.
    s.trim().parse::<i64>().is_ok_and(|v| v != 0)
}

fn parse_f64(s: &str, default: f64) -> f64 {
    s.trim().parse().unwrap_or(default)
}

fn parse_i32(s: &str, default: i32) -> i32 {
    s.trim().parse().unwrap_or(default)
}

/// Parse one `Style:` line's comma-separated values against the section's
/// own `Format:` column order — required, since `[V4+ Styles]` and the
/// legacy `[V4 Styles]` order columns differently and a script is free to
/// list them in yet another order.
#[must_use]
pub fn parse_style(cols: &[&str], format: &[&str]) -> Style {
    let base = Style::default();
    let get = |name: &str| field(cols, format, name);
    Style {
        name: get("Name").unwrap_or(&base.name).trim().to_owned(),
        fontname: get("Fontname").unwrap_or(&base.fontname).trim().to_owned(),
        fontsize: get("Fontsize").map_or(base.fontsize, |v| parse_f64(v, base.fontsize)),
        primary_colour: get("PrimaryColour")
            .and_then(crate::color::parse_color)
            .unwrap_or(base.primary_colour),
        secondary_colour: get("SecondaryColour")
            .and_then(crate::color::parse_color)
            .unwrap_or(base.secondary_colour),
        outline_colour: get("OutlineColour")
            .and_then(crate::color::parse_color)
            .unwrap_or(base.outline_colour),
        back_colour: get("BackColour")
            .and_then(crate::color::parse_color)
            .unwrap_or(base.back_colour),
        bold: get("Bold").map_or(base.bold, parse_bool_flag),
        italic: get("Italic").map_or(base.italic, parse_bool_flag),
        underline: get("Underline").map_or(base.underline, parse_bool_flag),
        strikeout: get("StrikeOut").map_or(base.strikeout, parse_bool_flag),
        scale_x: get("ScaleX").map_or(base.scale_x, |v| parse_f64(v, base.scale_x)),
        scale_y: get("ScaleY").map_or(base.scale_y, |v| parse_f64(v, base.scale_y)),
        spacing: get("Spacing").map_or(base.spacing, |v| parse_f64(v, base.spacing)),
        angle: get("Angle").map_or(base.angle, |v| parse_f64(v, base.angle)),
        border_style: get("BorderStyle")
            .map_or(base.border_style, |v| parse_i32(v, base.border_style)),
        outline: get("Outline").map_or(base.outline, |v| parse_f64(v, base.outline)),
        shadow: get("Shadow").map_or(base.shadow, |v| parse_f64(v, base.shadow)),
        alignment: get("Alignment").map_or(base.alignment, |v| parse_i32(v, base.alignment)),
        margin_l: get("MarginL").map_or(base.margin_l, |v| parse_i32(v, base.margin_l)),
        margin_r: get("MarginR").map_or(base.margin_r, |v| parse_i32(v, base.margin_r)),
        margin_v: get("MarginV").map_or(base.margin_v, |v| parse_i32(v, base.margin_v)),
    }
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

    #[test]
    fn parses_a_typical_v4_plus_style_line() {
        let format = [
            "Name",
            "Fontname",
            "Fontsize",
            "PrimaryColour",
            "Bold",
            "Alignment",
            "MarginL",
            "MarginR",
            "MarginV",
        ];
        let cols = [
            "Default",
            "Arial",
            "24",
            "&H00FFFFFF",
            "-1",
            "2",
            "10",
            "10",
            "10",
        ];
        let style = parse_style(&cols, &format);
        assert_eq!(style.name, "Default");
        assert_eq!(style.fontsize, 24.0);
        assert!(style.bold);
        assert_eq!(style.alignment, 2);
        assert_eq!(
            style.primary_colour,
            Rgba {
                r: 255,
                g: 255,
                b: 255,
                a: 255
            }
        );
    }

    #[test]
    fn out_of_order_columns_still_resolve_by_name() {
        let format = ["Fontsize", "Name"];
        let cols = ["42", "MyStyle"];
        let style = parse_style(&cols, &format);
        assert_eq!(style.name, "MyStyle");
        assert_eq!(style.fontsize, 42.0);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let style = parse_style(&[], &[]);
        assert_eq!(style, Style::default());
    }
}
