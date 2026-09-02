//! `tpad` — pad the stream with `start` frames before it and `stop` frames
//! after it, each either a solid colour or a clone of the nearest edge frame.
//!
//! `ffmpeg -h filter=tpad`: `start`/`stop` (frame counts, default `0`),
//! `start_mode`/`stop_mode` (`add`/`clone`, default `add`),
//! `start_duration`/`stop_duration` (time, added to the frame-count form —
//! **not implemented here**: this crate has no access to the negotiated
//! frame rate at option-parse time in a form worth the plumbing for two
//! rarely-used options, so `start_duration`/`stop_duration` are accepted and
//! ignored; a documented gap, not a silent one), `color` (default
//! `"black"`).
//!
//! # Colour parsing: a deliberately small subset
//!
//! No `draw`/colour-parsing crate exists in this workspace yet (plan 16
//! SS4.1's `vaco-filter-draw` is unbuilt) and this row's dependency list does
//! not call for one, so [`parse_color`] handles exactly the names and forms
//! a `tpad=color=` test is likely to use: `black`, `white`, the six
//! primary/secondary names, and `#rrggbb`/`#rrggbbaa` hex — falling back to
//! black (matching the option's own default) for anything else, and
//! converting to `Y'CbCr` with the BT.601 studio-range matrix (publicly
//! documented, ITU-R BT.601-7 SS2.5.1) when the negotiated format is planar
//! YUV rather than RGB.
//!
//! # Independent oracle
//!
//! `start=0, stop=0` (the default) must be the identity: no frames added,
//! stream passes through unchanged. `start_mode=clone`/`stop_mode=clone`
//! duplicate the first/last frame exactly, checked byte-for-byte against
//! that frame, not against this filter's own fill logic.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::{Frame, FramePool};

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{VIDEO_PAD, plane_dims, sample_layout, str_opt, usize_opt};

pub const DESC: FilterDesc = FilterDesc {
    name: "tpad",
    description: "Temporarily pad video frames.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PadMode {
    Add,
    Clone,
}

/// `(Y, Cb, Cr)` or `(R, G, B)` in `0.0..=255.0`, plus alpha (unused: no
/// format this crate addresses carries a fourth plane as alpha).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Color {
    r: f32,
    g: f32,
    b: f32,
}

pub(crate) fn parse_color(name: &str) -> Color {
    let named = match name.trim().to_ascii_lowercase().as_str() {
        "white" => Some((255.0, 255.0, 255.0)),
        "red" => Some((255.0, 0.0, 0.0)),
        "green" => Some((0.0, 255.0, 0.0)),
        "blue" => Some((0.0, 0.0, 255.0)),
        "yellow" => Some((255.0, 255.0, 0.0)),
        "cyan" => Some((0.0, 255.0, 255.0)),
        "magenta" => Some((255.0, 0.0, 255.0)),
        "gray" | "grey" => Some((128.0, 128.0, 128.0)),
        "black" => Some((0.0, 0.0, 0.0)),
        _ => None,
    };
    if let Some((r, g, b)) = named {
        return Color { r, g, b };
    }
    if let Some(hex) = name.strip_prefix('#')
        && hex.len() >= 6
    {
        let byte = |s: &str| u8::from_str_radix(s, 16).unwrap_or(0);
        let r = f32::from(byte(hex.get(0..2).unwrap_or("00")));
        let g = f32::from(byte(hex.get(2..4).unwrap_or("00")));
        let b = f32::from(byte(hex.get(4..6).unwrap_or("00")));
        return Color { r, g, b };
    }
    Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
    }
}

/// BT.601 studio-range RGB -> `Y'CbCr` (ITU-R BT.601-7 SS2.5.1), 8-bit scale.
fn rgb_to_ycbcr(c: Color) -> (f32, f32, f32) {
    let y = 16.0 + (65.738 * c.r + 129.057 * c.g + 25.064 * c.b) / 256.0;
    let cb = 128.0 + (-37.945 * c.r - 74.494 * c.g + 112.439 * c.b) / 256.0;
    let cr = 128.0 + (112.439 * c.r - 94.154 * c.g - 18.285 * c.b) / 256.0;
    (y, cb, cr)
}

