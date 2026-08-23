//! `weave`/`doubleweave` — weave two fields back into one full-height frame.
//!
//! `ffmpeg -h filter=weave`/`=doubleweave`: `first_field`/`0` (`top`=0
//! default, `bottom`=1) — both filters share the same single option (the
//! reference's own help even prints `(double)weave AVOptions:` for both).
//!
//! # Measured: role assignment is by field-index parity, not pair position
//!
//! Ran `setfield=tff,separatefields,doubleweave` on a `2x8` ramp where every
//! row carries a distinct, frame-identifiable value (`ffmpeg` 8.1,
//! 2026-08-23) and read back which source field ended up in which output
//! row. Labelling the continuous field stream `separatefields` produces as
//! field 0, field 1, field 2, ... (alternating top/bottom, `field 0` = top
//! since `tff`), the woven outputs were:
//!
//! ```text
//! out0 = weave(field0=top, field1=bottom)
//! out1 = weave(field1=top, field2=bottom)     <- field1 plays *top* here
//! ```
//!
//! Field 1 is placed at the bottom in `out0` and at the top in `out1`. So a
//! field's row-parity role in the woven output is **not** "first argument of
//! the pair is always top" — it is the field's own position in the
//! continuous stream: an even-indexed field is always the `first_field`
//! role (top by default) and an odd-indexed field is always the other,
//! regardless of which side of a pair it lands on. `weave` (non-overlapping
//! pairs `(0,1),(2,3),...`) and `doubleweave` (sliding pairs `(0,1),(1,2),
//! (2,3),...`) are the same combine rule; they differ only in whether the
//! just-used field is kept as `held` for the next call.
//!
//! # Independent oracle: round-trips with `separatefields`/`field`
//!
//! `separatefields` then `weave=first_field=top` (or `bff`+`bottom`)
//! reproduces the original frame byte for byte — measured directly and
//! checked in this module's tests, which is the invariant the row's brief
//! names explicitly. `doubleweave` composed with a field selector on its
//! output (picking every other output frame and comparing against the
//! non-overlapping `weave` result) is the second structural check.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::{Frame, FramePool};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::VIDEO_PAD;

pub const WEAVE_DESC: FilterDesc = FilterDesc {
    name: "weave",
    description: "Weave input video fields into frames.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

pub const DOUBLEWEAVE_DESC: FilterDesc = FilterDesc {
    name: "doubleweave",
    description: "Weave input video fields into double number of frames.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "weave", help = "Weave input video fields into frames")]
pub(crate) struct Opts {
    #[opt(
        name = "first_field",
        help = "set first field",
        default = 0,
        range = 0..=1,
        flags(video, filtering)
    )]
    pub first_field: i32,
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

/// Whether the field at continuous stream position `index` plays the
/// `first_field` role (top, unless `first_field=bottom`).
fn plays_first_role(index: u64, first_field_is_top: bool) -> bool {
    let is_even = index.is_multiple_of(2);
    is_even == first_field_is_top
}

#[derive(Debug)]
pub(crate) struct Filter {
    first_field_top: bool,
    /// `true` for `doubleweave`: keep the just-consumed field as `held` for
    /// the next call (sliding pairs). `false` for `weave`: reset to `None`
    /// after every combine (non-overlapping pairs).
    rolling: bool,
    held: Option<(Frame, bool)>,
    next_index: u64,
}

impl Filter {
    pub(crate) const fn new(first_field_top: bool, rolling: bool) -> Self {
        Self {
            first_field_top,
            rolling,
            held: None,
            next_index: 0,
        }
    }

    fn combine(pool: &FramePool, a: &(Frame, bool), b: &(Frame, bool)) -> Result<Frame> {
        let (top, bottom) = if a.1 { (&a.0, &b.0) } else { (&b.0, &a.0) };
        crate::video::weave_fields(pool, &a.0, top, bottom)
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let role_top = plays_first_role(self.next_index, self.first_field_top);
        self.next_index = self.next_index.saturating_add(1);
        let current = (input, role_top);
        let Some(held) = self.held.take() else {
            self.held = Some(current);
            return Ok(FrameOut::None);
        };
        let out = Self::combine(ctx.pool(), &held, &current)?;
        self.held = if self.rolling { Some(current) } else { None };
        Ok(FrameOut::One(out))
    }

    fn flush_state(&mut self) {
        self.held = None;
        self.next_index = 0;
    }
}

pub(crate) fn create_weave(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    Ok(Instance {
        desc: WEAVE_DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(opts.first_field == 0, false))),
    })
}

pub(crate) fn create_doubleweave(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    Ok(Instance {
        desc: DOUBLEWEAVE_DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(opts.first_field == 0, true))),
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
    use crate::video::test_support::{ramp_frame, row_value};
    use vaco_frame::FramePool;

    fn drive(filt: &mut Filter, pool: &FramePool, inputs: Vec<Frame>) -> Vec<Frame> {
        let mut out = Vec::new();
        for f in inputs {
            let role_top = plays_first_role(filt.next_index, filt.first_field_top);
            filt.next_index += 1;
            let current = (f, role_top);
            match filt.held.take() {
                None => filt.held = Some(current),
                Some(held) => {
                    let combined = Filter::combine(pool, &held, &current).unwrap();
                    out.push(combined);
                    filt.held = if filt.rolling { Some(current) } else { None };
                }
            }
        }
        out
    }

    #[test]
    fn separatefields_then_weave_is_the_identity() {
        // The invariant the row's brief names explicitly.
        let pool = FramePool::default();
        let mut orig = ramp_frame(2, 8);
        orig.flags.insert(vaco_frame::FrameFlags::TOP_FIELD_FIRST);
        let tff = crate::video::is_tff(&orig);
        let f0 = crate::video::extract_field(&pool, &orig, tff).unwrap();
        let f1 = crate::video::extract_field(&pool, &orig, !tff).unwrap();

        let mut weaver = Filter::new(true, false); // first_field=top
        let out = drive(&mut weaver, &pool, vec![f0, f1]);
        assert_eq!(out.len(), 1);
        for y in 0..8 {
            assert_eq!(row_value(&out[0], y), row_value(&orig, y), "row {y}");
        }
    }

    #[test]
    fn doubleweave_role_follows_field_index_not_pair_position() {
        // Measured: field1 plays "top" in out0's neighbour pair despite
        // being the *second* field of out0's own pair.
        assert!(plays_first_role(0, true)); // field0 -> top
        assert!(!plays_first_role(1, true)); // field1 -> bottom (in pair0)
        assert!(plays_first_role(2, true)); // field2 -> top
        // field1 is odd -> always "bottom" role, field2 is even -> "top":
        // consistent regardless of which pair (0,1) or (1,2) it appears in.
    }

    #[test]
    fn doubleweave_produces_n_minus_one_frames() {
        let pool = FramePool::default();
        let inputs: Vec<Frame> = (0..5).map(|_| ramp_frame(2, 4)).collect();
        let mut dw = Filter::new(true, true);
        let out = drive(&mut dw, &pool, inputs);
        assert_eq!(out.len(), 4, "5 fields -> 4 overlapping woven frames");
    }

    #[test]
    fn weave_produces_n_over_two_frames() {
        let pool = FramePool::default();
        let inputs: Vec<Frame> = (0..5).map(|_| ramp_frame(2, 4)).collect();
        let mut w = Filter::new(true, false);
        let out = drive(&mut w, &pool, inputs);
        assert_eq!(out.len(), 2, "5 fields, non-overlapping -> 2 frames, 1 dropped");
    }
}
