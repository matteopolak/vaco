//! The style knobs [`crate::TextRenderer::layout`] takes, shared by
//! `drawtext` and ASS so neither grows its own copy.

use vaco_filter_draw::Rgba;

#[derive(Debug, Clone, PartialEq)]
pub struct TextStyle {
    /// A `font=`-style name, resolved through [`crate::alias::resolve_family`].
    pub family: String,
    /// An exact file, bypassing family resolution entirely (`fontfile=`).
    pub fontfile: Option<std::path::PathBuf>,
    pub size_px: f32,
    pub bold: bool,
    pub italic: bool,
    pub color: Rgba,
    /// Line spacing added on top of the font's own metrics, in pixels.
    pub line_spacing: f32,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            family: "sans-serif".to_owned(),
            fontfile: None,
            size_px: 16.0,
            bold: false,
            italic: false,
            color: Rgba {
                r: 255,
                g: 255,
                b: 255,
                a: 255,
            },
            line_spacing: 0.0,
        }
    }
}

/// Where a laid-out block wraps, and to what width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Wrap {
    /// No wrapping: one line, however wide.
    None,
    /// Wrap at word boundaries once a line would exceed `max_width` pixels.
    Word(f32),
}

/// A nine-point anchor (ASS's `\an1`-`\an9`, numpad layout: 7/8/9 top,
/// 4/5/6 middle, 1/2/3 bottom, left/centre/right within each row) used to
/// turn a layout's own bounding box into a top-left draw position given a
/// target point. `drawtext`'s `x`/`y` are always top-left, i.e. anchor 7.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    BottomLeft,
    BottomCenter,
    BottomRight,
    MiddleLeft,
    MiddleCenter,
    MiddleRight,
    TopLeft,
    TopCenter,
    TopRight,
}

impl Anchor {
    /// ASS's `\an` numeric code (numpad layout), clamping anything outside
    /// `1..=9` to `2` (bottom-centre — libass's own default alignment).
    #[must_use]
    pub const fn from_ass_code(code: i32) -> Self {
        match code {
            1 => Self::BottomLeft,
            3 => Self::BottomRight,
            4 => Self::MiddleLeft,
            5 => Self::MiddleCenter,
            6 => Self::MiddleRight,
            7 => Self::TopLeft,
            8 => Self::TopCenter,
            9 => Self::TopRight,
            _ => Self::BottomCenter,
        }
    }

    /// Top-left draw position for a `w x h` box so that this anchor point of
    /// the box lands on `(target_x, target_y)`.
    #[must_use]
    pub fn place(self, target_x: f32, target_y: f32, w: f32, h: f32) -> (f32, f32) {
        let (fx, fy) = match self {
            Self::BottomLeft => (0.0, 1.0),
            Self::BottomCenter => (0.5, 1.0),
            Self::BottomRight => (1.0, 1.0),
            Self::MiddleLeft => (0.0, 0.5),
            Self::MiddleCenter => (0.5, 0.5),
            Self::MiddleRight => (1.0, 0.5),
            Self::TopLeft => (0.0, 0.0),
            Self::TopCenter => (0.5, 0.0),
            Self::TopRight => (1.0, 0.0),
        };
        (target_x - fx * w, target_y - fy * h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bottom_center_places_the_box_above_and_centred_on_the_target() {
        let (x, y) = Anchor::BottomCenter.place(100.0, 200.0, 40.0, 10.0);
        assert_eq!((x, y), (80.0, 190.0));
    }

    #[test]
    fn top_left_is_the_identity() {
        let (x, y) = Anchor::TopLeft.place(10.0, 20.0, 40.0, 10.0);
        assert_eq!((x, y), (10.0, 20.0));
    }

    #[test]
    fn out_of_range_ass_codes_degrade_to_bottom_center() {
        assert_eq!(Anchor::from_ass_code(0), Anchor::BottomCenter);
        assert_eq!(Anchor::from_ass_code(42), Anchor::BottomCenter);
        assert_eq!(Anchor::from_ass_code(2), Anchor::BottomCenter);
    }
}
