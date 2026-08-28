//! `telecine` — apply a pulldown pattern, turning a progressive frame
//! sequence into an interlaced one with more frames (`2:3` pulldown by
//! default: 24 frames in, 30 out).
//!
//! `ffmpeg -h filter=telecine`: `first_field` (`top`=0 default, `bottom`=1),
//! `pattern` (a string of digits, default `"23"`).
//!
//! # Measured algorithm
//!
//! Built a `2x4`, `24fps` progressive stream with one distinct value per
//! row per frame (`geq=lum='(Y+1)*10+N'`) and read back which source
//! frame's rows ended up in each of the 30 `telecine` output frames
//! (`ffmpeg` 8.1, 2026-08-23). The result decodes as a single, simple rule:
//!
//! 1. Maintain one continuous **field stream** with strictly alternating
//!    parity, independent of frame boundaries: field `j` is a top field if
//!    `j` is even (when `first_field=top`), bottom if odd.
//! 2. Walk the input frames in order; frame `i` contributes
//!    `pattern[i % len(pattern)]` consecutive fields to that stream, each
//!    one *is* that frame's own top or bottom rows (whichever the running
//!    parity calls for at that position — a frame contributing 3 fields
//!    therefore repeats one of its own two fields, standard `3:2` pulldown).
//! 3. Group the field stream into consecutive pairs and weave each pair
//!    (`crate::video::weave_fields`) into one output frame. Because parity
//!    strictly alternates, every pair has exactly one top and one bottom
//!    field, in either order — this crate's `weave` module documents the
//!    same "role by field-index parity, not pair position" finding.
//!
//! Verified against the raw measurement: with `pattern="23"`, `first_field
//! =top`, frame 0 (a "2"-frame) reconstructs unchanged as output frame 0;
//! frame 1 (a "3"-frame) reconstructs unchanged as output frame 1 *and*
//! contributes its own top field again as the first half of output frame 2
//! (the repeated field); frame 2 (a "2"-frame, continuing parity) then
//! supplies the second half of output frame 2 and the first half of output
//! frame 3. This exact pattern repeats every 4 input / 5 output frames.
//!
//! This module is the byte-exact half of an explicitly required pair —
//! [`crate::detelecine`]'s tests round-trip a synthetic 24 fps source
//! through both and check the frame count and content are recovered.

use std::collections::VecDeque;

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{VIDEO_PAD, extract_field, weave_fields};

pub const DESC: FilterDesc = FilterDesc {
    name: "telecine",
    description: "Apply a telecine pattern.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

/// Parse a pattern string of decimal digits into field counts, falling back
/// to `"23"` (the reference's own default) if the string is empty or has no
/// digits at all.
pub(crate) fn parse_pattern(s: &str) -> Vec<u8> {
    let digits: Vec<u8> = s.chars().filter_map(|c| c.to_digit(10)).map(|d| d as u8).collect();
    if digits.is_empty() { vec![2, 3] } else { digits }
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "telecine", help = "Apply a telecine pattern")]
pub(crate) struct Opts {
    #[opt(
        name = "first_field",
        help = "select first field",
        unit = "first_field",
        consts = crate::opt_consts::FIRST_FIELD_CONSTS,
        default = 0,
        range = 0..=1,
        flags(video, filtering)
    )]
    pub first_field: i32,
    #[opt(name = "pattern", help = "telecine pattern", default = "23".to_string(), flags(video, filtering))]
    pub pattern: String,
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

#[derive(Debug)]
pub(crate) struct Filter {
    pattern: Vec<u8>,
    first_field_top: bool,
    pattern_pos: usize,
    field_index: u64,
    /// Buffered fields, each already extracted to half height, in stream
    /// order.
    pending: VecDeque<Frame>,
}

impl Filter {
    pub(crate) fn new(pattern: Vec<u8>, first_field_top: bool) -> Self {
        Self {
            pattern,
            first_field_top,
            pattern_pos: 0,
            field_index: 0,
            pending: VecDeque::new(),
        }
    }

    /// The field count the current pattern position contributes. `pub(crate)`
    /// so [`crate::detelecine`]'s tests can drive this filter's queue
    /// directly without a `FilterContext`, the same way this crate's other
    /// tests test inner logic rather than the `Filter` trait object.
    pub(crate) fn digit(&self) -> u8 {
        self.pattern
            .get(self.pattern_pos % self.pattern.len().max(1))
            .copied()
            .unwrap_or(2)
    }

    /// Advance the pattern cursor by one input frame.
    pub(crate) fn advance_pattern(&mut self) {
        self.pattern_pos = self.pattern_pos.saturating_add(1);
    }

