//! Block, super block, and coded-order geometry (`Vaco-Spec-Ref:
//! theora-spec-20170603 section 2.3`).
//!
//! Blocks are numbered in *coded order*: super blocks in raster order (the
//! whole plane, bottom row first, per Theora's right-handed coordinate
//! system), and within each super block a 4x4 Hilbert curve. [`SB_HILBERT`]
//! is exactly the 16-entry curve the spec's own worked example (section 2.3,
//! Figure 2.4 and the accompanying 240x48 example) walks through; it is
//! reproduced here as a literal lookup table rather than a generative
//! algorithm because the spec itself defines it only by that one concrete
//! example, not a general Hilbert-curve construction.
//!
//! Because this crate only decodes intra frames, every block is always
//! coded, so a "coded order" list is simply the traversal order — no
//! `BCODED` filtering step is needed the way an inter-capable decoder would
//! need one.

use vaco_core::Result;
use vaco_limits::Budget;

use crate::ident::PixelFormat;

/// Local `(x, y)` position of each of the 16 Hilbert-curve steps within one
/// 4x4 super block, indexed by coded order.
const SB_HILBERT: [(u32, u32); 16] = [
    (0, 0),
    (1, 0),
    (1, 1),
    (0, 1),
    (0, 2),
    (0, 3),
    (1, 3),
    (1, 2),
    (2, 2),
    (2, 3),
    (3, 3),
    (3, 2),
    (3, 1),
    (2, 1),
    (2, 0),
    (3, 0),
];

/// Coded-order <-> raster geometry for one color plane.
#[derive(Debug, Clone)]
pub(crate) struct PlaneGeom {
    pub blocks_wide: u32,
    pub blocks_tall: u32,
    /// The first coded-order block index (`bi`) belonging to this plane;
    /// blocks are numbered continuously from Y' through Cb to Cr (section
    /// 2.3).
    pub base: u32,
    /// `coded_to_raster[bi - base] == (bx, by)`.
    coded_to_raster: Vec<(u16, u16)>,
    /// `raster_to_coded[by * blocks_wide + bx] == bi`.
    raster_to_coded: Vec<u32>,
}

impl PlaneGeom {
    fn build(blocks_wide: u32, blocks_tall: u32, base: u32, budget: &mut Budget) -> Result<Self> {
        let n = (blocks_wide as usize).saturating_mul(blocks_tall as usize);
        let mut coded_to_raster: Vec<(u16, u16)> = budget.alloc(n)?;
        let mut raster_to_coded: Vec<u32> = budget.alloc(n)?;
        let sb_cols = blocks_wide.div_ceil(4);
        let sb_rows = blocks_tall.div_ceil(4);
        let mut next = 0usize;
        for sb_y in 0..sb_rows {
            for sb_x in 0..sb_cols {
                for &(lx, ly) in &SB_HILBERT {
                    let bx = sb_x.saturating_mul(4).saturating_add(lx);
                    let by = sb_y.saturating_mul(4).saturating_add(ly);
                    if bx >= blocks_wide || by >= blocks_tall {
                        continue;
                    }
                    let bi = base.saturating_add(u32::try_from(next).unwrap_or(u32::MAX));
                    if let Some(slot) = coded_to_raster.get_mut(next) {
                        *slot = (
                            u16::try_from(bx).unwrap_or(u16::MAX),
                            u16::try_from(by).unwrap_or(u16::MAX),
                        );
                    }
                    let raster_idx = (by as usize).saturating_mul(blocks_wide as usize)
                        + bx as usize;
                    if let Some(slot) = raster_to_coded.get_mut(raster_idx) {
                        *slot = bi;
                    }
                    next += 1;
                }
            }
        }
        Ok(Self {
            blocks_wide,
            blocks_tall,
            base,
            coded_to_raster,
            raster_to_coded,
        })
    }

    /// Number of blocks in this plane.
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.coded_to_raster.len()
    }

    #[must_use]
    #[allow(
        dead_code,
        reason = "required by clippy::len_without_is_empty alongside len(); not called by the decode pipeline itself"
    )]
    pub(crate) fn is_empty(&self) -> bool {
        self.coded_to_raster.is_empty()
    }

    /// Raster `(bx, by)` for coded-order index `bi` (already offset by
    /// [`Self::base`]).
    ///
    /// Not called by the decode pipeline (which only ever goes raster ->
    /// coded, via [`Self::coded_of`]) — kept, and exercised by this module's
    /// own round-trip test, because it is the property that proves
    /// [`Self::coded_of`]'s table was built correctly: every raster position
    /// maps to a coded index and back.
    #[must_use]
    #[allow(dead_code, reason = "exercised by this module's own round-trip test")]
    pub(crate) fn raster_of(&self, bi: u32) -> Option<(u32, u32)> {
        let local = bi.checked_sub(self.base)?;
        self.coded_to_raster
            .get(local as usize)
            .map(|&(x, y)| (u32::from(x), u32::from(y)))
    }

    /// Coded-order index of the block at raster `(bx, by)`, or `None` if out
    /// of bounds.
    #[must_use]
    pub(crate) fn coded_of(&self, bx: u32, by: u32) -> Option<u32> {
        if bx >= self.blocks_wide || by >= self.blocks_tall {
            return None;
        }
        let idx = (by as usize).saturating_mul(self.blocks_wide as usize) + bx as usize;
        self.raster_to_coded.get(idx).copied()
    }
}

