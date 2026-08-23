//! `detelecine` — invert a `2:3` (or other) pulldown pattern, recovering the
//! original progressive frame sequence.
//!
//! `ffmpeg -h filter=detelecine`: `first_field` (`top`=0 default,
//! `bottom`=1), `pattern` (default `"23"`), `start_frame` (`0..=13`,
//! position in the pattern cycle of the first frame — for a stream cut
//! mid-cycle).
//!
//! # Algorithm: the exact inverse of `telecine`
//!
//! [`crate::telecine`]'s module doc derives the measured algorithm: a
//! continuous field stream with strictly alternating parity, grouped into
//! consecutive woven pairs, with each input frame's own row-parity
//! (`crate::video::weave_fields`'s `top_field` parameter always lands on
//! *even* output rows) meaning a telecined frame's even rows are always the
//! next unconsumed field in that stream and its odd rows the one after.
//!
//! So detelecine's job is: for each incoming telecined frame, extract its
//! even rows and odd rows as two fields (in that order — the even-row field
//! is always the temporally earlier one), append both to the same kind of
//! field queue `telecine` builds, then reconstruct original frames from it:
//! `pattern[i % len]` fields belong to original frame `i`; the **first
//! two** are woven together (`weave_fields`, ordered by which one is
//! tagged top — the same "role by field-index parity" rule, not by queue
//! position), and any further fields (`pattern[i] - 2`, i.e. the repeated
//! field a `"3"` digit introduced) are discarded.
//!
//! # Independent oracle: round-trip through `telecine`
//!
//! This module's own test constructs a synthetic 24 fps, frame-identifiable
//! source, runs it through `telecine` then `detelecine` with matching
//! options, and checks both the frame count and the row content are
//! recovered exactly — the invariant this row's brief names explicitly.

use std::collections::VecDeque;

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::{Frame, FrameData, FramePool};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::telecine::parse_pattern;
use crate::video::{VIDEO_PAD, extract_field, is_tff, weave_fields};

pub const DESC: FilterDesc = FilterDesc {
    name: "detelecine",
    description: "Apply an inverse telecine pattern.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "detelecine", help = "Apply an inverse telecine pattern")]
pub(crate) struct Opts {
    #[opt(
        name = "first_field",
        help = "select first field",
        default = 0,
        range = 0..=1,
        flags(video, filtering)
    )]
    pub first_field: i32,
    #[opt(name = "pattern", help = "telecine pattern", default = "23".to_string(), flags(video, filtering))]
    pub pattern: String,
    #[opt(
        name = "start_frame",
        help = "position of first frame with respect to the pattern",
        default = 0,
        range = 0..=13,
        flags(video, filtering)
    )]
    pub start_frame: i32,
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
    pattern_pos: usize,
    pending: VecDeque<Frame>,
}

impl Filter {
    pub(crate) fn new(pattern: Vec<u8>, start_frame: usize) -> Self {
        let len = pattern.len().max(1);
        Self {
            pattern,
            pattern_pos: start_frame % len,
            pending: VecDeque::new(),
        }
    }

    fn digit(&self) -> usize {
        usize::from(
            self.pattern
                .get(self.pattern_pos % self.pattern.len().max(1))
                .copied()
                .unwrap_or(2),
        )
    }

    fn push_frame_fields(&mut self, pool: &FramePool, src: &Frame) -> Result<()> {
        // Even rows are always the temporally earlier field (see module
        // doc): push it first, then the odd-row field.
        let mut even = extract_field(pool, src, true)?;
        let mut odd = extract_field(pool, src, false)?;
        even.flags.insert(vaco_frame::FrameFlags::TOP_FIELD_FIRST);
        odd.flags.remove(vaco_frame::FrameFlags::TOP_FIELD_FIRST);
        self.pending.push_back(even);
        self.pending.push_back(odd);
        Ok(())
    }

    fn try_reconstruct(&mut self, pool: &FramePool) -> Result<FrameOut> {
        let mut outs = smallvec::SmallVec::new();
        loop {
            let need = self.digit();
            if need < 2 || self.pending.len() < need {
                break;
            }
            let (Some(a), Some(b)) = (self.pending.pop_front(), self.pending.pop_front()) else {
                break;
            };
            let a_top = is_tff(&a);
            let (top, bottom) = if a_top { (&a, &b) } else { (&b, &a) };
            outs.push(weave_fields(pool, top, top, bottom)?);
            // Discard the repeated field(s) a "3"-or-higher digit introduced.
            for _ in 0..need.saturating_sub(2) {
                self.pending.pop_front();
            }
            self.pattern_pos = self.pattern_pos.saturating_add(1);
        }
        Ok(FrameOut::Many(outs))
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        self.push_frame_fields(ctx.pool(), &input)?;
        self.try_reconstruct(ctx.pool())
    }

    fn flush_state(&mut self) {
        self.pattern_pos = 0;
        self.pending.clear();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    #[allow(
        clippy::cast_sign_loss,
        reason = "start_frame's own range is 0..=13, validated by the option schema"
    )]
    let start_frame = opts.start_frame.max(0) as usize;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(parse_pattern(&opts.pattern), start_frame))),
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
    use crate::telecine;
    use vaco_frame::FramePool;
    use vaco_pixfmt::PixFmt;

    fn frame_with_rows(pool: &FramePool, h: u32, id: u8) -> Frame {
        let mut f = pool.acquire_video(PixFmt::Gray8, 2, h).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            for y in 0..h as usize {
                if let Some(row) = p.row_mut(y) {
                    row.fill(id.wrapping_mul(16).wrapping_add(y as u8));
                }
            }
        }
        f
    }

    /// Drive a `telecine::Filter` purely through its `push_field`/queue
    /// machinery (no `FilterContext` needed, mirroring `telecine`'s own
    /// tests), returning every woven output frame in order.
    fn run_telecine(pool: &FramePool, pattern: Vec<u8>, inputs: &[Frame]) -> Vec<Frame> {
        let mut filt = telecine::Filter::new(pattern, true);
        let mut out = Vec::new();
        for f in inputs {
            let digit = filt.digit();
            filt.advance_pattern();
            for _ in 0..digit {
                filt.push_field(pool, f).unwrap();
            }
            let FrameOut::Many(v) = filt.drain_pairs(pool).unwrap() else {
                panic!("expected FrameOut::Many")
            };
            out.extend(v);
        }
        out
    }

    #[test]
    fn telecine_then_detelecine_recovers_frame_count_and_content() {
        let pool = FramePool::default();
        let pattern = vec![2, 3];
        let inputs: Vec<Frame> = (0..8u8).map(|n| frame_with_rows(&pool, 4, n)).collect();
        let telecined = run_telecine(&pool, pattern.clone(), &inputs);

        let mut det = Filter::new(pattern, 0);
        let mut recovered = Vec::new();
        for f in telecined {
            det.push_frame_fields(&pool, &f).unwrap();
            let FrameOut::Many(v) = det.try_reconstruct(&pool).unwrap() else {
                panic!("expected FrameOut::Many")
            };
            recovered.extend(v);
        }

        assert_eq!(recovered.len(), inputs.len(), "frame count must round-trip");
        for (i, (orig, back)) in inputs.iter().zip(recovered.iter()).enumerate() {
            for y in 0..4 {
                assert_eq!(row_value(back, y), row_value(orig, y), "frame {i} row {y}");
            }
        }
    }
}