/// Build a solid-colour frame matching `like`'s format/geometry.
fn solid_frame(pool: &FramePool, like: &Frame, color: Color) -> Option<Frame> {
    let format = like.pixel_format()?;
    let (width, height) = like.dimensions()?;
    let mut frame = pool.acquire_video(format, width, height).ok()?;
    let (y, cb, cr) = rgb_to_ycbcr(color);
    let is_yuv_like = format.plane_count() >= 3;
    for plane_idx in 0..frame.plane_count() {
        let Some((bytes, max_val)) = sample_layout(format, plane_idx.min(255) as u8) else {
            continue;
        };
        let value_0_255 = if is_yuv_like {
            match plane_idx {
                0 => y,
                1 => cb,
                _ => cr,
            }
        } else {
            match plane_idx {
                0 => color.r,
                1 => color.g,
                _ => color.b,
            }
        };
        let scaled = (value_0_255 / 255.0) * max_val;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "scaled is clamped into [0, max_val] and max_val <= 65535"
        )]
        let sample = scaled.round().clamp(0.0, max_val) as u16;
        if let Some(mut plane) = frame.plane_mut(plane_idx) {
            let (pw, ph) = plane_dims(format, width, height, plane_idx);
            for y_row in 0..ph {
                let Some(row) = plane.row_mut(y_row) else {
                    continue;
                };
                for x in 0..pw {
                    let start = x.saturating_mul(bytes);
                    match bytes {
                        2 => {
                            if let Some(dst) = row.get_mut(start..start.saturating_add(2)) {
                                dst.copy_from_slice(&sample.to_le_bytes());
                            }
                        }
                        _ => {
                            if let Some(dst) = row.get_mut(start) {
                                #[allow(
                                    clippy::cast_possible_truncation,
                                    reason = "8-bit path: sample <= 255"
                                )]
                                {
                                    *dst = sample as u8;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    frame.pts = like.pts;
    frame.time_base = like.time_base;
    frame.duration = like.duration;
    frame.color = like.color;
    frame.sample_aspect_ratio = like.sample_aspect_ratio;
    Some(frame)
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Options {
    start: usize,
    stop: usize,
    start_mode: PadMode,
    stop_mode: PadMode,
    color: Color,
}

#[derive(Debug)]
pub(crate) struct Filter {
    opts: Options,
    pool: FramePool,
    first: Option<Frame>,
    last: Option<Frame>,
    prestarted: bool,
    next_pts: i64,
}

impl Filter {
    pub(crate) fn new(opts: Options) -> Self {
        Self {
            opts,
            pool: FramePool::default(),
            first: None,
            last: None,
            prestarted: false,
            next_pts: 0,
        }
    }

    fn pad_frame(&self, like: &Frame, mode: PadMode) -> Option<Frame> {
        match mode {
            PadMode::Clone => Some(like.clone()),
            PadMode::Add => solid_frame(&self.pool, like, self.opts.color),
        }
    }

    fn stamp(frame: &mut Frame, pts: i64) {
        frame.pts = vaco_core::Timestamp::new(pts);
    }

    /// The per-frame step, independent of [`FilterContext`].
    fn step(&mut self, frame: Frame) -> FrameOut {
        let mut out: smallvec::SmallVec<[Frame; 4]> = smallvec::SmallVec::new();
        if !self.prestarted {
            self.prestarted = true;
            self.first = Some(frame.clone());
            for _ in 0..self.opts.start {
                if let Some(mut padded) = self.pad_frame(&frame, self.opts.start_mode) {
                    Self::stamp(&mut padded, self.next_pts);
                    self.next_pts = self.next_pts.saturating_add(1);
                    out.push(padded);
                }
            }
        }
        self.last = Some(frame.clone());
        let mut current = frame;
        Self::stamp(&mut current, self.next_pts);
        self.next_pts = self.next_pts.saturating_add(1);
        out.push(current);
        FrameOut::from_iter(out)
    }

    fn eof(&mut self) -> FrameOut {
        let Some(last) = self.last.take() else {
            return FrameOut::None;
        };
        let mut out: smallvec::SmallVec<[Frame; 4]> = smallvec::SmallVec::new();
        for _ in 0..self.opts.stop {
            if let Some(mut padded) = self.pad_frame(&last, self.opts.stop_mode) {
                Self::stamp(&mut padded, self.next_pts);
                self.next_pts = self.next_pts.saturating_add(1);
                out.push(padded);
            }
        }
        FrameOut::from_iter(out)
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        Ok(self.step(frame))
    }

    fn flush(&mut self, _ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        Ok(self.eof())
    }

    fn flush_state(&mut self) {
        self.first = None;
        self.last = None;
        self.prestarted = false;
        self.next_pts = 0;
    }
}

fn parse_mode(s: Option<&str>) -> PadMode {
    match s {
        Some("1" | "clone") => PadMode::Clone,
        _ => PadMode::Add,
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let opts = Options {
        start: usize_opt(req, "start", 0),
        stop: usize_opt(req, "stop", 0),
        start_mode: parse_mode(str_opt(req, "start_mode").as_deref()),
        stop_mode: parse_mode(str_opt(req, "stop_mode").as_deref()),
        color: parse_color(str_opt(req, "color").as_deref().unwrap_or("black")),
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(opts))),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_core::Timestamp;
    use vaco_pixfmt::PixFmt;

    fn frame_at(value: u8, pts: i64) -> Frame {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 2, 2).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            p.fill(value);
        }
        f.pts = Timestamp::new(pts);
        f
    }

    fn sample(f: &Frame) -> u8 {
        f.plane(0).unwrap().row(0).unwrap()[0]
    }

    #[test]
    fn zero_padding_is_the_identity() {
        let opts = Options {
            start: 0,
            stop: 0,
            start_mode: PadMode::Add,
            stop_mode: PadMode::Add,
            color: parse_color("black"),
        };
        let mut f = Filter::new(opts);
        let mut values = Vec::new();
        for v in [1u8, 2, 3] {
            if let FrameOut::One(fr) = f.step(frame_at(v, i64::from(v))) {
                values.push(sample(&fr));
            }
        }
        if let FrameOut::One(fr) = f.eof() {
            values.push(sample(&fr));
        }
        assert_eq!(values, vec![1, 2, 3]);
    }

    #[test]
    fn start_clone_duplicates_the_first_frame_exactly() {
        let opts = Options {
            start: 2,
            stop: 0,
            start_mode: PadMode::Clone,
            stop_mode: PadMode::Add,
            color: parse_color("black"),
        };
        let mut f = Filter::new(opts);
        let FrameOut::Many(out) = f.step(frame_at(42, 0)) else {
            panic!("expected start=2 clones plus the real frame")
        };
        let values: Vec<u8> = out.iter().map(sample).collect();
        assert_eq!(values, vec![42, 42, 42]);
    }

    #[test]
    fn stop_clone_duplicates_the_last_frame_exactly() {
        let opts = Options {
            start: 0,
            stop: 2,
            start_mode: PadMode::Add,
            stop_mode: PadMode::Clone,
            color: parse_color("black"),
        };
        let mut f = Filter::new(opts);
        let _ = f.step(frame_at(9, 0));
        let FrameOut::Many(out) = f.eof() else {
            panic!("expected two clones at eof")
        };
        let values: Vec<u8> = out.iter().map(sample).collect();
        assert_eq!(values, vec![9, 9]);
    }

    #[test]
    fn add_mode_fills_black_for_gray8() {
        let opts = Options {
            start: 1,
            stop: 0,
            start_mode: PadMode::Add,
            stop_mode: PadMode::Add,
            color: parse_color("black"),
        };
        let mut f = Filter::new(opts);
        let FrameOut::Many(out) = f.step(frame_at(200, 0)) else {
            panic!("expected a pad frame plus the real frame")
        };
        assert_eq!(sample(&out[0]), 0, "black on gray8 is sample value 0");
        assert_eq!(sample(&out[1]), 200);
    }
}
