//! `fieldorder` — change which field of an interlaced frame is considered
//! first, physically shifting the lines by one to match.
//!
//! `ffmpeg -h filter=fieldorder`: `order` (`bff`=0, `tff`=1 default).
//!
//! # Measured: same order is a no-op; different order shifts by one line
//!
//! `2x8` gray-ramp probes (`ffmpeg` 8.1, 2026-08-23):
//!
//! - `setfield=tff,fieldorder=tff` is byte-identical to `setfield=tff` alone
//!   — the invariant this row's brief names explicitly.
//! - `setfield=bff,fieldorder=tff` on rows `[0a,14,1e,28,32,3c,46,50]` gives
//!   `[14,1e,28,32,3c,46,50,46]`: rows shift up by one (`out[i]=orig[i+1]`
//!   for `i<rows-1`), and the new last row is `orig[rows-2]` — **not**
//!   `orig[rows-1]` (which is discarded entirely) and not a duplicate of
//!   `out[rows-2]`'s own *output* value (those happen to be equal here only
//!   because the shift makes them so).
//! - `setfield=tff,fieldorder=bff` gives `[14,0a,14,1e,28,32,3c,46]`: rows
//!   shift down by one (`out[i]=orig[i-1]` for `i>=1`), and the new first
//!   row is `orig[1]` — **not** `orig[0]` (discarded) and not simply
//!   `out[1]`'s value read back (`out[1]=orig[0]`, a different value from
//!   `out[0]=orig[1]`; the two are easy to conflate because they are
//!   adjacent).
//!
//! Both edges are **reflect-101** mirroring of the shifted row index
//! against the original row range (`orig[-1]` reflects to `orig[1]`,
//! `orig[rows]` reflects to `orig[rows-2]` — the boundary row itself is not
//! repeated). That matches broadcast engineering reality rather than being
//! an arbitrary choice: the two fields of an interlaced frame are
//! physically separate scanlines at fixed even/odd row positions, and
//! relabelling which one is "first" without re-encoding motion requires
//! delaying the whole image by one scanline — the row that shifts off one
//! edge is gone, and the row exposed at the other edge is filled by
//! reflecting inward rather than repeating the edge.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::{Frame, FrameData, FramePool};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{VIDEO_PAD, alloc_like, copy_row, dims, ensure_addressable, is_tff};

pub const DESC: FilterDesc = FilterDesc {
    name: "fieldorder",
    description: "Set the field order.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "fieldorder", help = "Set the field order")]
pub(crate) struct Opts {
    #[opt(name = "order", help = "output field order", default = 1, range = 0..=1, flags(video, filtering))]
    pub order: i32,
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

/// Apply the measured shift-by-one-line reorder to `src`, or return `None`
/// when `src` is already in `target_tff` order (a no-op, per the required
/// invariant).
///
/// # Errors
/// Whatever [`crate::video::alloc_like`] reports.
pub(crate) fn reorder(pool: &FramePool, src: &Frame, target_tff: bool) -> Result<Option<Frame>> {
    if is_tff(src) == target_tff {
        return Ok(None);
    }
    let Some((format, width, height)) = dims(src) else {
        return Ok(None);
    };
    ensure_addressable(format)?;
    let mut out = alloc_like(pool, src, format, width, height)?;
    out.flags.set(vaco_frame::FrameFlags::TOP_FIELD_FIRST, target_tff);
    for p in 0..format.plane_count() {
        let rows = format.plane_height(height, p as u8) as usize;
        if rows == 0 {
            continue;
        }
        let Some(src_plane) = src.plane(p) else { continue };
        let Some(mut dst_plane) = out.plane_mut(p) else {
            continue;
        };
        // Reflect-101 the shifted index against `0..rows` (see module doc):
        // `delta=+1` for bff->tff, `delta=-1` for tff->bff. Only one row
        // index can ever fall outside range, since `|delta|==1`.
        let delta: isize = if target_tff { 1 } else { -1 };
        for y in 0..rows {
            #[allow(
                clippy::cast_possible_wrap,
                clippy::cast_sign_loss,
                reason = "rows is a plane row count, far below isize::MAX; the reflect below only ever adjusts by 1"
            )]
            let shifted = y as isize + delta;
            let src_y = if shifted < 0 {
                (-shifted) as usize
            } else if shifted as usize >= rows {
                // Reflect-101 at the upper edge: 2*(rows-1) - shifted.
                (2 * (rows - 1)).saturating_sub(shifted as usize)
            } else {
                shifted as usize
            };
            copy_row(&mut dst_plane, y, src_plane, src_y.min(rows.saturating_sub(1)));
        }
    }
    Ok(Some(out))
}

#[derive(Debug)]
pub(crate) struct Filter {
    target_tff: bool,
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Video { .. } = input.data else {
            return Ok(FrameOut::One(input));
        };
        match reorder(ctx.pool(), &input, self.target_tff)? {
            Some(out) => Ok(FrameOut::One(out)),
            None => Ok(FrameOut::One(input)),
        }
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter {
            target_tff: opts.order == 1,
        })),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use crate::video::test_support::{ramp_frame, row_value};
    use vaco_frame::FramePool;

    #[test]
    fn tff_to_tff_is_a_no_op() {
        // The invariant the row's brief names explicitly.
        let pool = FramePool::default();
        let mut f = ramp_frame(2, 8);
        f.flags.insert(vaco_frame::FrameFlags::TOP_FIELD_FIRST);
        let out = reorder(&pool, &f, true).unwrap();
        assert!(out.is_none(), "same order must be a documented no-op");
    }

    #[test]
    fn bff_to_tff_shifts_up_by_one_measured() {
        // Measured: [0,1,2,3,4,5,6,7] -> [1,2,3,4,5,6,7,6] (reflect-101 at
        // the bottom edge: orig[8] is out of range, reflects to orig[6]).
        let pool = FramePool::default();
        let f = ramp_frame(2, 8); // unmarked = bff, per crate::video::is_tff
        let out = reorder(&pool, &f, true).unwrap().unwrap();
        for y in 0..7 {
            assert_eq!(row_value(&out, y), row_value(&f, y + 1), "row {y}");
        }
        assert_eq!(row_value(&out, 7), 6, "reflect-101: orig[8] -> orig[6]");
    }

    #[test]
    fn tff_to_bff_shifts_down_by_one_measured() {
        // Measured: [0,1,2,3,4,5,6,7] -> [1,0,1,2,3,4,5,6] (reflect-101 at
        // the top edge: orig[-1] is out of range, reflects to orig[1]).
        let pool = FramePool::default();
        let mut f = ramp_frame(2, 8);
        f.flags.insert(vaco_frame::FrameFlags::TOP_FIELD_FIRST);
        let out = reorder(&pool, &f, false).unwrap().unwrap();
        for y in 1..8 {
            assert_eq!(row_value(&out, y), row_value(&f, y - 1), "row {y}");
        }
        assert_eq!(row_value(&out, 0), 1, "reflect-101: orig[-1] -> orig[1]");
    }
}