/// The three planes' geometry plus the frame-wide block count.
#[derive(Debug, Clone)]
pub(crate) struct FrameGeom {
    pub planes: [PlaneGeom; 3],
    pub nbs: u32,
    /// Number of luma blocks — the `bi < NLBS` cutoff section 7.7.3 uses to
    /// pick the luma vs. chroma Huffman table index.
    pub nlbs: u32,
}

impl FrameGeom {
    pub(crate) fn build(fmbw: u32, fmbh: u32, pf: PixelFormat, budget: &mut Budget) -> Result<Self> {
        let (lw, lh) = (fmbw.saturating_mul(2), fmbh.saturating_mul(2));
        let (cw, ch) = pf.chroma_blocks(fmbw, fmbh);
        let y = PlaneGeom::build(lw, lh, 0, budget)?;
        let nlbs = u32::try_from(y.len()).unwrap_or(u32::MAX);
        let cb = PlaneGeom::build(cw, ch, nlbs, budget)?;
        let cb_end = nlbs.saturating_add(u32::try_from(cb.len()).unwrap_or(u32::MAX));
        let cr = PlaneGeom::build(cw, ch, cb_end, budget)?;
        let nbs = cb_end.saturating_add(u32::try_from(cr.len()).unwrap_or(u32::MAX));
        Ok(Self {
            planes: [y, cb, cr],
            nbs,
            nlbs,
        })
    }

    /// Which plane index (0=Y, 1=Cb, 2=Cr) a coded-order `bi` belongs to.
    ///
    /// Not called today — the decode pipeline always knows its current plane
    /// from the loop it is in rather than looking it up from a `bi` — but
    /// kept as the natural counterpart to [`PlaneGeom::coded_of`] and
    /// exercised by this module's own test.
    #[must_use]
    #[allow(dead_code, reason = "exercised by this module's own test")]
    pub(crate) fn plane_of(&self, bi: u32) -> usize {
        let cb_base = self.planes.get(1).map_or(u32::MAX, |p| p.base);
        let cr_base = self.planes.get(2).map_or(u32::MAX, |p| p.base);
        if bi < cb_base {
            0
        } else if bi < cr_base {
            1
        } else {
            2
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    #[test]
    fn matches_the_spec_worked_example_240x48_luma() {
        // Section 2.3's worked example: FMBW=15, FMBH=3 => luma block grid
        // 30x6. The first super block's 16 indices in raster (bx,by) must
        // match the printed table exactly.
        let mut budget = Budget::new(Limits::permissive());
        let geom = PlaneGeom::build(30, 6, 0, &mut budget).unwrap();
        // by=0 (bottom) row, bx=0..3: coded indices 0,1,14,15.
        assert_eq!(geom.coded_of(0, 0), Some(0));
        assert_eq!(geom.coded_of(1, 0), Some(1));
        assert_eq!(geom.coded_of(2, 0), Some(14));
        assert_eq!(geom.coded_of(3, 0), Some(15));
        // by=1: 3,2,13,12.
        assert_eq!(geom.coded_of(0, 1), Some(3));
        assert_eq!(geom.coded_of(1, 1), Some(2));
        assert_eq!(geom.coded_of(2, 1), Some(13));
        assert_eq!(geom.coded_of(3, 1), Some(12));
        // by=2: 4,7,8,11.
        assert_eq!(geom.coded_of(0, 2), Some(4));
        assert_eq!(geom.coded_of(1, 2), Some(7));
        assert_eq!(geom.coded_of(2, 2), Some(8));
        assert_eq!(geom.coded_of(3, 2), Some(11));
        // by=3: 5,6,9,10.
        assert_eq!(geom.coded_of(0, 3), Some(5));
        assert_eq!(geom.coded_of(1, 3), Some(6));
        assert_eq!(geom.coded_of(2, 3), Some(9));
        assert_eq!(geom.coded_of(3, 3), Some(10));
    }

    #[test]
    fn raster_and_coded_are_exact_inverses_over_the_whole_grid() {
        let mut budget = Budget::new(Limits::permissive());
        let geom = PlaneGeom::build(30, 6, 100, &mut budget).unwrap();
        assert_eq!(geom.len(), 30 * 6);
        for by in 0..6 {
            for bx in 0..30 {
                let bi = geom.coded_of(bx, by).unwrap();
                assert_eq!(geom.raster_of(bi), Some((bx, by)));
            }
        }
    }

    #[test]
    fn frame_geom_numbers_planes_continuously() {
        let mut budget = Budget::new(Limits::permissive());
        let geom = FrameGeom::build(2, 2, PixelFormat::Yuv420, &mut budget).unwrap();
        // Luma: 4x4 blocks = 16. Chroma 4:2:0: 2x2 blocks = 4 each.
        assert_eq!(geom.nlbs, 16);
        assert_eq!(geom.nbs, 16 + 4 + 4);
        assert_eq!(geom.plane_of(0), 0);
        assert_eq!(geom.plane_of(15), 0);
        assert_eq!(geom.plane_of(16), 1);
        assert_eq!(geom.plane_of(19), 1);
        assert_eq!(geom.plane_of(20), 2);
        assert_eq!(geom.plane_of(23), 2);
    }
}
