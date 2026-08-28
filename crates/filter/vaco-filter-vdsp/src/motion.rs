//! A filter-side block motion search: "find the best match nearby", built by
//! calling `vaco-codec-dsp-mecmp`'s `sad` rather than re-deriving it — see
//! this crate's own doc for why a second SAD implementation is exactly what
//! `cargo xtask dup-check` (D19) exists to catch.
//!
//! # Why this is not `vaco-codec-dsp-me` (#260, D-13)
//!
//! That crate (not yet published when this was written) is the codec
//! encoder's rate-distortion-aware search: it exists to pick a motion vector
//! whose *bit cost* (encoding the vector itself) is weighed against its
//! *prediction quality*, for an encoder that will spend bits either way.
//! `deshake`'s feature tracking and `minterpolate`'s motion field have no
//! bit cost at all — they want the offset that best explains where content
//! moved, full stop — so a plain minimum-SAD full search over a window is
//! both correct and simpler than borrowing an RDO-shaped API for a problem
//! that has no rate term. When `vaco-codec-dsp-me` lands, a filter that
//! specifically wants its diamond/three-step search patterns (for search
//! speed, not correctness) should call it instead of this module.

use vaco_codec_dsp_mecmp::{Plane as MePlane, sad};
use vaco_frame::PlaneRef;

/// A found block match: the offset from the current block's own position to
/// its best match in the reference plane, and the SAD cost at that offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockMatch {
    pub dx: i32,
    pub dy: i32,
    pub cost: u32,
}

fn as_me_plane(p: PlaneRef<'_>) -> MePlane<'_> {
    MePlane::new(p.as_slice(), p.stride(), p.row_bytes(), p.rows())
}

/// Full (exhaustive) search for the `bw × bh` block at `(bx, by)` in `cur`,
/// over every integer offset in `-range..=range` on both axes, against
/// `refp`. Candidates whose reference block would fall outside `refp` are
/// skipped rather than clamped — a partial, edge-clipped block would bias
/// its own SAD low relative to the interior candidates it competes with.
///
/// Returns `(0, 0)` at `u32::MAX` cost if every candidate (including the
/// zero offset) falls outside `refp` — a caller-visible "no match found"
/// rather than a fabricated vector.
#[must_use]
pub fn search_block(cur: PlaneRef<'_>, refp: PlaneRef<'_>, bx: usize, by: usize, bw: usize, bh: usize, range: i32) -> BlockMatch {
    let cur_me = as_me_plane(cur);
    let ref_me = as_me_plane(refp);
    let Some(cur_block) = cur_me.sub(bx, by, bw, bh) else {
        return BlockMatch { dx: 0, dy: 0, cost: u32::MAX };
    };

    let mut best = BlockMatch { dx: 0, dy: 0, cost: u32::MAX };
    for dy in -range..=range {
        for dx in -range..=range {
            #[allow(clippy::cast_possible_wrap, reason = "block coordinates are frame-bounded, far below i64::MAX")]
            let (rx, ry) = (bx as i64 + i64::from(dx), by as i64 + i64::from(dy));
            if rx < 0 || ry < 0 {
                continue;
            }
            #[allow(clippy::cast_sign_loss, reason = "rx, ry >= 0 is checked above")]
            let (rx, ry) = (rx as usize, ry as usize);
            let Some(ref_block) = ref_me.sub(rx, ry, bw, bh) else {
                continue;
            };
            let cost = sad(cur_block, ref_block);
            if cost < best.cost {
                best = BlockMatch { dx, dy, cost };
            }
        }
    }
    best
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_frame::FramePool;
    use vaco_pixfmt::PixFmt;

    fn plane_of(rows: &[&[u8]]) -> vaco_frame::Frame {
        let pool = FramePool::default();
        let h = rows.len() as u32;
        let w = rows.first().map_or(0, |r| r.len()) as u32;
        let mut f = pool.acquire_video(PixFmt::Gray8, w, h).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            for (y, row) in rows.iter().enumerate() {
                if let Some(dst) = p.row_mut(y) {
                    dst[..row.len()].copy_from_slice(row);
                }
            }
        }
        f
    }

    #[test]
    fn identical_frames_find_the_zero_vector_at_zero_cost() {
        let f = plane_of(&[&[1, 2, 3, 4], &[5, 6, 7, 8], &[9, 10, 11, 12], &[13, 14, 15, 16]]);
        let mv = search_block(f.plane(0).unwrap(), f.plane(0).unwrap(), 1, 1, 2, 2, 2);
        assert_eq!(mv, BlockMatch { dx: 0, dy: 0, cost: 0 });
    }

    #[test]
    fn a_shifted_block_is_found_at_the_correct_offset() {
        // A distinctive 2x2 block sits at (1,1) in `cur` and at (2,2) in
        // `refp` (shifted by (+1, +1)); everything else is a flat 0, so the
        // only exact match is the true shift.
        let cur = plane_of(&[
            &[0, 0, 0, 0, 0],
            &[0, 9, 8, 0, 0],
            &[0, 7, 6, 0, 0],
            &[0, 0, 0, 0, 0],
            &[0, 0, 0, 0, 0],
        ]);
        let refp = plane_of(&[
            &[0, 0, 0, 0, 0],
            &[0, 0, 0, 0, 0],
            &[0, 0, 9, 8, 0],
            &[0, 0, 7, 6, 0],
            &[0, 0, 0, 0, 0],
        ]);
        let mv = search_block(cur.plane(0).unwrap(), refp.plane(0).unwrap(), 1, 1, 2, 2, 3);
        assert_eq!(mv, BlockMatch { dx: 1, dy: 1, cost: 0 });
    }

    #[test]
    fn out_of_range_block_position_reports_no_match_rather_than_panicking() {
        let f = plane_of(&[&[1, 2], &[3, 4]]);
        let mv = search_block(f.plane(0).unwrap(), f.plane(0).unwrap(), 10, 10, 2, 2, 1);
        assert_eq!(mv.cost, u32::MAX);
    }
}