    /// Weave every complete pair currently buffered.
    pub(crate) fn drain_pairs(&mut self, pool: &vaco_frame::FramePool) -> Result<FrameOut> {
        let mut outs = smallvec::SmallVec::new();
        while self.pending.len() >= 2 {
            let (Some(a), Some(b)) = (self.pending.pop_front(), self.pending.pop_front()) else {
                break;
            };
            // Fields alternate strictly, so of any two consecutive fields
            // in the stream exactly one is top and one is bottom; which one
            // depends only on whether the stream's first field was top or
            // bottom (`self.first_field_top`) and how many fields precede
            // `a`. Rather than track that per buffered field, the fields
            // are pushed onto `pending` already tagged via their own
            // half-height frame's flags (see `push_field`).
            let a_top = crate::video::is_tff(&a);
            let (top, bottom) = if a_top { (&a, &b) } else { (&b, &a) };
            outs.push(weave_fields(pool, top, top, bottom)?);
        }
        Ok(FrameOut::Many(outs))
    }

    /// Extract and buffer one field of `src`, tagged with its own
    /// top/bottom role.
    pub(crate) fn push_field(&mut self, pool: &vaco_frame::FramePool, src: &Frame) -> Result<()> {
        let is_top = self.field_index.is_multiple_of(2) == self.first_field_top;
        self.field_index = self.field_index.saturating_add(1);
        let mut field = extract_field(pool, src, is_top)?;
        field.flags.set(vaco_frame::FrameFlags::TOP_FIELD_FIRST, is_top);
        self.pending.push_back(field);
        Ok(())
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        let digit = self.digit();
        self.advance_pattern();
        for _ in 0..digit {
            self.push_field(ctx.pool(), &input)?;
        }
        self.drain_pairs(ctx.pool())
    }

    fn flush_state(&mut self) {
        self.pattern_pos = 0;
        self.field_index = 0;
        self.pending.clear();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(
            parse_pattern(&opts.pattern),
            opts.first_field == 0,
        ))),
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]
mod tests {
    use super::*;
    use crate::video::test_support::row_value;
    use vaco_frame::FramePool;
    use vaco_pixfmt::PixFmt;

    fn frame_with_rows(pool: &FramePool, h: u32, base: u8) -> Frame {
        let mut f = pool.acquire_video(PixFmt::Gray8, 2, h).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            for y in 0..h as usize {
                if let Some(row) = p.row_mut(y) {
                    row.fill(base.wrapping_add(y as u8));
                }
            }
        }
        f
    }

    #[test]
    fn pattern_23_produces_24_over_4_ratio() {
        // 4 input frames -> 5 output frames, matching the reference's
        // measured 24-in/30-out (24 = 4*6, 30 = 5*6).
        let pool = FramePool::default();
        let mut filt = Filter::new(vec![2, 3], true);
        let inputs: Vec<Frame> = (0..8u8).map(|n| frame_with_rows(&pool, 4, n * 10)).collect();
        let mut total_out = 0;
        for f in inputs {
            // Drive push/drain directly (no FilterContext needed at this level).
            let digit = filt.digit();
            filt.advance_pattern();
            for _ in 0..digit {
                filt.push_field(&pool, &f).unwrap();
            }
            let FrameOut::Many(v) = filt.drain_pairs(&pool).unwrap() else {
                panic!("expected FrameOut::Many")
            };
            total_out += v.len();
        }
        assert_eq!(total_out, 10, "8 input frames over a 2-frame/5-output cycle -> 10 output");
    }

    #[test]
    fn parse_pattern_falls_back_to_23() {
        assert_eq!(parse_pattern(""), vec![2, 3]);
        assert_eq!(parse_pattern("23"), vec![2, 3]);
        assert_eq!(parse_pattern("2332"), vec![2, 3, 3, 2]);
    }

    #[test]
    fn a_2_frame_reconstructs_unchanged() {
        // pattern[0]='2': the first input frame's two fields weave straight
        // back into an unchanged frame (measured: output frame 0 == input frame 0).
        let pool = FramePool::default();
        let mut filt = Filter::new(vec![2, 3], true);
        let f0 = frame_with_rows(&pool, 4, 10);
        let digit = filt.digit();
        filt.advance_pattern();
        for _ in 0..digit {
            filt.push_field(&pool, &f0).unwrap();
        }
        assert_eq!(filt.pending.len(), 2);
        let a = filt.pending.pop_front().unwrap();
        let b = filt.pending.pop_front().unwrap();
        let a_top = crate::video::is_tff(&a);
        let (top, bottom) = if a_top { (&a, &b) } else { (&b, &a) };
        let out = weave_fields(&pool, top, top, bottom).unwrap();
        for y in 0..4 {
            assert_eq!(row_value(&out, y), row_value(&f0, y), "row {y}");
        }
    }

    #[test]
    fn named_first_field_values_parse() {
        for (name, expected) in [("top", 0), ("t", 0), ("bottom", 1), ("b", 1)] {
            let opts = Opts::parse(Some(&format!("first_field={name}"))).unwrap();
            assert_eq!(opts.first_field, expected, "first_field={name}");
        }
    }
}
