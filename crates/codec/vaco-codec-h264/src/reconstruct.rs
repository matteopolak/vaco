//! The reconstruction seam: composing [`crate::intra`]'s prediction,
//! [`crate::dequant`]'s scaling, [`crate::scan`]'s inverse zig-zag, and
//! [`vaco_codec_dsp_idct::h264`]'s transforms into actual reconstructed
//! luma samples, macroblock by macroblock, across a whole picture --
//! clause 8.5's own ordered steps for `Intra_16x16` (8.5.2) and
//! `Intra_4x4` (clause 8.3.1's own per-block interleaved
//! predict-then-reconstruct order, via [`PictureBuffer`]'s real,
//! multi-macroblock neighbour propagation).
//!
//! # What this module does
//!
//! [`reconstruct_picture_luma`] walks a decoded
//! [`crate::mb::SliceStats::macroblocks`] list in raster (decode) order,
//! reconstructing each `Intra_16x16` or `Intra_4x4` macroblock's luma
//! plane into a shared [`PictureBuffer`], so every macroblock after the
//! first can draw real, already-reconstructed neighbour samples from
//! whichever macroblock is actually adjacent to it -- not the
//! always-unavailable case [`reconstruct_intra16x16_luma`] alone (still
//! used internally, per macroblock) is limited to on its own.
//!
//! Confirmed byte-exact against real `ffmpeg` on every corpus tried,
//! including mixed `Intra_16x16`/`Intra_4x4` content with real
//! cross-macroblock neighbour propagation between two `Intra_16x16`
//! macroblocks (`cabac_intra_oracle_noise.264`, `_testsrc.264`,
//! `_multi.264`), and, against a fair (undeblocked) reference,
//! `cabac_i_only.264` too.
//!
//! [`crate::deblock::deblock_picture_luma`] now applies a real clause 8.7
//! deblocking filter (scalar, luma-only, all-intra `bS` derivation --
//! see that module's own doc for exactly what is and is not in scope) on
//! top of this module's own reconstruction, closing most but not all of
//! `cabac_i_only.264`'s own gap against `ffmpeg`'s real, deblocked
//! output: 99.78% match, up from 63.77% before that filter existed and
//! 98.97% before a hand-traced, oracle-verified tC0 table correction
//! (`vaco_codec_dsp_deblock::tables`'s own doc), with 18 of 25 whole
//! frames now byte-exact and the remaining mismatches narrowed further
//! -- see
//! `cabac_i_only_reconstructs_without_error_and_mostly_matches_ffmpeg`'s
//! own doc comment for the full account.
//!
//! # What this module does not implement
//!
//! - **Chroma reconstruction.** Every fixture reconstructed so far has
//!   `CodedBlockPatternChroma == 0` (zero chroma residual), so chroma
//!   reconstruction is exactly [`crate::intra`]'s own already-verified
//!   prediction output with nothing added -- clause 8.5.3's chroma
//!   residual path ([`crate::scan::inverse_scan_chroma_dc`]'s
//!   already-tested raster-not-zigzag scan) is written but not yet
//!   composed into a `predC + r` sum here, since nothing on hand
//!   exercises it, and [`reconstruct_picture_luma`] returns luma only.
//! - **`I_PCM`.** Refused with an error rather than attempted -- not
//!   exercised by any fixture on hand.
//! - **Anything beyond one slice == one whole picture.** Every fixture
//!   this module has been run against has exactly this shape (confirmed
//!   structurally, `first_mb_in_slice == 0` on every slice); real
//!   multi-slice-per-picture neighbour-availability handling (clause
//!   6.4.8's "different slice" rule) is not implemented.

#![allow(
    dead_code,
    reason = "exercised by this module's own tests; not yet wired into vaco-codec-h264's own public decode/receive_frame surface"
)]

use vaco_codec_dsp_idct::h264::{idct4x4, idct8x8};
use vaco_codec_core::picture::{BlockScratch, PlaneView};
use vaco_limits::Budget;

use crate::dequant::{chroma_qp, dequant_4x4, dequant_8x8, dequant_chroma_dc_2x2, dequant_luma_dc_4x4};
use crate::intra::{
    Neighbours4, Neighbours8, Neighbours16, NeighboursChroma, predict_intra4x4, predict_intra8x8,
    predict_intra16x16, predict_intra_chroma,
};
use crate::mb::{MbResidual, MbSummary, MvInfo, blk_xy};
use crate::scan::{build_luma_ac_block, inverse_scan_chroma_dc, inverse_scan_luma_8x8, inverse_scan_luma_dc};

/// Clause 8.5.1/8.5.2, `Intra_16x16` luma only: predict, then add clause
/// 8.5.2's own per-4x4-block dequantised-and-transformed residual, then
/// `Clip1` (eq. (8-245); this crate's 8-bit-only scope makes `Clip1`
/// exactly `clamp(0, 255)`).
///
/// `mode` is `Intra16x16PredMode` (Table 8-3), `neighbours` the
/// already-resolved luma neighbour state [`crate::intra`] takes, `qpy`
/// this macroblock's own luma QP (clause 7.4.5, eq. (7-23) -- e.g.
/// [`crate::mb::SliceStats::first_slice_mb_qpy`] for the first macroblock
/// of a real decode), and `residual` this macroblock's own decoded
/// coefficients, still in scan order (e.g.
/// [`crate::mb::SliceStats::first_slice_mb_residual`]).
#[must_use]
#[allow(
    clippy::indexing_slicing,
    reason = "xO/yO are 4*blk_xy(0..16) in 0..12, i/j in 0..4, so every index into the fixed 16x16/16-element arrays below is provably in range -- not bitstream-derived"
)]
pub(crate) fn reconstruct_intra16x16_luma(
    mode: u8,
    neighbours: Neighbours16,
    qpy: i32,
    residual: &MbResidual,
) -> [[u8; 16]; 16] {
    let pred = predict_intra16x16(mode, neighbours);

    // Clause 8.5.2 step 1: the macroblock-wide luma DC transform, shared
    // by all 16 AC blocks below.
    let dc_raw = inverse_scan_luma_dc(residual.luma_dc.as_ref());
    let dc_y = dequant_luma_dc_4x4(&dc_raw, qpy);

    let mut out = pred;
    for blk in 0..16u32 {
        // Clause 8.5.2 step 2a, Figure 8-6: dcY's own (i, j) for this
        // luma4x4BlkIdx is addressed by the same z-order `blk_xy` this
        // crate's neighbour/context derivations already use for the
        // block's own spatial position -- dcY[i][j] at row i = the
        // block's y-in-blocks, column j = the block's x-in-blocks.
        let (bx, by) = blk_xy(blk);
        let dc_val = dc_y[(by * 4 + bx) as usize];

        let ac = residual.luma_ac.get(blk as usize).and_then(Option::as_ref);
        let c = build_luma_ac_block(dc_val, ac);
        // dc_already_scaled = true: position (0, 0) already went through
        // dequant_luma_dc_4x4's own scaling above (clause 8.5.6, not
        // 8.5.8) and must pass through this step untouched.
        let d = dequant_4x4(&c, qpy, true);
        let r = idct4x4(&d);

        let x_o = (bx * 4) as usize;
        let y_o = (by * 4) as usize;
        for i in 0..4usize {
            for j in 0..4usize {
                let p = i32::from(pred[y_o + i][x_o + j]);
                let sum = p + r[i * 4 + j];
                out[y_o + i][x_o + j] = sum.clamp(0, 255) as u8;
            }
        }
    }
    out
}

/// A whole picture's own luma sample buffer, plus the per-4x4-block
/// "has this been reconstructed yet" bitmap `Intra_4x4`'s own neighbour
/// derivation needs -- clause 6.4.7.3/6.4.8's combined effect, for frame
/// (non-MBAFF) pictures, reduces to exactly this: a global 4x4-block grid
/// addressed in absolute picture coordinates, where a position is
/// available iff its owning macroblock has already been fully
/// reconstructed, *or* it is the macroblock currently being reconstructed
/// and this specific 4x4 block was reconstructed earlier in *this*
/// macroblock's own z-order (clause 6.4.3) -- which is exactly what
/// catches clause 8.3.1.2's own "`x` is greater than 3 and `luma4x4BlkIdx`
/// is equal to 3 or 11" special case for free, rather than as a
/// hardcoded exception: block 3's top-right diagonal neighbour and block
/// 11's both resolve, via ordinary `blk_xy` z-order, to a *later* block
/// index in the same macroblock -- "not yet decoded", the general rule,
/// not a special one.
#[derive(Debug)]
struct PictureBuffer {
    mbs_wide: u32,
    mbs_high: u32,
    /// Row-major, `mbs_wide * 16` wide.
    luma: Vec<u8>,
    /// One per global 4x4 luma block position, row-major,
    /// `mbs_wide * 4` wide.
    decoded_4x4: Vec<bool>,
    /// Row-major, `mbs_wide * 8` wide -- `ChromaArrayType == 1` (4:2:0),
    /// this crate's only supported chroma format, halves each dimension.
    cb: Vec<u8>,
    /// Same layout as [`PictureBuffer::cb`].
    cr: Vec<u8>,
    /// One per global 4x4 *chroma* block position, row-major, `mbs_wide *
    /// 2` wide -- coarser than [`PictureBuffer::decoded_4x4`] because a
    /// macroblock's whole 8x8 chroma area (a 2x2 grid of 4x4 blocks) is
    /// always predicted and reconstructed together (clause 8.3.3's
    /// `intra_chroma_pred_mode` is one value per macroblock, not per
    /// 4x4 block the way `Intra_4x4` luma is), so one bitmap serves both
    /// Cb and Cr.
    chroma_decoded: Vec<bool>,
}

impl PictureBuffer {
    /// `budget`-charges exactly the three real sample planes ([`Self::luma`],
    /// [`Self::cb`], [`Self::cr`]) that survive into a
    /// [`ReconstructedPicture`] and, through it, this decoder's DPB --
    /// [`Self::decoded_4x4`]/[`Self::chroma_decoded`] are working
    /// bookkeeping that never outlives this one function's own call (they
    /// are not fields of [`ReconstructedPicture`]), so leaving them
    /// un-budgeted does not reproduce the DPB's own past leak: nothing here persists
    /// past a single `reconstruct_picture` call the way an un-released
    /// budget charge would.
    fn new(mbs_wide: u32, mbs_high: u32, budget: &mut Budget) -> vaco_core::Result<Self> {
        let w = (mbs_wide * 16) as usize;
        let h = (mbs_high * 16) as usize;
        let bw = (mbs_wide * 4) as usize;
        let bh = (mbs_high * 4) as usize;
        let cw = (mbs_wide * 8) as usize;
        let ch = (mbs_high * 8) as usize;
        let cbw = (mbs_wide * 2) as usize;
        let cbh = (mbs_high * 2) as usize;
        let mut luma: Vec<u8> = budget.alloc(w.saturating_mul(h))?;
        luma.fill(128);
        let mut cb: Vec<u8> = budget.alloc(cw.saturating_mul(ch))?;
        cb.fill(128);
        let mut cr: Vec<u8> = budget.alloc(cw.saturating_mul(ch))?;
        cr.fill(128);
        Ok(Self {
            mbs_wide,
            mbs_high,
            luma,
            decoded_4x4: vec![false; bw.saturating_mul(bh)],
            cb,
            cr,
            chroma_decoded: vec![false; cbw.saturating_mul(cbh)],
        })
    }

    const fn width(&self) -> u32 {
        self.mbs_wide * 16
    }

    const fn height(&self) -> u32 {
        self.mbs_high * 16
    }

    /// `true` iff picture pixel `(x, y)` is in bounds *and* its owning 4x4
    /// block has already been written -- the single availability test
    /// every `Intra_4x4` neighbour sample and every `Intra_16x16`
    /// cross-macroblock neighbour row/column both reduce to.
    #[allow(
        clippy::integer_division,
        reason = "x/4, y/4 converts a pixel position to its owning 4x4 block position -- exact by construction (4x4 blocks), not a precision-loss bug"
    )]
    fn available(&self, x: i32, y: i32) -> bool {
        let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
            return false;
        };
        if x >= self.width() || y >= self.height() {
            return false;
        }
        let (bx, by) = (x / 4, y / 4);
        let bw = self.mbs_wide * 4;
        self.decoded_4x4
            .get((by * bw + bx) as usize)
            .copied()
            .unwrap_or(false)
    }

    fn pixel(&self, x: i32, y: i32) -> u8 {
        let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
            return 0;
        };
        if x >= self.width() || y >= self.height() {
            return 0;
        }
        self.luma
            .get((y * self.width() + x) as usize)
            .copied()
            .unwrap_or(0)
    }

    fn set_pixel(&mut self, x: u32, y: u32, v: u8) {
        let w = self.width();
        if let Some(slot) = self.luma.get_mut((y * w + x) as usize) {
            *slot = v;
        }
    }

    /// Writes `row` starting at picture pixel `(x, y)`, one contiguous
    /// `copy_from_slice` rather than `row.len()` separate bound-checked
    /// [`Self::set_pixel`] calls -- every caller ([`Self::write_block4`],
    /// [`Self::write_block8`], and [`reconstruct_picture`]'s own
    /// `Intra_16x16` direct copy) writes a fixed-size block whose every row
    /// is a horizontal run of already-in-bounds picture pixels (macroblock
    /// coordinates never reach the picture edge from outside, by
    /// construction: `mbs_wide`/`mbs_high` bound every macroblock and every
    /// block within it), so this is a pure mechanical speedup, not a
    /// behaviour change -- an out-of-bounds row (which cannot occur in
    /// practice) is dropped whole here exactly as [`Self::set_pixel`] would
    /// have dropped it pixel by pixel.
    fn write_row_luma(&mut self, x: u32, y: u32, row: &[u8]) {
        let w = self.width() as usize;
        let start = y as usize * w + x as usize;
        if let Some(dst) = self.luma.get_mut(start..start.saturating_add(row.len())) {
            dst.copy_from_slice(row);
        }
    }

    /// Marks the 4x4 block at picture-pixel upper-left `(x, y)` as
    /// reconstructed -- called once that block's own samples are already
    /// written, so a *later* block's neighbour lookup (same macroblock or
    /// a macroblock decoded after this one) sees it as available.
    #[allow(
        clippy::integer_division,
        reason = "x/4, y/4 converts a pixel position to its owning 4x4 block position -- exact by construction (4x4 blocks), not a precision-loss bug"
    )]
    fn mark_block_decoded(&mut self, x: u32, y: u32) {
        let bw = self.mbs_wide * 4;
        let (bx, by) = (x / 4, y / 4);
        if let Some(slot) = self.decoded_4x4.get_mut((by * bw + bx) as usize) {
            *slot = true;
        }
    }

    fn write_block4(&mut self, x: u32, y: u32, block: [[u8; 4]; 4]) {
        for (i, row) in block.iter().enumerate() {
            self.write_row_luma(x, y + i as u32, row);
        }
        self.mark_block_decoded(x, y);
    }

    /// [`Self::write_block4`]'s own 8x8 counterpart -- marks all four 4x4
    /// sub-blocks this 8x8 area covers as decoded (the same granularity
    /// [`Self::decoded_4x4`] already tracks at, so a later `Intra_4x4`
    /// neighbour reading back into an `Intra_8x8`-or-inter-8x8-transform
    /// macroblock's own samples needs no special case).
    fn write_block8(&mut self, x: u32, y: u32, block: [[u8; 8]; 8]) {
        for (i, row) in block.iter().enumerate() {
            self.write_row_luma(x, y + i as u32, row);
        }
        for dy in 0..2u32 {
            for dx in 0..2u32 {
                self.mark_block_decoded(x + dx * 4, y + dy * 4);
            }
        }
    }

    const fn chroma_width(&self) -> u32 {
        self.mbs_wide * 8
    }

    const fn chroma_height(&self) -> u32 {
        self.mbs_high * 8
    }

    /// [`PictureBuffer::available`]'s own chroma-plane counterpart, at
    /// chroma-4x4-block granularity.
    #[allow(
        clippy::integer_division,
        reason = "x/4, y/4 converts a chroma pixel position to its owning 4x4 chroma block position -- exact by construction"
    )]
    fn chroma_available(&self, x: i32, y: i32) -> bool {
        let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
            return false;
        };
        if x >= self.chroma_width() || y >= self.chroma_height() {
            return false;
        }
        let (bx, by) = (x / 4, y / 4);
        let bw = self.mbs_wide * 2;
        self.chroma_decoded
            .get((by * bw + bx) as usize)
            .copied()
            .unwrap_or(false)
    }

    /// `comp == 0` is Cb, `comp == 1` is Cr -- matching
    /// [`crate::mb::MbResidual::chroma_dc`]/`chroma_ac`'s own outer-index
    /// convention throughout this crate.
    fn chroma_pixel(&self, comp: usize, x: i32, y: i32) -> u8 {
        let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
            return 0;
        };
        if x >= self.chroma_width() || y >= self.chroma_height() {
            return 0;
        }
        let plane = if comp == 0 { &self.cb } else { &self.cr };
        plane
            .get((y * self.chroma_width() + x) as usize)
            .copied()
            .unwrap_or(0)
    }

    fn set_chroma_pixel(&mut self, comp: usize, x: u32, y: u32, v: u8) {
        let w = self.chroma_width();
        let plane = if comp == 0 { &mut self.cb } else { &mut self.cr };
        if let Some(slot) = plane.get_mut((y * w + x) as usize) {
            *slot = v;
        }
    }

    /// [`Self::write_row_luma`]'s chroma counterpart -- same "every row is
    /// an in-bounds contiguous run" argument, one component (`Cb`/`Cr`) at
    /// a time.
    fn write_row_chroma(&mut self, comp: usize, x: u32, y: u32, row: &[u8]) {
        let w = self.chroma_width() as usize;
        let plane = if comp == 0 { &mut self.cb } else { &mut self.cr };
        let start = y as usize * w + x as usize;
        if let Some(dst) = plane.get_mut(start..start.saturating_add(row.len())) {
            dst.copy_from_slice(row);
        }
    }

    /// Marks a whole macroblock's 8x8 chroma area (both components share
    /// one bitmap -- see [`PictureBuffer::chroma_decoded`]'s own doc) as
    /// reconstructed.
    fn mark_chroma_mb_decoded(&mut self, mb_x: u32, mb_y: u32) {
        let bw = self.mbs_wide * 2;
        for dy in 0..2u32 {
            for dx in 0..2u32 {
                let (bx, by) = (mb_x * 2 + dx, mb_y * 2 + dy);
                if let Some(slot) = self.chroma_decoded.get_mut((by * bw + bx) as usize) {
                    *slot = true;
                }
            }
        }
    }
}

/// Widens a picture-space `u32` coordinate or a small fixed-bound `usize`
/// array index to the signed space this module's neighbour-availability
/// arithmetic needs (negative means "off the picture edge", clause 6.4.8).
/// `vaco-limits`'s `Limits::max_dimension` bounds every real macroblock
/// coordinate reaching this module to at most a few tens of thousands
/// (enforced in `vaco-parse-h264`'s SPS parsing, `ue_v_max`), far below
/// `i32::MAX`; the saturating fallback exists so this can never wrap if
/// that bound is ever raised, not because it is expected to run --
/// saturating keeps the result positive and past `width()`/`height()`, so
/// `PictureBuffer::available`/`pixel` still correctly treat it as
/// off-picture.
fn coord<T: TryInto<i32>>(v: T) -> i32 {
    v.try_into().unwrap_or(i32::MAX)
}

/// Builds one `Intra_4x4` block's [`crate::intra::Neighbours4`] from real
/// picture state -- clause 8.3.1.2's own 13 neighbouring samples, plus
/// the substitution rule for an unavailable top-right when `p[3,-1]` is
/// itself available.
fn intra4x4_neighbours(buf: &PictureBuffer, x: i32, y: i32) -> Neighbours4 {
    let top_available = (0..4).all(|dx| buf.available(x + dx, y - 1));
    let top = core::array::from_fn(|dx| buf.pixel(x + coord(dx), y - 1));
    let left_available = (0..4).all(|dy| buf.available(x - 1, y + dy));
    let left = core::array::from_fn(|dy| buf.pixel(x - 1, y + coord(dy)));
    let corner_available = buf.available(x - 1, y - 1);
    let corner = if corner_available {
        buf.pixel(x - 1, y - 1)
    } else {
        0
    };

    let top_right_available = (4..8).all(|dx| buf.available(x + dx, y - 1));
    let top_right = if top_right_available {
        core::array::from_fn(|dx| buf.pixel(x + 4 + coord(dx), y - 1))
    } else if top_available {
        // Clause 8.3.1.2's own substitution: p[3,-1]'s value stands in for
        // all four, and they are treated as available from here on.
        [top[3]; 4]
    } else {
        [0; 4]
    };

    Neighbours4 {
        top_available,
        top,
        top_right,
        left_available,
        left,
        corner,
    }
}

/// [`intra4x4_neighbours`]'s own `Intra_8x8` counterpart -- clause
/// 8.3.2.2's 25 neighbouring samples, plus the same top-right substitution
/// rule at 8-sample width instead of 4.
///
/// `i8x8` (0..=3, this macroblock's own `luma8x8BlkIdx`) is needed for one
/// genuinely 8x8-specific rule with no 4x4 equivalent: JM 19.1's
/// `intra8x8_pred_normal.c` forces the top-right neighbour unavailable
/// outright for `luma8x8BlkIdx == 3` (`pix_c.available = pix_c.available
/// && !(ioff == 8 && joff == 8)`, checked in every one of that file's own
/// nine mode functions), regardless of whether this crate's own
/// [`PictureBuffer::available`] would otherwise say yes. It would: block
/// 3's own top-right diagonal, unlike `Intra_4x4`'s luma4x4BlkIdx 3/11
/// special case (which [`PictureBuffer`]'s own doc notes falls out of
/// z-order "for free"), lands on block 1 (top-right quadrant), which *is*
/// already reconstructed by the time block 3 runs -- so without this
/// explicit override, this crate's own general-purpose availability check
/// would silently disagree with the standard here.
fn intra8x8_neighbours(buf: &PictureBuffer, i8x8: u32, x: i32, y: i32) -> Neighbours8 {
    let top_available = (0..8).all(|dx| buf.available(x + dx, y - 1));
    let top = core::array::from_fn(|dx| buf.pixel(x + coord(dx), y - 1));
    let left_available = (0..8).all(|dy| buf.available(x - 1, y + dy));
    let left = core::array::from_fn(|dy| buf.pixel(x - 1, y + coord(dy)));
    let corner_available = buf.available(x - 1, y - 1);
    let corner = if corner_available { buf.pixel(x - 1, y - 1) } else { 0 };

    let top_right_forced_unavailable = i8x8 == 3;
    let top_right_available =
        !top_right_forced_unavailable && (8..16).all(|dx| buf.available(x + dx, y - 1));
    let top_right = if top_right_available {
        core::array::from_fn(|dx| buf.pixel(x + 8 + coord(dx), y - 1))
    } else if top_available {
        // Clause 8.3.2.2's own substitution: p[7,-1]'s value stands in for
        // all eight, and they are treated as available from here on.
        [top[7]; 8]
    } else {
        [0; 8]
    };

    Neighbours8 {
        top_available,
        top,
        top_right,
        left_available,
        left,
        corner_available,
        corner,
    }
}

/// Reconstructs one whole `Intra_8x8` macroblock's luma plane into `buf` --
/// [`reconstruct_intra4x4_mb`]'s own 8x8 counterpart, same per-block
/// interleaved predict/reconstruct order (four `luma8x8BlkIdx` quadrants,
/// raster order, each one fully written before the next reads its own
/// neighbours) but at 8x8 granularity throughout: [`predict_intra8x8`] for
/// prediction, [`dequant_8x8`]/`idct8x8` for the residual (no
/// `dc_already_scaled` split -- the 8x8 transform has no macroblock-wide
/// DC term of its own, see [`dequant_8x8`]'s own doc).
#[allow(
    clippy::many_single_char_names,
    reason = "x/y/n/c/d/r/p mirror clause 8.5's own variable names (pixel position, neighbours, coefficients, dequantised, residual, prediction sample) -- the same convention reconstruct_intra4x4_mb already uses"
)]
fn reconstruct_intra8x8_mb(buf: &mut PictureBuffer, mb_x: u32, mb_y: u32, qpy: i32, residual: &MbResidual) {
    for i8x8 in 0..4u32 {
        let (qx, qy) = (i8x8 & 1, i8x8 >> 1);
        let x = mb_x * 16 + qx * 8;
        let y = mb_y * 16 + qy * 8;
        let n = intra8x8_neighbours(buf, i8x8, coord(x), coord(y));
        let mode = residual.intra8x8_pred_mode.get(i8x8 as usize).copied().unwrap_or(2);
        let pred = predict_intra8x8(mode, n);

        let ac = residual.luma8x8.get(i8x8 as usize).and_then(Option::as_ref);
        let c = inverse_scan_luma_8x8(ac);
        let d = dequant_8x8(&c, qpy);
        let r = idct8x8(&d);

        let mut block = [[0u8; 8]; 8];
        for (i, row) in block.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                let p = i32::from(pred.get(i).and_then(|r| r.get(j)).copied().unwrap_or(0));
                let sum = p + r.get(i * 8 + j).copied().unwrap_or(0);
                *v = sum.clamp(0, 255) as u8;
            }
        }
        buf.write_block8(x, y, block);
    }
}

/// Reconstructs one whole `Intra_4x4` macroblock's luma plane into `buf`
/// at macroblock origin `(mb_x, mb_y)` (macroblock units) -- clause
/// 8.3.1's own per-block interleaved predict/reconstruct order (the NOTE
/// under clause 8.3.1.2: "Each block is assumed to be constructed into a
/// frame prior to decoding of the next block"), not
/// [`reconstruct_intra16x16_luma`]'s predict-the-whole-macroblock-then-add
/// shape.
#[allow(
    clippy::many_single_char_names,
    reason = "x/y/n/c/d/r/p mirror clause 8.5's own variable names (pixel position, neighbours, coefficients, dequantised, residual, prediction sample) -- renaming would lose the direct correspondence to the spec"
)]
fn reconstruct_intra4x4_mb(
    buf: &mut PictureBuffer,
    mb_x: u32,
    mb_y: u32,
    qpy: i32,
    residual: &MbResidual,
) {
    for blk in 0..16u32 {
        let (bx, by) = blk_xy(blk);
        let x = mb_x * 16 + bx * 4;
        let y = mb_y * 16 + by * 4;
        let n = intra4x4_neighbours(buf, coord(x), coord(y));
        let mode = residual
            .intra4x4_pred_mode
            .get(blk as usize)
            .copied()
            .unwrap_or(2);
        let pred = predict_intra4x4(mode, n);

        // Clause 8.5.4's plain 16-position scan (no DC/AC split at all --
        // that split is `Intra_16x16`-only): position (0, 0) is a normal
        // coefficient like any other, so `dequant_4x4`'s own
        // `dc_already_scaled = false`.
        let ac = residual.luma_ac.get(blk as usize).and_then(Option::as_ref);
        let c = inverse_scan_luma_dc(ac);
        let d = dequant_4x4(&c, qpy, false);
        let r = idct4x4(&d);

        let mut block = [[0u8; 4]; 4];
        for (i, row) in block.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                let p = i32::from(pred.get(i).and_then(|r| r.get(j)).copied().unwrap_or(0));
                let sum = p + r.get(i * 4 + j).copied().unwrap_or(0);
                *v = sum.clamp(0, 255) as u8;
            }
        }
        buf.write_block4(x, y, block);
    }
}

/// Reconstructs a whole picture's luma plane from one CABAC I-slice's
/// [`crate::mb::SliceStats::macroblocks`] -- `Intra_16x16` and `Intra_4x4`
/// macroblocks, in decode (raster) order, each drawing its own real
/// neighbour samples from macroblocks already reconstructed earlier in
/// that same order. `I_PCM` is refused (`Err`) rather than silently
/// producing wrong samples -- not attempted this round, and this crate's
/// oracle corpora do not use it.
///
/// Chroma is not reconstructed (see this module's own scope note) --
/// only the luma plane is returned, `mbs_wide * 16` wide by
/// `mbs_high * 16` tall, row-major.
///
/// # Errors
///
/// [`vaco_core::Error::Unsupported`] if any macroblock is `I_PCM` or
/// otherwise not one of `Intra_16x16`/`Intra_4x4` (e.g. an inter
/// macroblock reaching this function at all would itself be a scope
/// violation this crate's CABAC decode should have already refused
/// earlier).
pub(crate) fn reconstruct_picture_luma(
    macroblocks: &[MbSummary],
    mbs_wide: u32,
    mbs_high: u32,
    budget: &mut Budget,
) -> vaco_core::Result<Vec<u8>> {
    // `reconstruct_picture_luma` has no inter path, so nothing here reads a
    // reference; the scratch exists only to satisfy the shared signature.
    let mut scratch = ReadScratch::new(budget)?;
    let _ = &mut scratch;
    let mut buf = PictureBuffer::new(mbs_wide, mbs_high, budget)?;
    for mb in macroblocks {
        if mb.is_ipcm {
            return Err(vaco_core::Error::Unsupported(
                "vaco-codec-h264: I_PCM picture reconstruction is not implemented",
            ));
        }
        if mb.skipped {
            return Err(vaco_core::Error::Unsupported(
                "vaco-codec-h264: skipped-macroblock reconstruction is not implemented (unreachable for I slices)",
            ));
        }
        if mb.is_intra16x16 {
            let x = mb.mb_x * 16;
            let y = mb.mb_y * 16;
            let top_available = (0..16).all(|dx| buf.available(coord(x) + dx, coord(y) - 1));
            let top = core::array::from_fn(|dx| buf.pixel(coord(x) + coord(dx), coord(y) - 1));
            let left_available = (0..16).all(|dy| buf.available(coord(x) - 1, coord(y) + dy));
            let left = core::array::from_fn(|dy| buf.pixel(coord(x) - 1, coord(y) + coord(dy)));
            let neighbours = Neighbours16 {
                top_available,
                top,
                left_available,
                left,
                corner: buf.pixel(coord(x) - 1, coord(y) - 1),
            };
            let block = reconstruct_intra16x16_luma(
                mb.intra16x16_pred_mode,
                neighbours,
                mb.qpy,
                &mb.residual,
            );
            for (i, row) in block.iter().enumerate() {
                for (j, &v) in row.iter().enumerate() {
                    buf.set_pixel(x + j as u32, y + i as u32, v);
                }
            }
            for blk in 0..16u32 {
                let (bx, by) = blk_xy(blk);
                buf.mark_block_decoded(x + bx * 4, y + by * 4);
            }
        } else if mb.is_intra4x4 {
            reconstruct_intra4x4_mb(&mut buf, mb.mb_x, mb.mb_y, mb.qpy, &mb.residual);
        } else {
            return Err(vaco_core::Error::Unsupported(
                "vaco-codec-h264: picture reconstruction only implements Intra_16x16/Intra_4x4 macroblocks",
            ));
        }
    }
    Ok(buf.luma)
}

/// Clause 8.4.2's inter prediction (six-tap qpel luma via `crate::interp`)
/// plus the same residual-add path `reconstruct_picture_luma`'s own
/// `Intra_4x4` branch already uses -- an inter macroblock's own luma
/// residual is the same "no DC/AC split, one `CabacResidual` per 4x4
/// block" shape `Intra_4x4` uses (clause 8.5.4), just added onto a
/// motion-compensated prediction instead of a spatial one.
///
/// **Scope, explicit**: luma only (`PictureBuffer` has no chroma), list 0
/// only (P slices; B's list 1/bi-prediction is out of scope, matching
/// `crate::motion`'s own scope note), and `ref_list0[ref_idx]` is taken
/// as-is from the caller -- reference picture list *construction*
/// (clause 8.2.4, sliding window, MMCO, long-term marking) lives in the
/// caller (`crate::dpb`), not here, the same split `crate::motion` draws
/// between MV *prediction* and MV *derivation context*.
pub(crate) fn reconstruct_picture_with_inter(
    macroblocks: &[MbSummary],
    mbs_wide: u32,
    mbs_high: u32,
    ref_list0: &[RefPicturePlanes<'_>],
    budget: &mut Budget,
) -> vaco_core::Result<Vec<u8>> {
    let mut scratch = ReadScratch::new(budget)?;
    let mut buf = PictureBuffer::new(mbs_wide, mbs_high, budget)?;
    let ref_width = mbs_wide * 16;
    let ref_height = mbs_high * 16;
    for mb in macroblocks {
        if mb.is_ipcm {
            return Err(vaco_core::Error::Unsupported(
                "vaco-codec-h264: I_PCM picture reconstruction is not implemented",
            ));
        }
        if mb.is_intra16x16 {
            let x = mb.mb_x * 16;
            let y = mb.mb_y * 16;
            let top_available = (0..16).all(|dx| buf.available(coord(x) + dx, coord(y) - 1));
            let top = core::array::from_fn(|dx| buf.pixel(coord(x) + coord(dx), coord(y) - 1));
            let left_available = (0..16).all(|dy| buf.available(coord(x) - 1, coord(y) + dy));
            let left = core::array::from_fn(|dy| buf.pixel(coord(x) - 1, coord(y) + coord(dy)));
            let neighbours = Neighbours16 {
                top_available,
                top,
                left_available,
                left,
                corner: buf.pixel(coord(x) - 1, coord(y) - 1),
            };
            let block = reconstruct_intra16x16_luma(
                mb.intra16x16_pred_mode,
                neighbours,
                mb.qpy,
                &mb.residual,
            );
            for (i, row) in block.iter().enumerate() {
                for (j, &v) in row.iter().enumerate() {
                    buf.set_pixel(x + j as u32, y + i as u32, v);
                }
            }
            for blk in 0..16u32 {
                let (bx, by) = blk_xy(blk);
                buf.mark_block_decoded(x + bx * 4, y + by * 4);
            }
        } else if mb.is_intra4x4 {
            reconstruct_intra4x4_mb(&mut buf, mb.mb_x, mb.mb_y, mb.qpy, &mb.residual);
        } else {
            reconstruct_inter_mb(&mut buf, mb, ref_list0, &[], ref_width, ref_height, InterWeights::none(), &mut scratch);
        }
    }
    Ok(buf.luma)
}

#[allow(
    clippy::indexing_slicing,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::many_single_char_names,
    reason = "blk/i/j are fixed 0..4 or 0..16 loop bounds; mv/pixel arithmetic is checked at the fetch closure's own clamp; x/y/c/d/r mirror clause 8.5's own variable names"
)]
/// One list's own raw (unweighted) luma qpel prediction for a 4x4 block --
/// clause 8.4.2.2.1's motion compensation, factored out of
/// [`reconstruct_inter_mb`] so a `Bi` block can call it twice (once per
/// list) before [`InterWeights::combine`] ever runs, instead of the
/// single-list weighting this used to apply inline.
fn sample_luma_block(
    plane: RefPlane<'_>,
    ref_width: u32,
    ref_height: u32,
    x: u32,
    y: u32,
    mv: (i16, i16),
    scratch: &mut ReadScratch,
) -> [[u8; 4]; 4] {
    #[allow(
        clippy::cast_possible_wrap,
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "mirrors this function's own pre-existing arithmetic, unchanged by the L0/L1 factoring"
    )]
    {
        let (mvx, mvy) = (i32::from(mv.0), i32::from(mv.1));
        let (int_dx, frac_x) = (mvx >> 2, (mvx & 3) as u32);
        let (int_dy, frac_y) = (mvy >> 2, (mvy & 3) as u32);
        let x0 = x as i32 + int_dx;
        let y0 = y as i32 + int_dy;
        let mut pred = [[0u8; 4]; 4];
        // Clause 8.4.2.2.1's six-tap filter reads two samples above and left of
        // the block and three below and right of its last one: a 9x9 region at
        // `(x0 - 2, y0 - 2)`. That is the whole of this function's footprint,
        // and it is what a banded reference is asked for in one piece.
        let (rx0, ry0) = (x0 - 2, y0 - 2);
        let plane = match plane {
            RefPlane::Flat(data) => data,
            RefPlane::Banded(view) => {
                let Ok(b) = view.block(rx0, ry0, 9, 9, &mut scratch.block) else {
                    // Only reachable if a caller reconstructed a macroblock row
                    // before waiting for the rows its motion vectors reach.
                    scratch.failed = true;
                    return pred;
                };
                let (data, stride) = (b.data, b.stride);
                let fetch = |ax: i32, ay: i32| -> u8 {
                    let (rx, ry) = ((ax - rx0).max(0) as usize, (ay - ry0).max(0) as usize);
                    data.get(ry * stride + rx).copied().unwrap_or(0)
                };
                for (i, row) in pred.iter_mut().enumerate() {
                    for (j, v) in row.iter_mut().enumerate() {
                        let full_x = x as i32 + j as i32 + int_dx;
                        let full_y = y as i32 + i as i32 + int_dy;
                        *v = crate::interp::luma_qpel_sample(fetch, full_x, full_y, frac_x, frac_y);
                    }
                }
                return pred;
            }
        };
        let safe = !plane.is_empty()
            && x0 - 2 >= 0
            && x0 + 6 < ref_width as i32
            && y0 - 2 >= 0
            && y0 + 6 < ref_height as i32;
        let fetch_clamped = |ax: i32, ay: i32| -> u8 {
            if plane.is_empty() {
                return 0;
            }
            let cx = ax.clamp(0, ref_width as i32 - 1) as u32;
            let cy = ay.clamp(0, ref_height as i32 - 1) as u32;
            plane.get((cy * ref_width + cx) as usize).copied().unwrap_or(0)
        };
        let fetch_fast = |ax: i32, ay: i32| -> u8 {
            let cx = ax as u32;
            let cy = ay as u32;
            plane.get((cy * ref_width + cx) as usize).copied().unwrap_or(0)
        };
        for (i, row) in pred.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                let full_x = x as i32 + j as i32 + int_dx;
                let full_y = y as i32 + i as i32 + int_dy;
                *v = if safe {
                    crate::interp::luma_qpel_sample(fetch_fast, full_x, full_y, frac_x, frac_y)
                } else {
                    crate::interp::luma_qpel_sample(fetch_clamped, full_x, full_y, frac_x, frac_y)
                };
            }
        }
        pred
    }
}

#[allow(
    clippy::indexing_slicing,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::many_single_char_names,
    reason = "mirrors sample_luma_block's own identical allow; w/h are always <= 16 (clamped inside crate::interp::luma_qpel_partition)"
)]
/// [`sample_luma_block`]'s own whole-partition counterpart: one list's raw
/// luma qpel prediction for a partition up to 16x16, fetched and filtered once
/// through [`crate::interp::luma_qpel_partition`] rather than once per
/// 4x4 block. `x`/`y` are the partition's own picture-relative top-left
/// corner; `w`/`h` its size, always a multiple of 4 not exceeding 16 for
/// any real H.264 partition or merged group of same-motion partitions
/// (see [`partition_rects`]).
fn sample_luma_partition(
    plane: RefPlane<'_>,
    ref_width: u32,
    ref_height: u32,
    x: u32,
    y: u32,
    w: usize,
    h: usize,
    mv: (i16, i16),
    scratch: &mut ReadScratch,
) -> [[u8; 16]; 16] {
    let (mvx, mvy) = (i32::from(mv.0), i32::from(mv.1));
    let (int_dx, frac_x) = (mvx >> 2, (mvx & 3) as u32);
    let (int_dy, frac_y) = (mvy >> 2, (mvy & 3) as u32);
    let x0 = x as i32 + int_dx;
    let y0 = y as i32 + int_dy;
    let mut out = [[0u8; 16]; 16];
    // Clause 8.4.2.2.1's six-tap filter reads two samples above/left of the
    // partition and three below/right of its last one -- a
    // `(w + 5) x (h + 5)` region at `(x0 - 2, y0 - 2)`, generalising
    // [`sample_luma_block`]'s fixed 9x9 the same way the partition itself
    // generalises a fixed 4x4 block.
    let (rx0, ry0) = (x0 - 2, y0 - 2);
    let plane = match plane {
        RefPlane::Flat(data) => data,
        RefPlane::Banded(view) => {
            let Ok(b) = view.block(rx0, ry0, (w + 5) as u32, (h + 5) as u32, &mut scratch.block) else {
                // Only reachable if a caller reconstructed a macroblock row
                // before waiting for the rows its motion vectors reach.
                scratch.failed = true;
                return out;
            };
            let (data, stride) = (b.data, b.stride);
            let fetch = |ax: i32, ay: i32| -> u8 {
                let (rx, ry) = ((ax - rx0).max(0) as usize, (ay - ry0).max(0) as usize);
                data.get(ry * stride + rx).copied().unwrap_or(0)
            };
            crate::interp::luma_qpel_partition(fetch, x0, y0, w, h, frac_x, frac_y, &mut out);
            return out;
        }
    };
    let safe = !plane.is_empty()
        && x0 - 2 >= 0
        && x0 + w as i32 + 2 < ref_width as i32
        && y0 - 2 >= 0
        && y0 + h as i32 + 2 < ref_height as i32;
    let fetch_clamped = |ax: i32, ay: i32| -> u8 {
        if plane.is_empty() {
            return 0;
        }
        let cx = ax.clamp(0, ref_width as i32 - 1) as u32;
        let cy = ay.clamp(0, ref_height as i32 - 1) as u32;
        plane.get((cy * ref_width + cx) as usize).copied().unwrap_or(0)
    };
    let fetch_fast = |ax: i32, ay: i32| -> u8 {
        let cx = ax as u32;
        let cy = ay as u32;
        plane.get((cy * ref_width + cx) as usize).copied().unwrap_or(0)
    };
    if safe {
        crate::interp::luma_qpel_partition(fetch_fast, x0, y0, w, h, frac_x, frac_y, &mut out);
    } else {
        crate::interp::luma_qpel_partition(fetch_clamped, x0, y0, w, h, frac_x, frac_y, &mut out);
    }
    out
}

/// One maximal rectangle of a macroblock's own 4x4 motion grid
/// (`MbSummary::mv_blocks`) sharing exactly one [`MvInfo`] -- the group
/// [`sample_luma_partition`] predicts in a single call. Coordinates are in
/// 4x4-block units within the macroblock (`0..4`).
#[derive(Clone, Copy)]
struct PartitionRect {
    bx: u8,
    by: u8,
    bw: u8,
    bh: u8,
    info: MvInfo,
}

#[allow(
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "bx/by/bw/bh are all loop variables over 0..4, provably in range for the fixed 4x4 grids here"
)]
/// Decompose a macroblock's own 16-entry 4x4 motion grid into the maximal
/// axis-aligned rectangles of identical motion -- the partition (or merged
/// group of adjacent same-motion
/// partitions) [`sample_luma_partition`] predicts as one call instead of
/// [`sample_luma_block`]'s own one-call-per-4x4-block shape.
///
/// This is correct regardless of whether two *merged* cells were really one
/// syntax-level partition or two adjacent ones that happen to carry
/// identical `ref_idx`/`mv` on both lists: clause 8.4's own prediction
/// value at any position depends only on that position's resolved motion,
/// never on which partition the bitstream said it belonged to, so
/// predicting a larger uniform region in one call reproduces the per-4x4
/// oracle bit for bit (checked directly, not merely argued, by this
/// module's own `partition_rects_matches_the_per_4x4_oracle` test family).
///
/// A real H.264 sub-macroblock partitioning is always already a tiling of
/// this grid into rectangles (16x16, 16x8, 8x16, 8x8 and 8x8's own
/// 8x8/8x4/4x8/4x4 sub-partitions), so the greedy "grow right, then grow
/// down" scan below recovers the true partition boundaries exactly -- it is
/// not a general maximal-rectangle solver, which this grid never needs.
fn partition_rects(mv_blocks: &[MvInfo; 16]) -> ([PartitionRect; 16], usize) {
    let key = |i: &MvInfo| {
        (
            i.reads_l0(),
            i.reads_l1(),
            i.ref_idx_l0(),
            i.ref_idx_l1(),
            i.mv_l0(),
            i.mv_l1(),
        )
    };
    let at = |bx: u32, by: u32| mv_blocks[(by * 4 + bx) as usize];
    let mut done = [[false; 4]; 4];
    let mut rects = [PartitionRect { bx: 0, by: 0, bw: 0, bh: 0, info: MvInfo::default() }; 16];
    let mut n = 0usize;
    for by in 0..4u32 {
        for bx in 0..4u32 {
            if done[by as usize][bx as usize] {
                continue;
            }
            let k0 = key(&at(bx, by));
            let mut bw = 1u32;
            while bx + bw < 4 && !done[by as usize][(bx + bw) as usize] && key(&at(bx + bw, by)) == k0 {
                bw += 1;
            }
            let mut bh = 1u32;
            'grow_down: while by + bh < 4 {
                for dx in 0..bw {
                    if done[(by + bh) as usize][(bx + dx) as usize] || key(&at(bx + dx, by + bh)) != k0 {
                        break 'grow_down;
                    }
                }
                bh += 1;
            }
            for dy in 0..bh {
                for dx in 0..bw {
                    done[(by + dy) as usize][(bx + dx) as usize] = true;
                }
            }
            if let Some(slot) = rects.get_mut(n) {
                *slot = PartitionRect { bx: bx as u8, by: by as u8, bw: bw as u8, bh: bh as u8, info: at(bx, by) };
            }
            n += 1;
        }
    }
    (rects, n)
}

#[allow(
    clippy::indexing_slicing,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::many_single_char_names,
    reason = "blk/i/j are fixed 0..4 or 0..16 loop bounds; mv/pixel arithmetic is checked at the fetch closure's own clamp; x/y/c/d/r mirror clause 8.5's own variable names"
)]
fn reconstruct_inter_mb(
    buf: &mut PictureBuffer,
    mb: &MbSummary,
    ref_list0: &[RefPicturePlanes<'_>],
    ref_list1: &[RefPicturePlanes<'_>],
    ref_width: u32,
    ref_height: u32,
    weights: InterWeights<'_>,
    scratch: &mut ReadScratch,
) {
    let empty = RefPlane::Flat(&[]);
    // Motion compensation is unaffected by `transform_size_8x8_flag` --
    // clause 8.4's own prediction-sample derivation never reads it, only
    // the residual (clause 7.3.5.3.3) does -- so prediction runs exactly
    // once for the whole macroblock, over the maximal same-motion
    // rectangles `partition_rects` finds in `mb.mv_blocks`, before either
    // residual path below reads from the assembled `pred_mb`. This replaces the
    // one-call-per-4x4-block shape `sample_luma_block`
    // (`fetch_pred_4x4`, kept for the differential tests) used before: a
    // real 16x16 partition used to cost sixteen independent 9x9
    // re-fetches for 256 outputs needing only 441 source samples between
    // them.
    let (rects, n) = partition_rects(&mb.mv_blocks);
    let mut pred_mb = [[0u8; 16]; 16];
    for rect in rects.iter().take(n) {
        let info = rect.info;
        let (w, h) = (usize::from(rect.bw) * 4, usize::from(rect.bh) * 4);
        let x = mb.mb_x * 16 + u32::from(rect.bx) * 4;
        let y = mb.mb_y * 16 + u32::from(rect.by) * 4;
        let ref_idx0 = info.ref_idx_l0().max(0) as usize;
        let ref_idx1 = info.ref_idx_l1().max(0) as usize;
        // Clause 8.4.2.3: both the single-list weighting and the
        // bi-prediction combination are per *reference index*, so both are
        // resolved once the two raw samples (or the one, for a
        // single-list block) are known -- never before, since which
        // weight entry applies depends on `ref_idx`, not on the list
        // membership alone.
        let p0 = info.reads_l0().then(|| {
            sample_luma_partition(ref_list0.get(ref_idx0).map_or(empty, |r| r.luma), ref_width, ref_height, x, y, w, h, info.mv_l0(), scratch)
        });
        let p1 = info.reads_l1().then(|| {
            sample_luma_partition(ref_list1.get(ref_idx1).map_or(empty, |r| r.luma), ref_width, ref_height, x, y, w, h, info.mv_l1(), scratch)
        });
        let (row0, col0) = (usize::from(rect.by) * 4, usize::from(rect.bx) * 4);
        for oy in 0..h {
            for ox in 0..w {
                // `.as_ref()` first: `p0`/`p1` are `Option<[[u8; 16]; 16]>`
                // (`Copy`, since `[u8; 16]` is), so `.map()` called directly
                // on them by value copies the whole 256-byte array into the
                // closure on *every* one of this loop's `w * h` iterations
                // (up to 256 per merged partition) just to read one byte out
                // of it -- measured as the dominant cost of an earlier
                // version of this function, large enough on its own to
                // erase this item's entire fetch-count win.
                let a = p0.as_ref().map(|b| b[oy][ox]);
                let b = p1.as_ref().map(|b| b[oy][ox]);
                let v = match (a, b) {
                    (Some(a), Some(b)) => weights.combine(ref_idx0, ref_idx1, a, b),
                    (Some(a), None) => weights.single(0, ref_idx0, a),
                    (None, Some(b)) => weights.single(1, ref_idx1, b),
                    (None, None) => 0,
                };
                if let Some(dst) = pred_mb.get_mut(row0 + oy).and_then(|r| r.get_mut(col0 + ox)) {
                    *dst = v;
                }
            }
        }
    }

    if mb.transform_8x8 {
        for i8x8 in 0..4u32 {
            let (qx, qy) = (i8x8 & 1, i8x8 >> 1);
            let x = mb.mb_x * 16 + qx * 8;
            let y = mb.mb_y * 16 + qy * 8;
            let (row0, col0) = (qy as usize * 8, qx as usize * 8);
            let ac = mb.residual.luma8x8.get(i8x8 as usize).and_then(Option::as_ref);
            let c = inverse_scan_luma_8x8(ac);
            let d = dequant_8x8(&c, mb.qpy);
            let r = idct8x8(&d);
            let mut block = [[0u8; 8]; 8];
            for (i, row) in block.iter_mut().enumerate() {
                for (j, v) in row.iter_mut().enumerate() {
                    let sum = i32::from(pred_mb[row0 + i][col0 + j]) + r.get(i * 8 + j).copied().unwrap_or(0);
                    *v = sum.clamp(0, 255) as u8;
                }
            }
            buf.write_block8(x, y, block);
        }
        return;
    }

    for blk in 0..16u32 {
        let (bx, by) = blk_xy(blk);
        let x = mb.mb_x * 16 + bx * 4;
        let y = mb.mb_y * 16 + by * 4;
        let (row0, col0) = (by as usize * 4, bx as usize * 4);

        let ac = mb
            .residual
            .luma_ac
            .get(blk as usize)
            .and_then(Option::as_ref);
        let c = inverse_scan_luma_dc(ac);
        let d = dequant_4x4(&c, mb.qpy, false);
        let r = idct4x4(&d);

        let mut block = [[0u8; 4]; 4];
        for (i, row) in block.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                let sum = i32::from(pred_mb[row0 + i][col0 + j]) + r.get(i * 4 + j).copied().unwrap_or(0);
                *v = sum.clamp(0, 255) as u8;
            }
        }
        buf.write_block4(x, y, block);
    }
}

/// Clause 8.4.2.3's explicit weighted-prediction parameters for one slice,
/// list 0 only (this decoder is single-list: B slices are refused before
/// reconstruction ever runs).
///
/// `weighted_pred_flag` is **x264's own default** for P slices, so this is
/// not an exotic path: nearly every real H.264 file carries a
/// `pred_weight_table()`. On most content the encoder picks the neutral
/// weight (`w == 1 << logWD`, `o == 0`) and the weighted formula collapses
/// to a plain copy, which is exactly why ignoring it looked correct across
/// several fixtures -- and why the first fixture with real global brightness
/// change (`life`, a cellular automaton that flickers) had *every* inter
/// macroblock wrong from its first P picture, at `w = 15, logWD = 4,
/// o = -3`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PredWeight {
    /// `luma_log2_weight_denom` / `chroma_log2_weight_denom` (`logWD`).
    pub(crate) log2_denom: u8,
    /// `luma_weight_l0[refIdxL0]` / `chroma_weight_l0[refIdxL0][iCbCr]`.
    pub(crate) weight: i32,
    /// `luma_offset_l0[refIdxL0]` / `chroma_offset_l0[refIdxL0][iCbCr]`.
    pub(crate) offset: i32,
}

impl PredWeight {
    /// Clause 8.4.2.3.2, eq. (8-270)/(8-271), the single-list (`predFlagL0
    /// == 1`, `predFlagL1 == 0`) case:
    ///
    /// ```text
    /// logWD >= 1: Clip1( ( ( pred * w0 + 2^(logWD-1) ) >> logWD ) + o0 )
    /// logWD == 0: Clip1( pred * w0 + o0 )
    /// ```
    ///
    /// The offset is `o0 << (BitDepth - 8)`, which is `o0` unchanged at the
    /// 8-bit depth this decoder supports.
    fn apply(self, pred: u8) -> u8 {
        let p = i32::from(pred);
        let v = if self.log2_denom >= 1 {
            let round = 1i32 << (self.log2_denom - 1);
            ((p.saturating_mul(self.weight).saturating_add(round)) >> self.log2_denom)
                .saturating_add(self.offset)
        } else {
            p.saturating_mul(self.weight).saturating_add(self.offset)
        };
        #[allow(
            clippy::cast_sign_loss,
            clippy::cast_possible_truncation,
            reason = "clamped to 0..=255 immediately above the cast"
        )]
        {
            v.clamp(0, 255) as u8
        }
    }
}

/// One slice's whole `pred_weight_table()`, already split per plane so
/// [`reconstruct_picture`]'s inner loops index a plain slice rather than
/// re-deriving anything per macroblock. Empty (`weighted == false`) for an
/// I slice or a P slice whose PPS has `weighted_pred_flag == 0`.
///
/// `l1` is always empty for P/SP slices (`pred_weight_table()` has no list
/// 1 at all there — `vaco_parse_h264::PredWeightTable::l1` is `Vec::new()`
/// by construction) and real for a B slice with `weighted_bipred_idc == 1`
/// (clause 8.4.2.3's *explicit* weighted bi-prediction, the sibling of the
/// single-list case `l0` already handles).
#[derive(Debug, Default)]
pub(crate) struct SliceWeightTables {
    weighted: bool,
    luma: Vec<PredWeight>,
    chroma: [Vec<PredWeight>; 2],
    luma1: Vec<PredWeight>,
    chroma1: [Vec<PredWeight>; 2],
}

/// One `(l0, l1)` reference index pair's clause 8.4.2.3.2 implicit weights
/// (`weighted_bipred_idc == 2`, `logWD` fixed at 5, offsets always 0) --
/// x264's own default for B slices, so this is the common bi-prediction
/// path in real content, not an exotic one.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ImplicitWeight {
    pub(crate) w0: i32,
    pub(crate) w1: i32,
}

/// The whole slice's implicit-weight table, one entry per `(ref_idx_l0,
/// ref_idx_l1)` pair -- built by `crate::decoder` (the only place that
/// knows every reference picture's own POC, clause 8.4.2.3.2's own input)
/// and handed in here as a plain lookup, the same "construction lives with
/// the caller who has the data" split `RefPicturePlanes` already draws for
/// reference *pixels*.
#[derive(Debug, Default)]
pub(crate) struct ImplicitWeights {
    /// `table[ref_idx_l0][ref_idx_l1]`.
    table: Vec<Vec<ImplicitWeight>>,
}

impl ImplicitWeights {
    pub(crate) fn new(table: Vec<Vec<ImplicitWeight>>) -> Self {
        Self { table }
    }

    /// `(32, 32)` (equal-weight average, clause 8.4.2.3.2's own fallback
    /// for `td == 0`/long-term/out-of-range `DistScaleFactor`) for any pair
    /// this slice's own table does not cover -- defensive only, since a
    /// well-formed B slice's table always has one row per active list-0
    /// reference and one column per active list-1 reference.
    fn get(&self, ref_idx_l0: usize, ref_idx_l1: usize) -> ImplicitWeight {
        self.table
            .get(ref_idx_l0)
            .and_then(|row| row.get(ref_idx_l1))
            .copied()
            .unwrap_or(ImplicitWeight { w0: 32, w1: 32 })
    }
}

/// Clause 8.4.2.3's own three bi-prediction modes (`weighted_bipred_idc`),
/// resolved once per slice rather than re-branched per block.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) enum BiPredMode {
    /// `weighted_bipred_idc == 0`: clause 8.4.2.3.1's plain average,
    /// `(predL0 + predL1 + 1) >> 1` -- no weight table involved at all.
    #[default]
    Default,
    /// `weighted_bipred_idc == 1`: each list's own explicit
    /// `pred_weight_table()` entry, combined per clause 8.4.2.3.2's
    /// two-list formula.
    Explicit,
    /// `weighted_bipred_idc == 2`: [`ImplicitWeights`]'s own
    /// POC-distance-derived per-reference-pair weights -- **x264's own
    /// default for B slices**, so this is the common real-content path.
    Implicit,
}

/// Everything [`reconstruct_inter_mb`]/`predict_chroma_inter` need to turn
/// two already-fetched, unweighted prediction samples (or one, for an
/// `L0`-/`L1`-only block) into the final predicted sample -- bundled so
/// call sites do not thread five separate arguments through for what is,
/// per block, a single decision.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct InterWeights<'a> {
    pub(crate) l0: SliceWeights<'a>,
    pub(crate) l1: SliceWeights<'a>,
    pub(crate) mode: BiPredMode,
    pub(crate) implicit: Option<&'a ImplicitWeights>,
}

impl InterWeights<'_> {
    const fn none() -> Self {
        Self { l0: None, l1: None, mode: BiPredMode::Default, implicit: None }
    }

    /// One list's own single-list prediction (`Pred_L0`/`Pred_L1`, clause
    /// 8.4.2.3.2's single-list branch): the plain weight table lookup,
    /// unweighted-identity when this slice carries no table at all.
    fn single(self, list: usize, ref_idx: usize, sample: u8) -> u8 {
        let table = if list == 0 { self.l0 } else { self.l1 };
        weight_for(table, ref_idx).map_or(sample, |w| w.apply(sample))
    }

    /// Clause 8.4.2.3's own bi-prediction combination -- the three
    /// `weighted_bipred_idc` cases, each transcribed from JM 19.1's
    /// `mc_prediction.c::weighted_bi_prediction`/`fill_wp_params` (Tier A
    /// per `provenance/sources.toml`) rather than re-derived from the
    /// specification prose a second time: `Default` is a plain rounded
    /// average; `Explicit` and `Implicit` both reduce to
    /// `round_shift(w0*p0 + w1*p1, logWD + 1) + ((o0+o1+1)>>1)`, differing
    /// only in where `(w0, w1, o0, o1, logWD)` come from.
    fn combine(self, ref_idx0: usize, ref_idx1: usize, p0: u8, p1: u8) -> u8 {
        let (p0, p1) = (i32::from(p0), i32::from(p1));
        match self.mode {
            BiPredMode::Default => {
                #[allow(
                    clippy::cast_sign_loss,
                    clippy::cast_possible_truncation,
                    reason = "(p0+p1+1)>>1 with p0/p1 in 0..=255 is always in 0..=255"
                )]
                {
                    ((p0 + p1 + 1) >> 1) as u8
                }
            }
            BiPredMode::Explicit => {
                let w0 = weight_for(self.l0, ref_idx0).unwrap_or(PredWeight { log2_denom: 0, weight: 1, offset: 0 });
                let w1 = weight_for(self.l1, ref_idx1).unwrap_or(PredWeight { log2_denom: 0, weight: 1, offset: 0 });
                let log_wd = w0.log2_denom;
                let round = 1i32 << log_wd;
                let sum = p0.saturating_mul(w0.weight).saturating_add(p1.saturating_mul(w1.weight)).saturating_add(round);
                let shifted = sum >> (log_wd + 1);
                let offset = (w0.offset + w1.offset + 1) >> 1;
                shifted.saturating_add(offset).clamp(0, 255).cast_unsigned_u8()
            }
            BiPredMode::Implicit => {
                let w = self.implicit.map_or(ImplicitWeight { w0: 32, w1: 32 }, |t| t.get(ref_idx0, ref_idx1));
                let sum = p0.saturating_mul(w.w0).saturating_add(p1.saturating_mul(w.w1)).saturating_add(32);
                (sum >> 6).clamp(0, 255).cast_unsigned_u8()
            }
        }
    }
}

/// `i32 -> u8`, clamped range already asserted by the caller -- named so
/// [`InterWeights::combine`]'s own `#[allow]` burden stays at one spot
/// instead of three.
trait ClampedCast {
    fn cast_unsigned_u8(self) -> u8;
}
impl ClampedCast for i32 {
    #[allow(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "the value is clamp(0, 255) immediately before this call"
    )]
    fn cast_unsigned_u8(self) -> u8 {
        self as u8
    }
}

impl SliceWeightTables {
    /// Builds the tables from a parsed `pred_weight_table()`, filling in
    /// clause 7.4.3.2's own inferred values for any reference whose
    /// `luma_weight_l0_flag`/`chroma_weight_l0_flag` was 0: the neutral
    /// weight `2^logWD` with offset 0, i.e. an identity.
    pub(crate) fn from_table(table: Option<&vaco_parse_h264::PredWeightTable>) -> Self {
        let Some(t) = table else { return Self::default() };
        let luma_denom = t.luma_log2_weight_denom;
        let chroma_denom = t.chroma_log2_weight_denom.unwrap_or(0);
        let neutral = |denom: u8| 1i32 << denom;
        let build = |entries: &[vaco_parse_h264::slice::RefWeight]| -> (Vec<PredWeight>, [Vec<PredWeight>; 2]) {
            let mut luma = Vec::new();
            let mut chroma = [Vec::new(), Vec::new()];
            for entry in entries {
                let (w, o) = entry.luma.unwrap_or((neutral(luma_denom), 0));
                luma.push(PredWeight { log2_denom: luma_denom, weight: w, offset: o });
                let c = entry.chroma.unwrap_or([(neutral(chroma_denom), 0); 2]);
                for comp in 0..2usize {
                    let (w, o) = c.get(comp).copied().unwrap_or((neutral(chroma_denom), 0));
                    if let Some(v) = chroma.get_mut(comp) {
                        v.push(PredWeight { log2_denom: chroma_denom, weight: w, offset: o });
                    }
                }
            }
            (luma, chroma)
        };
        let (luma, chroma) = build(&t.l0);
        let (luma1, chroma1) = build(&t.l1);
        Self { weighted: true, luma, chroma, luma1, chroma1 }
    }

    fn luma(&self) -> SliceWeights<'_> {
        self.weighted.then_some(self.luma.as_slice())
    }

    fn chroma(&self, comp: usize) -> SliceWeights<'_> {
        if !self.weighted {
            return None;
        }
        self.chroma.get(comp).map(Vec::as_slice)
    }

    fn luma1(&self) -> SliceWeights<'_> {
        self.weighted.then_some(self.luma1.as_slice())
    }

    fn chroma1(&self, comp: usize) -> SliceWeights<'_> {
        if !self.weighted {
            return None;
        }
        self.chroma1.get(comp).map(Vec::as_slice)
    }
}

/// Every reference index's weights for one slice and one plane, or `None`
/// when `weighted_pred_flag` is 0 (no `pred_weight_table()` at all -- an
/// unweighted plain copy, which is *not* the same as a table full of
/// neutral weights only because it costs nothing to skip).
pub(crate) type SliceWeights<'a> = Option<&'a [PredWeight]>;

/// The weight for `ref_idx`, or the identity when this slice is unweighted
/// or the table is shorter than the reference list (defensive: clause
/// 7.4.3.2 requires one entry per active reference, but a malformed stream
/// need not comply and this must not panic or silently mispredict).
fn weight_for(weights: SliceWeights<'_>, ref_idx: usize) -> Option<PredWeight> {
    weights.and_then(|w| w.get(ref_idx)).copied()
}

/// One reference picture's three planes -- what [`reconstruct_picture`]'s
/// own `ref_list0` needs per candidate, since clause 8.4.2.1's per-block
/// `ref_idx_l0` selection has to reach chroma exactly the same way
/// [`reconstruct_inter_mb`]'s own `ref_list0: &[&[u8]]` already reaches
/// luma.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RefPicturePlanes<'a> {
    pub(crate) luma: RefPlane<'a>,
    pub(crate) cb: RefPlane<'a>,
    pub(crate) cr: RefPlane<'a>,
}

/// How one reference plane is stored, which is the only thing motion
/// compensation has to care about beyond the samples themselves.
///
/// A picture published in a single band (`-threads 1`, and every test oracle
/// in this module) is one allocation, and every read is the plain indexed fetch
/// this decoder has always done -- [`RefPlane::Flat`] is exactly that code, so
/// the non-threaded path pays nothing at all for row granularity. A picture
/// published band by band while it is still being produced cannot be one
/// allocation (a writer cannot hold `&mut` above row `R` while a reader holds
/// `&` below it), so [`RefPlane::Banded`] asks
/// [`vaco_codec_core::picture::PlaneView::block`] for the region a block needs
/// in one piece and reads that. Both arms feed the same clause 8.4.2.2
/// arithmetic in [`crate::interp`]; only the fetch differs, exactly as the
/// in-picture and edge-clamped fetches already did.
#[derive(Debug, Clone, Copy)]
pub(crate) enum RefPlane<'a> {
    /// The whole plane, row-major at its own width. Empty means "no such
    /// reference", which reads as zero.
    Flat(&'a [u8]),
    /// A plane published in bands, readable up to this view's own watermark.
    Banded(PlaneView<'a>),
}

/// The per-task working state a reference read needs a `&mut` on.
///
/// `failed` exists because [`vaco_codec_core::picture::PlaneView::block`] can
/// refuse a region whose rows are not published yet, and motion compensation
/// has no `Result` to put that in without threading one through clause 8.4's
/// whole prediction path. Fabricating samples instead would be silent and
/// content-dependent, so the flag is raised and
/// [`reconstruct_picture_rows`] turns it into an error at the end of the
/// macroblock row -- one branch per row, not per sample.
#[derive(Debug)]
pub(crate) struct ReadScratch {
    block: BlockScratch,
    failed: bool,
}

impl ReadScratch {
    /// Scratch for the largest region clause 8.4.2.2 reads: a whole 16x16
    /// partition's own six-tap footprint -- `(16 + 5) x (16 + 5)`, up from
    /// the 16x16 this held back
    /// when every partition was predicted 4x4 at a time.
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::LimitExceeded`] when the budget refuses.
    pub(crate) fn new(budget: &mut Budget) -> vaco_core::Result<Self> {
        Ok(Self { block: BlockScratch::new(budget, 21, 21)?, failed: false })
    }

    /// Turn a raised failure flag into an error, and clear it.
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::InvalidData`] if any reference read since the last
    /// check could not be served from the rows published so far.
    fn check(&mut self) -> vaco_core::Result<()> {
        if core::mem::take(&mut self.failed) {
            return Err(vaco_core::Error::InvalidData(
                "vaco-codec-h264: motion compensation read past a reference picture's published rows",
            ));
        }
        Ok(())
    }

    /// Put a reused scratch back into the state [`ReadScratch::new`] leaves
    /// it in, without reallocating [`Self::block`].
    ///
    /// `block` itself needs no clearing: every read through it
    /// (`crate::interp`'s fetch closures) writes the whole footprint it is
    /// about to filter over before reading any of it back, the same
    /// invariant that already lets a *fresh* `BlockScratch` start from
    /// whatever the allocator handed it rather than a zeroed buffer.
    fn reset(&mut self) {
        self.failed = false;
    }
}

/// A fully reconstructed picture: luma at `mbs_wide*16 x mbs_high*16`,
/// Cb/Cr at half that in each dimension (`ChromaArrayType == 1`, this
/// crate's only supported chroma format), all row-major.
#[derive(Debug, Clone, Default)]
pub(crate) struct ReconstructedPicture {
    pub(crate) luma: Vec<u8>,
    pub(crate) cb: Vec<u8>,
    pub(crate) cr: Vec<u8>,
}

/// [`intra4x4_neighbours`]'s own chroma counterpart: clause 8.3.3's 17
/// neighbouring chroma samples (`p[x,-1]` for `x = 0..7`, `p[-1,y]` for
/// `y = -1..7`), resolved from real picture state at the *macroblock's*
/// own 8x8 chroma origin -- unlike luma's `Intra_4x4`, chroma prediction
/// always operates at this one whole-macroblock granularity regardless of
/// which luma mode the same macroblock used (clause 8.3.3's own
/// `intra_chroma_pred_mode` is one value per macroblock).
fn chroma_neighbours(buf: &PictureBuffer, comp: usize, mb_x: u32, mb_y: u32) -> NeighboursChroma {
    let x = coord(mb_x * 8);
    let y = coord(mb_y * 8);
    let top_available = (0..8).all(|dx| buf.chroma_available(x + dx, y - 1));
    let top = core::array::from_fn(|dx| buf.chroma_pixel(comp, x + coord(dx), y - 1));
    let left_available = (0..8).all(|dy| buf.chroma_available(x - 1, y + dy));
    let left = core::array::from_fn(|dy| buf.chroma_pixel(comp, x - 1, y + coord(dy)));
    NeighboursChroma {
        top_available,
        top,
        left_available,
        left,
        corner: buf.chroma_pixel(comp, x - 1, y - 1),
    }
}

/// Clause 8.5.3's chroma residual: the shared 2x2 DC transform
/// ([`dequant_chroma_dc_2x2`]) folded into each of the 4 chroma AC
/// blocks' own position (0, 0) (the same "DC injected into every AC
/// block" shape [`build_luma_ac_block`] already gives luma's
/// `Intra_16x16`, reused as-is -- clause 8.5.3 eq. (8-248)/(8-249) is
/// the identical construction one level down), then added onto `pred`
/// (either an intra prediction or a motion-compensated one -- this step
/// does not care which).
#[allow(
    clippy::indexing_slicing,
    reason = "bx/by are 0/1 from blk_xy(0..4), x_o/y_o in {0,4}, i/j in 0..4 -- every index below is provably in range, not bitstream-derived"
)]
fn add_chroma_residual(mut pred: [[u8; 8]; 8], comp: usize, mb: &MbSummary, qpc: i32) -> [[u8; 8]; 8] {
    let dc_raw = inverse_scan_chroma_dc(mb.residual.chroma_dc.get(comp).and_then(Option::as_ref));
    let dc = dequant_chroma_dc_2x2(&dc_raw, qpc);
    for blk in 0..4u32 {
        let (bx, by) = blk_xy(blk);
        let dc_val = dc.get((by * 2 + bx) as usize).copied().unwrap_or(0);
        let ac = mb
            .residual
            .chroma_ac
            .get(comp)
            .and_then(|arr| arr.get(blk as usize))
            .and_then(Option::as_ref);
        let c = build_luma_ac_block(dc_val, ac);
        let d = dequant_4x4(&c, qpc, true);
        let r = idct4x4(&d);

        let x_o = (bx * 4) as usize;
        let y_o = (by * 4) as usize;
        for i in 0..4usize {
            for j in 0..4usize {
                let p = i32::from(pred[y_o + i][x_o + j]);
                let sum = p + r.get(i * 4 + j).copied().unwrap_or(0);
                pred[y_o + i][x_o + j] = sum.clamp(0, 255) as u8;
            }
        }
    }
    pred
}

/// Clause 8.4.2.2.2's bilinear chroma motion compensation for one whole
/// macroblock's one chroma component, reusing the *same* per-luma-4x4-block
/// `mv_blocks` an inter macroblock's luma prediction already reads
/// ([`reconstruct_inter_mb`]) -- each luma 4x4 partition's own motion
/// vector governs a 2x2 chroma area (clause 8.4.1.4's own note: "when the
/// luma vector applies to 4x4 luma samples, the corresponding chroma
/// vector applies to 2x2 chroma samples"), so looping the same 16
/// positions at chroma's half resolution generalises correctly to every
/// partition shape (16x16 down to 4x4) without special-casing any of
/// them -- exactly the same shape [`reconstruct_inter_mb`] already uses
/// for luma, one level coarser.
#[allow(
    clippy::indexing_slicing,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "blk/dx/dy are fixed 0..16/0..2 loop bounds; mv/pixel arithmetic is checked at the fetch closure's own clamp -- mirrors reconstruct_inter_mb's own identical allow"
)]
/// The largest `ref_idx` clause 7.4.3's `num_ref_idx_l0_active_minus1` can
/// name, plus one -- the width of [`RowReach`]'s per-reference arrays.
pub(crate) const MAX_REF_IDX: usize = 32;

/// How far down each reference picture reconstructing one macroblock row
/// reaches, per list and per reference index.
///
/// This is the bound a row-threaded caller waits on, and it is derived from
/// clause 8.4.2.2's own filter reach rather than assumed:
///
/// * **Luma.** [`sample_luma_block`] reads rows `y0 - 2 ..= y0 + 6` for a 4x4
///   block at `y0 = y + (mv_y >> 2)` -- two above for the six-tap's leading
///   taps, three below its last sample, plus the block's own four rows. The
///   deepest row a block reaches is therefore `y + (mv_y >> 2) + 6`.
/// * **Chroma.** [`sample_chroma_2x2`] reads a 3x3 region at
///   `cy0 + (mv_y >> 3)` -- the bilinear's own two rows for each of the two
///   chroma sub-positions -- so the deepest row is `cy0 + (mv_y >> 3) + 2`.
///
/// A reference not read at all in this row is `None`, so a picture nothing in
/// this row predicts from is never waited on. `None` is also what an intra
/// macroblock contributes: its `mv_blocks` are not read by clause 8.4 at all.
#[derive(Debug)]
pub(crate) struct RowReach {
    /// `luma[list][ref_idx]`: the deepest luma row read, or `None`.
    pub(crate) luma: [[Option<u32>; MAX_REF_IDX]; 2],
    /// [`RowReach::luma`]'s chroma counterpart, in chroma rows.
    pub(crate) chroma: [[Option<u32>; MAX_REF_IDX]; 2],
}

/// Derive [`RowReach`] for one macroblock row.
#[allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "a negative reach is clamped to zero by the saturating conversion below; row numbers are u32 by construction"
)]
pub(crate) fn row_reference_reach(row: &[MbSummary]) -> RowReach {
    let mut reach = RowReach { luma: [[None; MAX_REF_IDX]; 2], chroma: [[None; MAX_REF_IDX]; 2] };
    for mb in row {
        if mb.is_intra16x16 || mb.is_intra4x4 || mb.is_intra8x8 || mb.is_ipcm {
            continue;
        }
        for (i, info) in mb.mv_blocks.iter().enumerate() {
            let by = (i >> 2) as u32;
            let y = mb.mb_y.saturating_mul(16).saturating_add(by.saturating_mul(4));
            let cy = mb.mb_y.saturating_mul(8).saturating_add(by.saturating_mul(2));
            for list in 0..2usize {
                let (reads, idx, mv) = if list == 0 {
                    (info.reads_l0(), info.ref_idx_l0(), info.mv_l0())
                } else {
                    (info.reads_l1(), info.ref_idx_l1(), info.mv_l1())
                };
                if !reads {
                    continue;
                }
                let idx = idx.max(0) as usize;
                let mvy = i32::from(mv.1);
                let deep_luma = i64::from(y) + i64::from(mvy >> 2) + 6;
                let deep_chroma = i64::from(cy) + i64::from(mvy >> 3) + 2;
                let put = |slot: &mut Option<u32>, v: i64| {
                    let v = v.clamp(0, i64::from(u32::MAX)) as u32;
                    *slot = Some(slot.map_or(v, |old| old.max(v)));
                };
                if let Some(l) = reach.luma.get_mut(list).and_then(|a| a.get_mut(idx)) {
                    put(l, deep_luma);
                }
                if let Some(c) = reach.chroma.get_mut(list).and_then(|a| a.get_mut(idx)) {
                    put(c, deep_chroma);
                }
            }
        }
    }
    reach
}

/// One list's own raw chroma samples at a 4x4 luma block's own four chroma
/// sub-positions -- [`sample_luma_block`]'s chroma counterpart, returning the
/// 2x2 group together for the same L0/L1-then-combine reason, and because a
/// banded reference is asked for the region they share exactly once.
fn sample_chroma_2x2(
    plane: RefPlane<'_>,
    chroma_width: u32,
    chroma_height: u32,
    cx0: i32,
    cy0: i32,
    mv: (i16, i16),
    scratch: &mut ReadScratch,
) -> [[u8; 2]; 2] {
    #[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss, clippy::cast_possible_truncation, reason = "mirrors this function's own pre-existing arithmetic")]
    {
        let (mvx, mvy) = (i32::from(mv.0), i32::from(mv.1));
        let mut out = [[0u8; 2]; 2];
        // Clause 8.4.2.2.2's bilinear reads `(xIntC, yIntC)` and its right/below
        // neighbour, so the four sub-positions between them span a 3x3 region at
        // `(cx0 + mv >> 3, cy0 + mv >> 3)`.
        let (rx0, ry0) = (cx0 + (mvx >> 3), cy0 + (mvy >> 3));
        let plane = match plane {
            RefPlane::Flat(data) => data,
            RefPlane::Banded(view) => {
                let Ok(b) = view.block(rx0, ry0, 3, 3, &mut scratch.block) else {
                    scratch.failed = true;
                    return out;
                };
                let (data, stride) = (b.data, b.stride);
                let fetch = |ax: i32, ay: i32| -> u8 {
                    let (rx, ry) = ((ax - rx0).max(0) as usize, (ay - ry0).max(0) as usize);
                    data.get(ry * stride + rx).copied().unwrap_or(0)
                };
                for (dy, row) in out.iter_mut().enumerate() {
                    for (dx, v) in row.iter_mut().enumerate() {
                        *v = crate::interp::chroma_mc_sample(fetch, cx0 + dx as i32, cy0 + dy as i32, mvx, mvy);
                    }
                }
                return out;
            }
        };
        let fetch = |ax: i32, ay: i32| -> u8 {
            if plane.is_empty() {
                return 0;
            }
            let cx = ax.clamp(0, chroma_width as i32 - 1) as u32;
            let cy = ay.clamp(0, chroma_height as i32 - 1) as u32;
            plane.get((cy * chroma_width + cx) as usize).copied().unwrap_or(0)
        };
        for (dy, row) in out.iter_mut().enumerate() {
            for (dx, v) in row.iter_mut().enumerate() {
                *v = crate::interp::chroma_mc_sample(fetch, cx0 + dx as i32, cy0 + dy as i32, mvx, mvy);
            }
        }
        out
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::indexing_slicing,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::many_single_char_names,
    reason = "mirrors reconstruct_inter_mb's own allow: blk/i/j are fixed 0..4 or 0..16 loop \
              bounds; mv/pixel arithmetic is checked at the fetch closure's own clamp; x/y/c/d/r \
              mirror clause 8.5's own variable names"
)]
fn predict_chroma_inter(
    mb: &MbSummary,
    comp: usize,
    ref_list0: &[RefPicturePlanes<'_>],
    ref_list1: &[RefPicturePlanes<'_>],
    chroma_width: u32,
    chroma_height: u32,
    weights: InterWeights<'_>,
    scratch: &mut ReadScratch,
) -> [[u8; 8]; 8] {
    let empty = RefPlane::Flat(&[]);
    let mut out = [[0u8; 8]; 8];
    for blk in 0..16u32 {
        let (bx, by) = blk_xy(blk);
        let info = mb.mv_blocks[(by * 4 + bx) as usize];
        let ref_idx0 = info.ref_idx_l0().max(0) as usize;
        let ref_idx1 = info.ref_idx_l1().max(0) as usize;
        let plane0 = ref_list0.get(ref_idx0).map_or(empty, |r| if comp == 0 { r.cb } else { r.cr });
        let plane1 = ref_list1.get(ref_idx1).map_or(empty, |r| if comp == 0 { r.cb } else { r.cr });
        let cx0 = (mb.mb_x * 8 + bx * 2) as i32;
        let cy0 = (mb.mb_y * 8 + by * 2) as i32;
        let p0 = info
            .reads_l0()
            .then(|| sample_chroma_2x2(plane0, chroma_width, chroma_height, cx0, cy0, info.mv_l0(), scratch));
        let p1 = info
            .reads_l1()
            .then(|| sample_chroma_2x2(plane1, chroma_width, chroma_height, cx0, cy0, info.mv_l1(), scratch));

        for dy in 0..2i32 {
            for dx in 0..2i32 {
                let a = p0.map(|m| m[dy as usize][dx as usize]);
                let b = p1.map(|m| m[dy as usize][dx as usize]);
                let v = match (a, b) {
                    (Some(a), Some(b)) => weights.combine(ref_idx0, ref_idx1, a, b),
                    (Some(a), None) => weights.single(0, ref_idx0, a),
                    (None, Some(b)) => weights.single(1, ref_idx1, b),
                    (None, None) => 0,
                };
                let oy = (by * 2) as usize + dy as usize;
                let ox = (bx * 2) as usize + dx as usize;
                if let Some(row) = out.get_mut(oy)
                    && let Some(cell) = row.get_mut(ox)
                {
                    *cell = v;
                }
            }
        }
    }
    out
}

/// [`reconstruct_picture_luma`]/[`reconstruct_picture_with_inter`]'s own
/// per-macroblock dispatch (reusing [`reconstruct_intra16x16_luma`]/
/// [`reconstruct_intra4x4_mb`]/[`reconstruct_inter_mb`] unchanged, exactly
/// as those two functions do), extended with the chroma half neither of
/// those two touch: clause 8.3.3's intra chroma prediction (DC/H/V/Plane)
/// or clause 8.4.2.2.2's bilinear chroma motion compensation, plus clause
/// 8.5.3's chroma residual add -- composed from pieces already
/// independently verified ([`predict_intra_chroma`],
/// [`dequant_chroma_dc_2x2`], [`inverse_scan_chroma_dc`]) rather than new
/// arithmetic, closing the gap this module's own doc used to describe as
/// "written but not yet composed".
///
/// Chroma prediction is independent of which luma branch a macroblock
/// took: clause 8.3.3's `intra_chroma_pred_mode` and clause 8.4.1.4's
/// chroma motion vectors both apply at the whole-macroblock 8x8
/// granularity regardless of whether luma was `Intra_16x16`, `Intra_4x4`
/// or inter, so this function computes it once per macroblock, after
/// (not interleaved with) the luma branch's own per-block work.
///
/// `chroma_qp_offset_{cb,cr}` are the PPS's own
/// `chroma_qp_index_offset`/`second_chroma_qp_index_offset` (clause
/// 8.5.8) -- constant for the whole picture, unlike `QPY` which is
/// per-macroblock.
///
/// # Errors
///
/// As [`reconstruct_picture_luma`]: [`vaco_core::Error::Unsupported`] for
/// `I_PCM`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn reconstruct_picture(
    macroblocks: &[MbSummary],
    mbs_wide: u32,
    mbs_high: u32,
    chroma_qp_offset_cb: i32,
    chroma_qp_offset_cr: i32,
    ref_list0: &[RefPicturePlanes<'_>],
    ref_list1: &[RefPicturePlanes<'_>],
    weights: &SliceWeightTables,
    bipred_mode: BiPredMode,
    implicit: Option<&ImplicitWeights>,
    budget: &mut Budget,
) -> vaco_core::Result<ReconstructedPicture> {
    let ctx = PictureCtx::new(
        mbs_wide,
        mbs_high,
        chroma_qp_offset_cb,
        chroma_qp_offset_cr,
        ref_list0,
        ref_list1,
        weights,
        bipred_mode,
        implicit,
    );
    let mut scratch = ReadScratch::new(budget)?;
    let mut buf = PictureBuffer::new(mbs_wide, mbs_high, budget)?;
    for mb in macroblocks {
        reconstruct_mb(&mut buf, mb, &ctx, &mut scratch)?;
    }
    scratch.check()?;

    Ok(ReconstructedPicture {
        luma: buf.luma,
        cb: buf.cb,
        cr: buf.cr,
    })
}

/// The half of [`reconstruct_mb`]'s inputs that is constant for a whole
/// picture, derived once instead of per macroblock.
///
/// It exists because reconstruction is now driven a macroblock *row* at a time
/// (see [`reconstruct_picture_rows`]), so the derivation that used to sit above
/// the single `for mb in macroblocks` loop needs somewhere to live that outlives
/// one row.
pub(crate) struct PictureCtx<'a> {
    ref_list0: &'a [RefPicturePlanes<'a>],
    ref_list1: &'a [RefPicturePlanes<'a>],
    weights: &'a SliceWeightTables,
    bipred_mode: BiPredMode,
    implicit: Option<&'a ImplicitWeights>,
    chroma_qp_offset_cb: i32,
    chroma_qp_offset_cr: i32,
    ref_width: u32,
    ref_height: u32,
    chroma_width: u32,
    chroma_height: u32,
}

impl<'a> PictureCtx<'a> {
    /// Derive the picture-constant reconstruction state.
    #[allow(clippy::too_many_arguments, reason = "one argument per picture-constant input, all of which reconstruction needs")]
    pub(crate) fn new(
        mbs_wide: u32,
        mbs_high: u32,
        chroma_qp_offset_cb: i32,
        chroma_qp_offset_cr: i32,
        ref_list0: &'a [RefPicturePlanes<'a>],
        ref_list1: &'a [RefPicturePlanes<'a>],
        weights: &'a SliceWeightTables,
        bipred_mode: BiPredMode,
        implicit: Option<&'a ImplicitWeights>,
    ) -> Self {
        Self {
            ref_list0,
            ref_list1,
            weights,
            bipred_mode,
            implicit,
            chroma_qp_offset_cb,
            chroma_qp_offset_cr,
            ref_width: mbs_wide * 16,
            ref_height: mbs_high * 16,
            chroma_width: mbs_wide * 8,
            chroma_height: mbs_high * 8,
        }
    }
}

/// One macroblock's clause 8.4/8.5 reconstruction: the body of what used to be
/// [`reconstruct_picture`]'s single loop, unchanged, hoisted so a caller can
/// drive it a row at a time.
///
/// # Errors
///
/// [`vaco_core::Error::Unsupported`] for `I_PCM`.
fn reconstruct_mb(
    buf: &mut PictureBuffer,
    mb: &MbSummary,
    ctx: &PictureCtx<'_>,
    scratch: &mut ReadScratch,
) -> vaco_core::Result<()> {
    let (ref_width, ref_height) = (ctx.ref_width, ctx.ref_height);
    let (chroma_width, chroma_height) = (ctx.chroma_width, ctx.chroma_height);
    let (chroma_qp_offset_cb, chroma_qp_offset_cr) = (ctx.chroma_qp_offset_cb, ctx.chroma_qp_offset_cr);
    let (ref_list0, ref_list1) = (ctx.ref_list0, ctx.ref_list1);
    let weights = ctx.weights;
    let bipred_mode = ctx.bipred_mode;
    let implicit = ctx.implicit;

    if mb.is_ipcm {
        return Err(vaco_core::Error::Unsupported(
            "vaco-codec-h264: I_PCM picture reconstruction is not implemented",
        ));
    }
    let is_inter = !mb.is_intra16x16 && !mb.is_intra4x4 && !mb.is_intra8x8;
    if mb.is_intra16x16 {
        let x = mb.mb_x * 16;
        let y = mb.mb_y * 16;
        let top_available = (0..16).all(|dx| buf.available(coord(x) + dx, coord(y) - 1));
        let top = core::array::from_fn(|dx| buf.pixel(coord(x) + coord(dx), coord(y) - 1));
        let left_available = (0..16).all(|dy| buf.available(coord(x) - 1, coord(y) + dy));
        let left = core::array::from_fn(|dy| buf.pixel(coord(x) - 1, coord(y) + coord(dy)));
        let neighbours = Neighbours16 {
            top_available,
            top,
            left_available,
            left,
            corner: buf.pixel(coord(x) - 1, coord(y) - 1),
        };
        let block = reconstruct_intra16x16_luma(mb.intra16x16_pred_mode, neighbours, mb.qpy, &mb.residual);
        for (i, row) in block.iter().enumerate() {
            buf.write_row_luma(x, y + i as u32, row);
        }
        for blk in 0..16u32 {
            let (bx, by) = blk_xy(blk);
            buf.mark_block_decoded(x + bx * 4, y + by * 4);
        }
    } else if mb.is_intra4x4 {
        reconstruct_intra4x4_mb(buf, mb.mb_x, mb.mb_y, mb.qpy, &mb.residual);
    } else if mb.is_intra8x8 {
        reconstruct_intra8x8_mb(buf, mb.mb_x, mb.mb_y, mb.qpy, &mb.residual);
    } else {
        let luma_weights = InterWeights { l0: weights.luma(), l1: weights.luma1(), mode: bipred_mode, implicit };
        reconstruct_inter_mb(buf, mb, ref_list0, ref_list1, ref_width, ref_height, luma_weights, scratch);
    }

    let qpc_cb = chroma_qp(mb.qpy, chroma_qp_offset_cb);
    let qpc_cr = chroma_qp(mb.qpy, chroma_qp_offset_cr);
    for (comp, qpc) in [(0usize, qpc_cb), (1usize, qpc_cr)] {
        let pred = if is_inter {
            let chroma_weights =
                InterWeights { l0: weights.chroma(comp), l1: weights.chroma1(comp), mode: bipred_mode, implicit };
            predict_chroma_inter(mb, comp, ref_list0, ref_list1, chroma_width, chroma_height, chroma_weights, scratch)
        } else {
            let neighbours = chroma_neighbours(buf, comp, mb.mb_x, mb.mb_y);
            predict_intra_chroma(mb.intra_chroma_pred_mode, neighbours)
        };
        let out = add_chroma_residual(pred, comp, mb, qpc);
        let x0 = mb.mb_x * 8;
        let y0 = mb.mb_y * 8;
        for (i, row) in out.iter().enumerate() {
            buf.write_row_chroma(comp, x0, y0 + i as u32, row);
        }
    }
    buf.mark_chroma_mb_decoded(mb.mb_x, mb.mb_y);
    Ok(())
}



/// Clause 8.4/8.5 reconstruction and clause 8.7 deblocking, driven a macroblock
/// row at a time so a caller can publish finished rows while the picture is
/// still being produced.
///
/// # Why deblocking must lag reconstruction by exactly one macroblock row
///
/// Clause 8.3's intra prediction is defined on **unfiltered** neighbours, and
/// the only ones it reads above the current macroblock row are the single luma
/// row `my * 16 - 1` and chroma row `my * 8 - 1`. Filtering macroblock row
/// `my - 1` rewrites exactly those rows (its vertical edges touch every one of
/// its own sixteen luma rows, the last of which *is* `my * 16 - 1`), so it must
/// not run until row `my` has been reconstructed. Filtering row `my - 1` needs
/// nothing from row `my`, so one row of lag is both necessary and sufficient --
/// and no copy of the unfiltered row is needed, which is the reason to pick this
/// schedule over saving a top-border row the way a lag-zero schedule would have
/// to.
///
/// # Why a row is final only after the *next* row is filtered
///
/// Filtering macroblock row `d` writes upwards into the row above it: luma's top
/// macroblock edge at `y = d * 16` rewrites `p0`/`p1`/`p2`, i.e. rows
/// `d * 16 - 1`, `- 2` and `- 3`; chroma's rewrites `p0` alone, row `d * 8 - 1`.
/// So once row `d` is filtered, [`luma_rows_final`] and [`chroma_rows_final`]
/// rows of each plane are final -- everything below that overhang is still row
/// `d + 1`'s to modify.
#[derive(Debug)]
pub(crate) struct PictureReconstructor {
    buf: PictureBuffer,
    scratch: ReadScratch,
    /// Index of the next macroblock to reconstruct, so a row can be found in
    /// one pass rather than by scanning `macroblocks` for `mb_y == my`.
    cursor: usize,
}

/// Luma rows that no later filtering can touch, once macroblock row `d` has
/// been filtered. See [`PictureReconstructor`]'s own doc for the derivation.
pub(crate) const fn luma_rows_final(d: u32) -> u32 {
    d.saturating_mul(16).saturating_add(13)
}

/// [`luma_rows_final`]'s chroma counterpart: chroma's filter modifies `p0` and
/// nothing further above, so the overhang is one row rather than three.
pub(crate) const fn chroma_rows_final(d: u32) -> u32 {
    d.saturating_mul(8).saturating_add(7)
}

/// Whether `macroblocks` is exactly one complete picture in raster order.
///
/// The row schedule assumes it, which is what `crate::mb`'s own decode loop
/// produces. A caller that gets `false` must fall back to
/// [`PictureReconstructor::reconstruct_all`] plus whole-picture filtering,
/// which is order-independent and therefore always right -- rather than
/// silently reconstructing a subset.
pub(crate) fn macroblocks_in_raster_order(macroblocks: &[MbSummary], mbs_wide: u32, mbs_high: u32) -> bool {
    macroblocks.len() == (mbs_wide as usize).saturating_mul(mbs_high as usize)
        && macroblocks
            .iter()
            .enumerate()
            .all(|(i, mb)| (mb.mb_y.saturating_mul(mbs_wide).saturating_add(mb.mb_x) as usize) == i)
}

impl PictureReconstructor {
    /// Allocate the working picture.
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::LimitExceeded`] when the budget refuses.
    pub(crate) fn new(mbs_wide: u32, mbs_high: u32, budget: &mut Budget) -> vaco_core::Result<Self> {
        let scratch = ReadScratch::new(budget)?;
        let buf = PictureBuffer::new(mbs_wide, mbs_high, budget)?;
        Ok(Self { buf, scratch, cursor: 0 })
    }

    /// Reconstruct every macroblock of row `my`.
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::Unsupported`] for `I_PCM`, or
    /// [`vaco_core::Error::InvalidData`] if a reference read reached past the
    /// rows published so far -- which means the caller did not wait far enough.
    pub(crate) fn reconstruct_row(
        &mut self,
        macroblocks: &[MbSummary],
        my: u32,
        ctx: &PictureCtx<'_>,
    ) -> vaco_core::Result<()> {
        while let Some(mb) = macroblocks.get(self.cursor) {
            if mb.mb_y != my {
                break;
            }
            reconstruct_mb(&mut self.buf, mb, ctx, &mut self.scratch)?;
            self.cursor += 1;
        }
        self.scratch.check()
    }

    /// Reconstruct every macroblock, whatever order they arrive in.
    ///
    /// # Errors
    ///
    /// As [`PictureReconstructor::reconstruct_row`].
    pub(crate) fn reconstruct_all(
        &mut self,
        macroblocks: &[MbSummary],
        ctx: &PictureCtx<'_>,
    ) -> vaco_core::Result<()> {
        for mb in macroblocks {
            reconstruct_mb(&mut self.buf, mb, ctx, &mut self.scratch)?;
        }
        self.cursor = macroblocks.len();
        self.scratch.check()
    }

    /// Filter macroblock row `my` of all three planes.
    ///
    /// Luma and chroma are independent -- clause 8.7 derives chroma's boundary
    /// strength from the macroblock coding modes, never from luma samples -- so
    /// the order between the three calls does not affect the result.
    pub(crate) fn deblock_row(
        &mut self,
        deblock: &crate::deblock::DeblockCtx<'_>,
        my: u32,
        chroma_qp_offset_cb: i32,
        chroma_qp_offset_cr: i32,
    ) {
        deblock.luma_mb_row(&mut self.buf.luma, my);
        deblock.chroma_mb_row(&mut self.buf.cb, chroma_qp_offset_cb, my);
        deblock.chroma_mb_row(&mut self.buf.cr, chroma_qp_offset_cr, my);
    }

    /// The three planes as they stand.
    pub(crate) fn planes(&self) -> (&[u8], &[u8], &[u8]) {
        (&self.buf.luma, &self.buf.cb, &self.buf.cr)
    }

    /// The finished picture.
    pub(crate) fn finish(self) -> ReconstructedPicture {
        ReconstructedPicture { luma: self.buf.luma, cb: self.buf.cb, cr: self.buf.cr }
    }

    /// `(mbs_wide, mbs_high)` this reconstructor's three sample planes and
    /// bookkeeping bitmaps are sized for -- what `crate::task_pool` keys its
    /// free list on, since a buffer sized for one resolution cannot serve
    /// another.
    pub(crate) const fn geometry(&self) -> (u32, u32) {
        (self.buf.mbs_wide, self.buf.mbs_high)
    }

    /// Bytes charged for [`Self::buf`]'s three real sample planes -- what a
    /// pooled reuse must hand `Budget::charge` instead of the
    /// `Budget::alloc` calls [`PictureBuffer::new`] would otherwise make.
    /// [`Self::buf`]'s two bookkeeping bitmaps are not included, matching
    /// [`PictureBuffer::new`]'s own doc: they never leave this type, so
    /// nothing outside it needs to account for them.
    pub(crate) fn charged_bytes(&self) -> u64 {
        (self.buf.luma.len())
            .saturating_add(self.buf.cb.len())
            .saturating_add(self.buf.cr.len()) as u64
    }

    /// Put a finished reconstructor back into the state
    /// [`PictureReconstructor::new`] would produce for the *same* geometry,
    /// without reallocating any of its three sample planes, its two
    /// bookkeeping bitmaps, or its scratch block.
    ///
    /// Pooled reuse (`crate::task_pool::TaskBufferPools`) is only ever
    /// offered a reconstructor whose geometry already matches the request
    /// (the pool clears its free list on a geometry change), so there is
    /// nothing here to resize.
    pub(crate) fn reset(&mut self) {
        self.buf.luma.fill(128);
        self.buf.decoded_4x4.fill(false);
        self.buf.cb.fill(128);
        self.buf.cr.fill(128);
        self.buf.chroma_decoded.fill(false);
        self.scratch.reset();
        self.cursor = 0;
    }
}


#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code"
)]
mod tests {
    use super::*;

    /// Clause 8.4.2.3.2's explicit single-list weighting, pinned to the
    /// exact parameters that exposed it: a 25-frame 416x240 High-profile
    /// `libx264` encode of ffmpeg's `life` source picked
    /// `luma_log2_weight_denom = 4`, `luma_weight_l0[0] = 15`,
    /// `luma_offset_l0[0] = -3`, and *every* inter macroblock of its first
    /// P picture reconstructed wrong while this was ignored. The expected
    /// values here were derived independently -- by fitting `(logWD, w, o)`
    /// to 1888 coefficient-free predicted samples of that picture taken
    /// from `ffmpeg`'s own decode -- not by running this function.
    #[test]
    fn explicit_weighted_prediction_matches_the_measured_x264_parameters() {
        let w = PredWeight { log2_denom: 4, weight: 15, offset: -3 };
        // ((p * 15 + 8) >> 4) - 3, clipped.
        assert_eq!(w.apply(4), 1);
        assert_eq!(w.apply(0), 0, "clipped at the bottom, not wrapped");
        assert_eq!(w.apply(255), 236);
        assert_eq!(w.apply(128), 117);
    }

    /// `logWD == 0` takes clause 8.4.2.3.2's *other* branch: no rounding
    /// term and no shift at all, which is a different expression, not the
    /// same one with a zero shift.
    #[test]
    fn a_zero_log2_denominator_skips_the_rounding_term() {
        let w = PredWeight { log2_denom: 0, weight: 1, offset: 7 };
        assert_eq!(w.apply(10), 17);
        let doubling = PredWeight { log2_denom: 0, weight: 2, offset: 0 };
        assert_eq!(doubling.apply(100), 200);
        assert_eq!(doubling.apply(200), 255, "Clip1 saturates rather than wrapping");
    }

    /// The neutral weight is an exact identity for every sample value --
    /// this is why ignoring `pred_weight_table()` looked correct on every
    /// fixture whose encoder happened to choose neutral weights.
    #[test]
    fn the_neutral_weight_is_an_exact_identity() {
        for denom in 0..=7u8 {
            let w = PredWeight { log2_denom: denom, weight: 1 << denom, offset: 0 };
            for v in 0..=255u8 {
                assert_eq!(w.apply(v), v, "denom={denom} v={v}");
            }
        }
    }

    /// A reference whose `luma_weight_l0_flag` was 0 must come back as
    /// that identity (clause 7.4.3.2's inferred values), not as a missing
    /// entry that would silently fall back to unweighted prediction for
    /// that one reference while the others are weighted.
    #[test]
    fn an_absent_per_reference_flag_infers_the_neutral_weight() {
        let table = vaco_parse_h264::slice::PredWeightTable {
            luma_log2_weight_denom: 5,
            chroma_log2_weight_denom: Some(6),
            l0: vec![
                vaco_parse_h264::slice::RefWeight { luma: Some((20, -4)), chroma: None },
                vaco_parse_h264::slice::RefWeight { luma: None, chroma: None },
            ],
            l1: Vec::new(),
        };
        let w = SliceWeightTables::from_table(Some(&table));
        let luma = w.luma().expect("weighted");
        assert_eq!((luma[0].log2_denom, luma[0].weight, luma[0].offset), (5, 20, -4));
        assert_eq!((luma[1].log2_denom, luma[1].weight, luma[1].offset), (5, 32, 0));
        let cb = w.chroma(0).expect("weighted");
        assert_eq!((cb[0].log2_denom, cb[0].weight, cb[0].offset), (6, 64, 0));
        // No table at all is distinct from a table of neutral weights.
        assert!(SliceWeightTables::from_table(None).luma().is_none());
    }
    use crate::cabac_residual::CabacResidual;

    fn unavailable() -> Neighbours16 {
        Neighbours16 {
            top_available: false,
            top: [0; 16],
            left_available: false,
            left: [0; 16],
            corner: 0,
        }
    }

    /// No residual at all reduces to plain prediction -- the flat
    /// fixture's own case, re-checked here at this module's own level
    /// rather than only in `intra.rs`.
    #[test]
    fn zero_residual_is_pure_prediction() {
        let out = reconstruct_intra16x16_luma(2, unavailable(), 26, &MbResidual::default());
        assert!(out.iter().all(|row| row.iter().all(|&v| v == 128)));
    }

    /// A single luma DC coefficient, no AC at all, must shift every
    /// sample in the macroblock by the same amount (clause 8.5.2's dcY is
    /// shared by all 16 blocks; with no AC, `idct4x4` of a DC-only input
    /// is a flat block per clause 8.5.12.2's own separable sum).
    #[test]
    fn dc_only_residual_shifts_every_sample_uniformly() {
        let residual = MbResidual {
            luma_dc: Some(CabacResidual {
                levels: vec![10],
                positions: vec![0],
            }),
            ..Default::default()
        };
        let out = reconstruct_intra16x16_luma(2, unavailable(), 26, &residual);
        let first = out[0][0];
        assert!(
            out.iter().all(|row| row.iter().all(|&v| v == first)),
            "a DC-only residual must reconstruct to a single flat value, got {out:?}"
        );
        // Not a no-op: some real shift away from the pure-prediction 128
        // must have happened, or this test could not tell a DC add from a
        // dropped residual.
        assert_ne!(first, 128);
    }

    /// The first non-flat pixel comparison this investigation has had:
    /// `cabac_intra_oracle_gradient.264` (one macroblock, forced
    /// `Intra_16x16` DC with real, nonzero luma residual -- `partitions=none`
    /// alone was not enough to stop `libx264` choosing `Intra_4x4` on a
    /// smooth gradient; `preset=ultrafast` was, per its own restricted
    /// intra analysis) decodes end to end through the *live* CABAC path
    /// (`crate::mb::decode_slice_cabac`) into this module's
    /// `reconstruct_intra16x16_luma`, and is checked against `ffmpeg
    /// 8.1`'s own real decode of the same file, saved once as
    /// `cabac_intra_oracle_gradient_ref.yuv`
    /// (`ffmpeg -i ... -pix_fmt yuv420p -f rawvideo`). Chroma is not
    /// reconstructed here (this module's own scope line) -- confirmed
    /// separately, off the same live decode, that this fixture's own
    /// `CodedBlockPatternChroma == 0`, so the reference file's chroma
    /// planes (also checked below, directly, not just assumed flat) never
    /// needed anything past this crate's already-verified chroma DC
    /// prediction.
    ///
    /// Isolates residual correctness from prediction correctness on
    /// purpose: `Intra16x16PredMode` is DC, already covered by the flat
    /// fixture with zero residual -- the only new thing this test
    /// exercises is dequantisation + the inverse transform, on real,
    /// nonzero coefficients decoded off the actual bitstream.
    #[test]
    fn gradient_fixture_luma_matches_real_ffmpeg_byte_for_byte() {
        use vaco_bitstream::{BitReader, annexb};
        use vaco_codec_cabac::CabacDecoder;
        use vaco_format_nalu::RbspBuf;
        use vaco_limits::{Budget, Limits};
        use vaco_parse_h264::{H264NalHeader, NalUnitType, ParameterSets, SliceHeader};

        let data: &[u8] = include_bytes!("../tests/fixtures/cabac_intra_oracle_gradient.264");
        let reference: &[u8] =
            include_bytes!("../tests/fixtures/cabac_intra_oracle_gradient_ref.yuv");
        assert_eq!(
            reference.len(),
            384,
            "reference fixture: expected 16x16 4:2:0 (256 + 64 + 64 bytes)"
        );

        let mut params = ParameterSets::new();
        let mut budget = Budget::new(Limits::default());
        let mut rbsp = RbspBuf::new();
        let mut stats = None;

        for nal in annexb::nal_units(data) {
            let Some(header) = H264NalHeader::parse(nal) else {
                continue;
            };
            match header.nal_unit_type {
                NalUnitType::Sps => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let _ = params.add_sps(rbsp.as_slice(), &mut budget);
                }
                NalUnitType::Pps => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let _ = params.add_pps(rbsp.as_slice(), &mut budget);
                }
                NalUnitType::IdrSlice | NalUnitType::NonIdrSlice => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let payload = rbsp.as_slice();
                    let mut reader = BitReader::new(payload);
                    reader.skip(8);
                    let pps_id = {
                        let mut r2 = BitReader::new(payload);
                        r2.skip(8);
                        let mut g = vaco_codec_golomb::BoundedGolomb::new(&mut r2, &mut budget);
                        let _ = g.ue_v(u32::MAX).unwrap();
                        let _ = g.ue_v(9).unwrap();
                        g.ue_v(255).unwrap() as u8
                    };
                    let (pps, sps) = params.sps_for_pps(pps_id).unwrap();
                    let slice_header =
                        SliceHeader::parse_data(&mut reader, header, sps, pps, &mut budget)
                            .unwrap();
                    let mut cabac = CabacDecoder::from_reader(reader);
                    let s = crate::mb::decode_slice_cabac(
                        &mut cabac,
                        &mut budget,
                        sps,
                        pps,
                        &slice_header,
                        None,
                    )
                    .unwrap_or_else(|e| {
                        panic!("gradient fixture: decode_slice_cabac failed: {e:?}")
                    });
                    assert!(
                        !cabac.malformed(),
                        "gradient fixture: CABAC engine reported malformed input"
                    );
                    stats = Some(s);
                }
                _ => {}
            }
        }

        let stats = stats.expect("gradient fixture: no slice NAL found");
        assert_eq!(
            stats.macroblock_count, 1,
            "gradient fixture: expected exactly one macroblock"
        );
        assert_eq!(
            stats.first_slice_mb_intra16x16_pred_mode,
            Some(2),
            "gradient fixture: expected Intra16x16PredMode == 2 (DC), same mode as the flat fixture"
        );
        let (cbp_luma, cbp_chroma) = stats
            .first_slice_mb_cbp
            .expect("gradient fixture: no coded_block_pattern recorded");
        assert_ne!(
            cbp_luma, 0,
            "gradient fixture: expected nonzero luma CBP -- this fixture exists to exercise residual decode"
        );
        assert_eq!(
            cbp_chroma, 0,
            "gradient fixture: expected zero chroma CBP -- this test does not reconstruct chroma residual"
        );
        let qpy = stats
            .first_slice_mb_qpy
            .expect("gradient fixture: no QPY recorded");
        let residual = stats
            .first_slice_mb_residual
            .expect("gradient fixture: no residual recorded");

        let luma = reconstruct_intra16x16_luma(2, unavailable(), qpy, &residual);
        let mut mismatches = Vec::new();
        for y in 0..16usize {
            for x in 0..16usize {
                let got = luma[y][x];
                let want = reference[y * 16 + x];
                if got != want {
                    mismatches.push((x, y, got, want));
                }
            }
        }
        assert!(
            mismatches.is_empty(),
            "gradient fixture: luma reconstruction diverges from real ffmpeg at {} of 256 samples \
             (first few (x, y, got, want): {:?})",
            mismatches.len(),
            &mismatches[..mismatches.len().min(8)]
        );

        // Chroma is not reconstructed by this module (see its own doc) --
        // confirmed, not assumed, that the reference decode's chroma is
        // flat 128 throughout, matching this crate's already-verified
        // "zero CBP, DC prediction, no neighbours" case from the flat
        // fixture, so no chroma residual path is owed to this comparison.
        let chroma = &reference[256..384];
        assert!(
            chroma.iter().all(|&v| v == 128),
            "gradient fixture: expected flat 128 chroma in the reference decode (CodedBlockPatternChroma == 0)"
        );
    }

    /// Decodes every CABAC I-slice in `data` (each one, for this crate's
    /// oracle corpus, its own complete standalone picture -- confirmed
    /// structurally for every fixture used below, not assumed: every
    /// slice has `first_mb_in_slice == 0`) and reconstructs each one's
    /// luma plane. Panics (via `.expect`/`.unwrap`, this module's own
    /// test-code allow) on any parse or decode failure -- there is no
    /// "partial" result worth returning to a fixture-comparison test.
    fn decode_all_frames_luma(data: &[u8], apply_deblocking: bool) -> Vec<(u32, u32, Vec<u8>)> {
        decode_all_frames_luma_tolerant(data, apply_deblocking)
            .into_iter()
            .enumerate()
            .map(|(i, r)| r.unwrap_or_else(|e| panic!("frame {i}: {e}")))
            .collect()
    }

    /// Same as [`decode_all_frames_luma`], but never panics -- one
    /// slice's own decode/reconstruction failure (e.g. `malformed()`)
    /// becomes an `Err` for that one frame instead of aborting the whole
    /// file, so a corpus with one bad frame among many still reports
    /// every other frame's own comparison. Used where a fixture is not
    /// (yet) expected to decode cleanly end to end -- see
    /// `cabac_i_only_reconstructs_without_error_and_mostly_matches_ffmpeg`.
    fn decode_all_frames_luma_tolerant(
        data: &[u8],
        apply_deblocking: bool,
    ) -> Vec<Result<(u32, u32, Vec<u8>), String>> {
        use vaco_bitstream::{BitReader, annexb};
        use vaco_codec_cabac::CabacDecoder;
        use vaco_format_nalu::RbspBuf;
        use vaco_limits::{Budget, Limits};
        use vaco_parse_h264::{H264NalHeader, NalUnitType, ParameterSets, SliceHeader};

        let mut params = ParameterSets::new();
        let mut budget = Budget::new(Limits::default());
        let mut rbsp = RbspBuf::new();
        let mut frames = Vec::new();

        for nal in annexb::nal_units(data) {
            let Some(header) = H264NalHeader::parse(nal) else {
                continue;
            };
            match header.nal_unit_type {
                NalUnitType::Sps => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let _ = params.add_sps(rbsp.as_slice(), &mut budget);
                }
                NalUnitType::Pps => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let _ = params.add_pps(rbsp.as_slice(), &mut budget);
                }
                NalUnitType::IdrSlice | NalUnitType::NonIdrSlice => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let payload = rbsp.as_slice();
                    let mut reader = BitReader::new(payload);
                    reader.skip(8);
                    let pps_id = {
                        let mut r2 = BitReader::new(payload);
                        r2.skip(8);
                        let mut g = vaco_codec_golomb::BoundedGolomb::new(&mut r2, &mut budget);
                        let _ = g.ue_v(u32::MAX).unwrap();
                        let _ = g.ue_v(9).unwrap();
                        g.ue_v(255).unwrap() as u8
                    };
                    let (pps, sps) = params.sps_for_pps(pps_id).unwrap();
                    let slice_header =
                        SliceHeader::parse_data(&mut reader, header, sps, pps, &mut budget)
                            .unwrap();
                    assert_eq!(
                        slice_header.first_mb_in_slice, 0,
                        "this helper assumes one slice == one whole picture"
                    );
                    let mbs_wide = sps.pic_width_in_mbs;
                    let mbs_high =
                        sps.pic_height_in_map_units * if sps.frame_mbs_only { 1 } else { 2 };
                    let mut cabac = CabacDecoder::from_reader(reader);
                    let result = crate::mb::decode_slice_cabac(
                        &mut cabac,
                        &mut budget,
                        sps,
                        pps,
                        &slice_header,
                        None,
                    )
                    .map_err(|e| format!("decode_slice_cabac failed: {e:?}"))
                    .and_then(|stats| {
                        if cabac.malformed() {
                            return Err("CABAC engine reported malformed input".to_owned());
                        }
                        let mut luma =
                            reconstruct_picture_luma(&stats.macroblocks, mbs_wide, mbs_high, &mut budget)
                                .map_err(|e| format!("reconstruct_picture_luma failed: {e:?}"))?;
                        if apply_deblocking {
                            crate::deblock::deblock_picture_luma(
                                &mut luma,
                                &stats.macroblocks,
                                mbs_wide,
                                mbs_high,
                                slice_header.disable_deblocking_filter_idc,
                                slice_header.slice_alpha_c0_offset_div2,
                                slice_header.slice_beta_offset_div2,
                                &[],
                                &[],
                            )
                            .map_err(|e| format!("deblock_picture_luma failed: {e:?}"))?;
                        }
                        Ok((mbs_wide, mbs_high, luma))
                    });
                    frames.push(result);
                }
                _ => {}
            }
        }
        frames
    }

    /// Compares one reconstructed luma plane against its reference,
    /// asserting a byte-exact match and reporting the first differing
    /// macroblock (not just the first differing byte) if it does not
    /// match -- the instrument this investigation has never had before
    /// this round.
    fn assert_luma_matches(
        name: &str,
        frame_idx: usize,
        ours: &[u8],
        reference: &[u8],
        mbs_wide: u32,
    ) {
        assert_eq!(
            ours.len(),
            reference.len(),
            "{name} frame {frame_idx}: luma plane size mismatch"
        );
        let mut first_mismatch = None;
        let mut mismatches = 0usize;
        for (i, (&a, &b)) in ours.iter().zip(reference.iter()).enumerate() {
            if a != b {
                mismatches += 1;
                if first_mismatch.is_none() {
                    let width = (mbs_wide * 16) as usize;
                    let (x, y) = (i % width, i / width);
                    let (mb_x, mb_y) = (x / 16, y / 16);
                    first_mismatch = Some((x, y, mb_x, mb_y, a, b));
                }
            }
        }
        assert!(
            mismatches == 0,
            "{name} frame {frame_idx}: {mismatches} of {} luma samples differ from ffmpeg; \
             first mismatch at pixel {:?} (x, y, mb_x, mb_y, ours, ffmpeg)",
            ours.len(),
            first_mismatch
        );
    }

    /// `cabac_intra_oracle_testsrc.264`: mixed `Intra_16x16`/`Intra_4x4`
    /// content (libx264's own log: 25%/75%), no deblocking -- the first
    /// clean (unconfounded by the loop filter this crate does not
    /// implement) multi-macroblock comparison exercising *both*
    /// prediction families and real cross-macroblock neighbour
    /// propagation in the same picture. Now byte-exact after two fixes:
    /// (1) `decode_residual_cabac`'s same-macroblock `coded_block_flag`
    /// neighbour lookups no longer route through `grids.mb_info_at`,
    /// which is (correctly) `None` until `set_mb_info` runs at the very
    /// end of `decode_macroblock_cabac`; and (2) luma DC's own
    /// `coded_block_flag` (`ctxBlockCat == 0`, one flag per macroblock)
    /// no longer aliases luma4x4BlkIdx 0's own AC `coded_block_flag`
    /// slot in the shared per-4x4-block `cbf_luma` grid -- that aliasing
    /// let an `Intra_16x16` macroblock's *AC block 0* flag silently
    /// stand in for its *DC* flag whenever a later `Intra_16x16`
    /// neighbour asked for it, invisible until the first
    /// `Intra_16x16`-to-`Intra_16x16` macroblock adjacency in a decode
    /// (macroblock (2, 1)'s own left neighbour, macroblock (1, 1), here).
    /// See `CabacGrids::cbf_luma_dc` for the dedicated, macroblock-
    /// granular storage that replaced the aliased lookup.
    #[test]
    fn testsrc_fixture_matches_ffmpeg_byte_for_byte() {
        let data: &[u8] = include_bytes!("../tests/fixtures/cabac_intra_oracle_testsrc.264");
        let reference: &[u8] =
            include_bytes!("../tests/fixtures/cabac_intra_oracle_testsrc_ref.yuv");
        let frames = decode_all_frames_luma(data, true);
        assert_eq!(frames.len(), 1);
        let (mbs_wide, _mbs_high, luma) = &frames[0];
        assert_luma_matches("testsrc", 0, luma, &reference[..luma.len()], *mbs_wide);
    }

    /// `cabac_intra_oracle_noise.264`: independent random noise, almost
    /// entirely `Intra_4x4` (libx264's own log: `I16..4: 0.0% 0.0%
    /// 100.0%`), no deblocking -- the densest residual-decode stress case
    /// this crate's oracle corpus has, now reconstructable end to end.
    #[test]
    fn noise_fixture_matches_ffmpeg_byte_for_byte() {
        let data: &[u8] = include_bytes!("../tests/fixtures/cabac_intra_oracle_noise.264");
        let reference: &[u8] = include_bytes!("../tests/fixtures/cabac_intra_oracle_noise_ref.yuv");
        let frames = decode_all_frames_luma(data, true);
        assert_eq!(frames.len(), 1);
        let (mbs_wide, _mbs_high, luma) = &frames[0];
        assert_luma_matches("noise", 0, luma, &reference[..luma.len()], *mbs_wide);
    }

    /// `cabac_intra_oracle_multi.264`: five independent IDR pictures, one
    /// slice each, no deblocking -- checks the "each slice is decoded
    /// with entirely fresh neighbour state" assumption
    /// [`decode_all_frames_luma`] leans on holds across multiple pictures
    /// in one file, not just within a single one.
    #[test]
    fn multi_fixture_matches_ffmpeg_byte_for_byte_on_every_frame() {
        let data: &[u8] = include_bytes!("../tests/fixtures/cabac_intra_oracle_multi.264");
        let reference: &[u8] = include_bytes!("../tests/fixtures/cabac_intra_oracle_multi_ref.yuv");
        let frames = decode_all_frames_luma(data, true);
        assert_eq!(frames.len(), 5, "expected five independent IDR pictures");
        let frame_stride = 64 * 64 + 2 * 32 * 32;
        for (idx, (mbs_wide, _mbs_high, luma)) in frames.iter().enumerate() {
            let ref_frame = &reference[idx * frame_stride..idx * frame_stride + luma.len()];
            assert_luma_matches("multi", idx, luma, ref_frame, *mbs_wide);
        }
    }

    /// `cabac_i_only.264`: all `Intra_4x4`, 25
    /// independent IDR pictures -- **not** encoded with deblocking
    /// disabled (`disable_deblocking_filter_idc == 0` on every slice,
    /// confirmed structurally, not assumed), and this crate implements no
    /// deblocking filter at all, so a byte-exact match against `ffmpeg`'s
    /// real (deblocked) decode is not the achievable bar here the way it
    /// is for the four `no-deblock` fixtures above.
    ///
    /// The test below this one settles the question that used to hang
    /// over this corpus: decoded instead against `ffmpeg -skip_loop_filter
    /// all` (deblocking disabled at decode time, not re-encoded), this
    /// crate's own output is **byte-exact**, all 25 frames -- the
    /// previously-reported 63.77% mismatch against the real (deblocked)
    /// reference below was entirely the missing loop filter, not a
    /// decode defect. This crate's `Intra_4x4` reconstruction has no
    /// remaining known defect on this corpus at all.
    #[test]
    fn cabac_i_only_matches_ffmpeg_with_deblocking_skipped() {
        let data: &[u8] = include_bytes!("../tests/fixtures/cabac_i_only.264");
        let reference: &[u8] = include_bytes!("../tests/fixtures/cabac_i_only_nodeblock_ref.yuv");
        let frames = decode_all_frames_luma(data, false);
        assert_eq!(frames.len(), 25);
        let frame_stride = 64 * 64 + 2 * 32 * 32;
        for (idx, (mbs_wide, _mbs_high, luma)) in frames.iter().enumerate() {
            let ref_frame = &reference[idx * frame_stride..idx * frame_stride + luma.len()];
            assert_luma_matches("cabac_i_only (no deblock)", idx, luma, ref_frame, *mbs_wide);
        }
    }

    /// The same 25-frame, all-`Intra_4x4` corpus as
    /// [`cabac_i_only_matches_ffmpeg_with_deblocking_skipped`], but
    /// through [`decode_ip_stream_yuv`] (the same driver
    /// [`crate::decoder::H264Decoder`] itself is built on) and checking
    /// Y, U and V independently -- this crate's densest available
    /// exercise of the chroma intra path (DC/Horizontal/Vertical/Plane,
    /// real per-macroblock texture) against real, un-deblocked `ffmpeg`
    /// output.
    #[test]
    fn cabac_i_only_chroma_matches_ffmpeg_per_plane_with_deblocking_skipped() {
        let data: &[u8] = include_bytes!("../tests/fixtures/cabac_i_only.264");
        let reference: &[u8] = include_bytes!("../tests/fixtures/cabac_i_only_nodeblock_ref.yuv");
        let frames = decode_ip_stream_yuv(data);
        assert_eq!(frames.len(), 25);
        let (luma_len, chroma_len) = (64 * 64, 32 * 32);
        let frame_stride = luma_len + 2 * chroma_len;
        let mut mismatch = [0usize; 3];
        let mut total = [0usize; 3];
        for (idx, frame) in frames.iter().enumerate() {
            let pic = frame
                .as_ref()
                .unwrap_or_else(|e| panic!("cabac_i_only frame {idx}: {e}"));
            let base = idx * frame_stride;
            let planes: [(&[u8], &[u8]); 3] = [
                (&pic.luma, &reference[base..base + luma_len]),
                (&pic.cb, &reference[base + luma_len..base + luma_len + chroma_len]),
                (
                    &pic.cr,
                    &reference[base + luma_len + chroma_len..base + luma_len + 2 * chroma_len],
                ),
            ];
            for (p, (ours, refp)) in planes.iter().enumerate() {
                for (&a, &b) in ours.iter().zip(refp.iter()) {
                    total[p] += 1;
                    if a != b {
                        mismatch[p] += 1;
                    }
                }
            }
        }
        for (p, name) in ["Y", "Cb", "Cr"].iter().enumerate() {
            let pct = if total[p] == 0 {
                0.0
            } else {
                100.0 * (1.0 - mismatch[p] as f64 / total[p] as f64)
            };
            eprintln!(
                "cabac_i_only (no deblock) plane {name}: {} / {} samples differ ({pct:.2}% match)",
                mismatch[p], total[p]
            );
        }
        assert_eq!(mismatch, [0, 0, 0], "byte-exact against ffmpeg -skip_loop_filter all, every plane");
    }

    /// The same corpus against `ffmpeg`'s own real, deblocked decode --
    /// now exercising a real (scalar, luma-only, intra-only-`bS`) clause
    /// 8.7 deblocking filter ([`crate::deblock::deblock_picture_luma`],
    /// built on [`vaco_codec_dsp_deblock`]'s edge/line primitives) rather
    /// than measuring the confound of not having one at all.
    #[test]
    #[ignore = "known incomplete, substantially closed this round rather than merely explained: \
        landing a real clause 8.7 deblocking filter (vaco-codec-dsp-deblock's scalar edge/line \
        primitives, wired in by crate::deblock for the all-intra case -- bS = 4 at macroblock \
        edges, bS = 3 internal, per Table 8-18 collapsed to its all-intra case; inter-macroblock \
        bS and chroma deblocking are both out of scope, the same explicit-not-merely-unimplemented \
        shape this crate uses elsewhere) brought this test from 63.77% to 98.97% match -- 0/25 \
        frames still fail outright, and frame 0 plus frames 16-24 are now fully byte-exact \
        (every one of their 4096 luma samples). The remaining ~1% was hand-traced, not left \
        unexamined: a narrow, consistent off-by-one in the normal (bS < 4) luma filter's \
        tC0-clipped branch (p1'/q1' via Clip3(-tC0, tC0, ...)). Tested directly against the \
        oracle rather than re-derived from principle (indexA == 30's own bS == 3 tC0 entry, \
        3 -> 2 -- vaco_codec_dsp_deblock::tables's own doc has the full account, including one \
        plausible-looking alternate guess elsewhere that made things worse and was reverted): \
        99.78% match, 18 of 25 frames now fully byte-exact (up from 2). The remaining ~0.2% \
        (7 frames, small mismatches) was not chased further under this round's own explicit \
        time-box -- raising this floor honestly rather than continuing to guess against the \
        oracle past the point of quick returns. Does not retire \
        assert_slice_ends_at_rbsp_trailing_bits -- that assertion's own remaining relevance is a \
        distinct question; nothing here argues for weakening it, and it was not touched."]
    fn cabac_i_only_reconstructs_without_error_and_mostly_matches_ffmpeg() {
        let data: &[u8] = include_bytes!("../tests/fixtures/cabac_i_only.264");
        let reference: &[u8] = include_bytes!("../tests/fixtures/cabac_i_only_ref.yuv");
        let frames = decode_all_frames_luma_tolerant(data, true);
        assert_eq!(frames.len(), 25);
        let frame_stride = 64 * 64 + 2 * 32 * 32;
        let mut total = 0usize;
        let mut total_mismatch = 0usize;
        let mut failed_frames = 0usize;
        for (idx, frame) in frames.iter().enumerate() {
            let (mbs_wide, _mbs_high, luma) = match frame {
                Ok(f) => f,
                Err(e) => {
                    failed_frames += 1;
                    eprintln!("cabac_i_only frame {idx}: decode/reconstruct failed: {e}");
                    continue;
                }
            };
            let ref_frame = &reference[idx * frame_stride..idx * frame_stride + luma.len()];
            let width = (*mbs_wide * 16) as usize;
            let mut frame_mismatch = 0usize;
            let mut first = None;
            for (i, (&a, &b)) in luma.iter().zip(ref_frame.iter()).enumerate() {
                total += 1;
                if a != b {
                    total_mismatch += 1;
                    frame_mismatch += 1;
                    if first.is_none() {
                        let (x, y) = (i % width, i / width);
                        first = Some((x, y, x / 16, y / 16, a, b));
                    }
                }
            }
            eprintln!(
                "cabac_i_only frame {idx}: {frame_mismatch} / {} luma samples differ; first mismatch (x, y, mb_x, mb_y, ours, ffmpeg) = {:?}",
                luma.len(),
                first
            );
        }
        eprintln!(
            "cabac_i_only: {failed_frames} / {} frames failed to decode/reconstruct at all",
            frames.len()
        );
        let match_fraction = if total == 0 {
            0.0
        } else {
            1.0 - (total_mismatch as f64 / total as f64)
        };
        eprintln!(
            "cabac_i_only overall (successfully-decoded frames only): {total_mismatch} / {total} luma samples differ ({:.2}% match)",
            match_fraction * 100.0
        );
        assert_eq!(
            failed_frames,
            0,
            "cabac_i_only: {failed_frames} of {} frames failed to decode/reconstruct at all -- see stderr above",
            frames.len()
        );
        assert!(
            match_fraction >= 0.995,
            "cabac_i_only: only {:.2}% of luma samples match ffmpeg's real (deblocked) decode -- \
             ~99.78% (measured this round, after an oracle-verified tC0 table correction; 18 of \
             25 frames are fully byte-exact) is the expected floor here; a drop below it means a \
             real regression, not just the narrow, already-time-boxed remaining gap this test's \
             own ignore reason describes",
            match_fraction * 100.0
        );
    }

    /// Decodes a full I/P stream, maintaining a simple single-list DPB
    /// (P slices only; every picture is one slice; no MMCO/long-term
    /// refs -- `RefPicList0` is just every previously-decoded picture,
    /// most recent first, clause 8.2.4.2.1's own simplest case) across
    /// pictures, exercising `crate::motion`/`crate::interp`/
    /// `reconstruct_picture_with_inter` end to end for the first time.
    pub(super) fn decode_ip_stream_luma(data: &[u8]) -> Vec<Result<Vec<u8>, String>> {
        use vaco_bitstream::{BitReader, annexb};
        use vaco_codec_cabac::CabacDecoder;
        use vaco_format_nalu::RbspBuf;
        use vaco_limits::{Budget, Limits};
        use vaco_parse_h264::{H264NalHeader, NalUnitType, ParameterSets, SliceHeader, SliceKind};

        let mut params = ParameterSets::new();
        let mut budget = Budget::new(Limits::default());
        let mut rbsp = RbspBuf::new();
        let mut dpb: Vec<Vec<u8>> = Vec::new();
        let mut frames = Vec::new();

        for nal in annexb::nal_units(data) {
            let Some(header) = H264NalHeader::parse(nal) else {
                continue;
            };
            match header.nal_unit_type {
                NalUnitType::Sps => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let _ = params.add_sps(rbsp.as_slice(), &mut budget);
                }
                NalUnitType::Pps => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let _ = params.add_pps(rbsp.as_slice(), &mut budget);
                }
                NalUnitType::IdrSlice | NalUnitType::NonIdrSlice => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let payload = rbsp.as_slice();
                    let mut reader = BitReader::new(payload);
                    reader.skip(8);
                    let pps_id = {
                        let mut r2 = BitReader::new(payload);
                        r2.skip(8);
                        let mut g = vaco_codec_golomb::BoundedGolomb::new(&mut r2, &mut budget);
                        let _ = g.ue_v(u32::MAX).unwrap();
                        let _ = g.ue_v(9).unwrap();
                        g.ue_v(255).unwrap() as u8
                    };
                    let (pps, sps) = params.sps_for_pps(pps_id).unwrap();
                    let slice_header =
                        match SliceHeader::parse_data(&mut reader, header, sps, pps, &mut budget) {
                            Ok(h) => h,
                            Err(e) => {
                                frames.push(Err(format!("slice header: {e:?}")));
                                continue;
                            }
                        };
                    let mbs_wide = sps.pic_width_in_mbs;
                    let mbs_high =
                        sps.pic_height_in_map_units * if sps.frame_mbs_only { 1 } else { 2 };
                    let mut cabac = CabacDecoder::from_reader(reader);
                    let result = crate::mb::decode_slice_cabac(
                        &mut cabac,
                        &mut budget,
                        sps,
                        pps,
                        &slice_header,
                        None,
                    )
                    .map_err(|e| format!("decode_slice_cabac failed: {e:?}"))
                    .and_then(|stats| {
                        if cabac.malformed() {
                            return Err("CABAC engine reported malformed input".to_owned());
                        }
                        let luma = if slice_header.kind == SliceKind::I {
                            reconstruct_picture_luma(&stats.macroblocks, mbs_wide, mbs_high, &mut budget)
                                .map_err(|e| format!("reconstruct_picture_luma failed: {e:?}"))?
                        } else {
                            let empty = RefPlane::Flat(&[]);
                            let ref_list0: Vec<RefPicturePlanes<'_>> = dpb
                                .iter()
                                .rev()
                                .map(|p| RefPicturePlanes {
                                    luma: RefPlane::Flat(p.as_slice()),
                                    cb: empty,
                                    cr: empty,
                                })
                                .collect();
                            reconstruct_picture_with_inter(
                                &stats.macroblocks,
                                mbs_wide,
                                mbs_high,
                                &ref_list0,
                                &mut budget,
                            )
                            .map_err(|e| format!("reconstruct_picture_with_inter failed: {e:?}"))?
                        };
                        Ok(luma)
                    });
                    match &result {
                        Ok(luma) => dpb.push(luma.clone()),
                        Err(_) => dpb.push(vec![128u8; (mbs_wide * 16 * mbs_high * 16) as usize]),
                    }
                    frames.push(result);
                }
                _ => {}
            }
        }
        frames
    }

    /// `cabac_ip_simple.264`: the first real exercise of
    /// `crate::motion`/`crate::interp`/`reconstruct_picture_with_inter`/
    /// this crate's own P-slice DPB.
    ///
    /// Byte-exact against `ffmpeg -skip_loop_filter all` (the reference
    /// this fixture's own `.yuv` was captured with -- this crate does not
    /// deblock in this path either, so the comparison is fair) across all
    /// 25 frames, after locating the mechanism behind a structured
    /// "row 0 exact, every row below it wrong" defect that survived
    /// several earlier, real fixes in the same area (skipped macroblocks
    /// never populating the mv grid; `P_8x8`'s `num_sub == 1` case
    /// zeroing three of its own four 4x4 grid positions). None of those
    /// touched the actual remaining cause: `decode_sub_mb_pred_cabac`
    /// treated *both* `num_sub == 2` sub-macroblock types (Table 7-14's
    /// `P_L0_8x4` and `P_L0_4x8`, code 1 and code 2) as the same
    /// left/right (`P_L0_4x8`) split, discarding which one the
    /// bitstream actually named -- so every `P_L0_8x4` (top/bottom)
    /// sub-partition read its neighbour and resolved its own `C` from the
    /// wrong quadrant corner, and split ownership of the quadrant's own
    /// four 4x4 grid positions by `x` instead of `y`. Wrong only below
    /// row 0 because row 0's own macroblocks in this fixture happen not
    /// to exercise a `P_L0_8x4` split; independent of interpolation
    /// because the wrong neighbour/`C` corrupts the *predicted* motion
    /// vector itself, before any sample is ever fetched.
    #[test]
    fn cabac_ip_simple_decodes_and_reports_its_own_match_against_ffmpeg() {
        let data: &[u8] = include_bytes!("../tests/fixtures/cabac_ip_simple.264");
        let reference: &[u8] = include_bytes!("../tests/fixtures/cabac_ip_simple_ref.yuv");
        let frames = decode_ip_stream_luma(data);
        let frame_stride = 64 * 64 + 2 * 32 * 32;
        let mut total = 0usize;
        let mut mismatch = 0usize;
        let mut failed = 0usize;
        for (idx, frame) in frames.iter().enumerate() {
            match frame {
                Ok(luma) => {
                    let ref_frame = &reference[idx * frame_stride..idx * frame_stride + luma.len()];
                    let mut frame_mismatch = 0usize;
                    for (&a, &b) in luma.iter().zip(ref_frame.iter()) {
                        total += 1;
                        if a != b {
                            mismatch += 1;
                            frame_mismatch += 1;
                        }
                    }
                    eprintln!("  frame {idx}: {frame_mismatch} / {} differ", luma.len());
                }
                Err(e) => {
                    failed += 1;
                    eprintln!("cabac_ip_simple frame {idx}: {e}");
                }
            }
        }
        let pct = if total == 0 {
            0.0
        } else {
            100.0 * (1.0 - mismatch as f64 / total as f64)
        };
        eprintln!(
            "cabac_ip_simple: {failed} / {} frames failed outright; {mismatch} / {total} luma samples differ ({pct:.2}% match)",
            frames.len()
        );
        assert_eq!(failed, 0, "every frame must decode without a hard error");
        assert_eq!(mismatch, 0, "byte-exact against ffmpeg -skip_loop_filter all");
    }

    /// [`decode_ip_stream_luma`]'s own sibling, driving [`reconstruct_picture`]
    /// (the chroma-including entry point [`crate::decoder::H264Decoder`]
    /// itself calls) instead of the luma-only pair -- exists so this
    /// crate's own chroma composition (`predict_intra_chroma`,
    /// `predict_chroma_inter`, the chroma residual add) gets the same
    /// real-corpus, per-plane measurement discipline the luma path
    /// already has, rather than shipping on the strength of unit tests
    /// alone -- measuring one plane is not measuring the output.
    pub(super) fn decode_ip_stream_yuv(data: &[u8]) -> Vec<Result<ReconstructedPicture, String>> {
        use vaco_bitstream::{BitReader, annexb};
        use vaco_codec_cabac::CabacDecoder;
        use vaco_format_nalu::RbspBuf;
        use vaco_limits::{Budget, Limits};
        use vaco_parse_h264::{H264NalHeader, NalUnitType, ParameterSets, SliceHeader, SliceKind};

        let mut params = ParameterSets::new();
        let mut budget = Budget::new(Limits::default());
        let mut rbsp = RbspBuf::new();
        let mut dpb: Vec<ReconstructedPicture> = Vec::new();
        let mut frames = Vec::new();

        for nal in annexb::nal_units(data) {
            let Some(header) = H264NalHeader::parse(nal) else {
                continue;
            };
            match header.nal_unit_type {
                NalUnitType::Sps => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let _ = params.add_sps(rbsp.as_slice(), &mut budget);
                }
                NalUnitType::Pps => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let _ = params.add_pps(rbsp.as_slice(), &mut budget);
                }
                NalUnitType::IdrSlice | NalUnitType::NonIdrSlice => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let payload = rbsp.as_slice();
                    let mut reader = BitReader::new(payload);
                    reader.skip(8);
                    let pps_id = {
                        let mut r2 = BitReader::new(payload);
                        r2.skip(8);
                        let mut g = vaco_codec_golomb::BoundedGolomb::new(&mut r2, &mut budget);
                        let _ = g.ue_v(u32::MAX).unwrap();
                        let _ = g.ue_v(9).unwrap();
                        g.ue_v(255).unwrap() as u8
                    };
                    let (pps, sps) = params.sps_for_pps(pps_id).unwrap();
                    let slice_header =
                        match SliceHeader::parse_data(&mut reader, header, sps, pps, &mut budget) {
                            Ok(h) => h,
                            Err(e) => {
                                frames.push(Err(format!("slice header: {e:?}")));
                                continue;
                            }
                        };
                    let mbs_wide = sps.pic_width_in_mbs;
                    let mbs_high =
                        sps.pic_height_in_map_units * if sps.frame_mbs_only { 1 } else { 2 };
                    let mut cabac = CabacDecoder::from_reader(reader);
                    let result = crate::mb::decode_slice_cabac(
                        &mut cabac,
                        &mut budget,
                        sps,
                        pps,
                        &slice_header,
                        None,
                    )
                    .map_err(|e| format!("decode_slice_cabac failed: {e:?}"))
                    .and_then(|stats| {
                        if cabac.malformed() {
                            return Err("CABAC engine reported malformed input".to_owned());
                        }
                        let ref_list0: Vec<RefPicturePlanes<'_>> = if slice_header.kind == SliceKind::I {
                            Vec::new()
                        } else {
                            dpb.iter()
                                .rev()
                                .map(|p| RefPicturePlanes {
                                    luma: RefPlane::Flat(&p.luma),
                                    cb: RefPlane::Flat(&p.cb),
                                    cr: RefPlane::Flat(&p.cr),
                                })
                                .collect()
                        };
                        reconstruct_picture(
                            &stats.macroblocks,
                            mbs_wide,
                            mbs_high,
                            pps.chroma_qp_index_offset,
                            pps.second_chroma_qp_index_offset,
                            &ref_list0,
                            &[],
                            &SliceWeightTables::from_table(slice_header.pred_weight_table.as_ref()),
                            BiPredMode::Default,
                            None,
                            &mut budget,
                        )
                        .map_err(|e| format!("reconstruct_picture failed: {e:?}"))
                    });
                    match &result {
                        Ok(pic) => dpb.push(pic.clone()),
                        Err(_) => dpb.push(ReconstructedPicture {
                            luma: vec![128u8; (mbs_wide * 16 * mbs_high * 16) as usize],
                            cb: vec![128u8; (mbs_wide * 8 * mbs_high * 8) as usize],
                            cr: vec![128u8; (mbs_wide * 8 * mbs_high * 8) as usize],
                        }),
                    }
                    frames.push(result);
                }
                _ => {}
            }
        }
        frames
    }

    /// The part of [`cabac_ip_simple_full_deblocking_matches_ffmpegs_real_decode`]
    /// that is fully fixed and stays a real (non-`#[ignore]`d) regression
    /// check: frame 0, an I slice, whose macroblocks are by construction
    /// all intra, so it never exercises the still-open P-slice residual
    /// that test's own doc describes -- byte-exact, all three planes,
    /// against `ffmpeg`'s real (deblocked) decode, proving the chroma
    /// deblocking gap this dispatch found and closed actually closed it.
    #[test]
    fn cabac_ip_simple_frame_zero_full_deblocking_matches_ffmpeg() {
        use vaco_bitstream::{BitReader, annexb};
        use vaco_codec_cabac::CabacDecoder;
        use vaco_format_nalu::RbspBuf;
        use vaco_limits::{Budget, Limits};
        use vaco_parse_h264::{H264NalHeader, NalUnitType, ParameterSets, SliceHeader};

        let data: &[u8] = include_bytes!("../tests/fixtures/cabac_ip_simple.264");
        let reference: &[u8] = include_bytes!("../tests/fixtures/cabac_ip_simple_deblocked_ref.yuv");
        let (luma_len, chroma_len) = (64 * 64, 32 * 32);

        let mut params = ParameterSets::new();
        let mut budget = Budget::new(Limits::default());
        let mut rbsp = RbspBuf::new();

        for nal in annexb::nal_units(data) {
            let Some(header) = H264NalHeader::parse(nal) else { continue };
            match header.nal_unit_type {
                NalUnitType::Sps => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let _ = params.add_sps(rbsp.as_slice(), &mut budget);
                }
                NalUnitType::Pps => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let _ = params.add_pps(rbsp.as_slice(), &mut budget);
                }
                NalUnitType::IdrSlice => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let payload = rbsp.as_slice();
                    let mut reader = BitReader::new(payload);
                    reader.skip(8);
                    let pps_id = {
                        let mut r2 = BitReader::new(payload);
                        r2.skip(8);
                        let mut g = vaco_codec_golomb::BoundedGolomb::new(&mut r2, &mut budget);
                        let _ = g.ue_v(u32::MAX).unwrap();
                        let _ = g.ue_v(9).unwrap();
                        g.ue_v(255).unwrap() as u8
                    };
                    let (pps, sps) = params.sps_for_pps(pps_id).unwrap();
                    let slice_header =
                        SliceHeader::parse_data(&mut reader, header, sps, pps, &mut budget).unwrap();
                    let mbs_wide = sps.pic_width_in_mbs;
                    let mbs_high = sps.pic_height_in_map_units * if sps.frame_mbs_only { 1 } else { 2 };
                    let mut cabac = CabacDecoder::from_reader(reader);
                    let stats =
                        crate::mb::decode_slice_cabac(&mut cabac, &mut budget, sps, pps, &slice_header, None).unwrap();
                    let mut pic =
                        reconstruct_picture(&stats.macroblocks, mbs_wide, mbs_high, pps.chroma_qp_index_offset, pps.second_chroma_qp_index_offset, &[], &[], &SliceWeightTables::default(), BiPredMode::Default, None, &mut budget)
                            .unwrap();
                    crate::deblock::deblock_picture_luma(
                        &mut pic.luma,
                        &stats.macroblocks,
                        mbs_wide,
                        mbs_high,
                        slice_header.disable_deblocking_filter_idc,
                        slice_header.slice_alpha_c0_offset_div2,
                        slice_header.slice_beta_offset_div2,
                        &[],
                        &[],
                    )
                    .unwrap();
                    for (chroma, offset) in
                        [(&mut pic.cb, pps.chroma_qp_index_offset), (&mut pic.cr, pps.second_chroma_qp_index_offset)]
                    {
                        crate::deblock::deblock_picture_chroma(
                            chroma,
                            &stats.macroblocks,
                            mbs_wide,
                            mbs_high,
                            offset,
                            slice_header.disable_deblocking_filter_idc,
                            slice_header.slice_alpha_c0_offset_div2,
                            slice_header.slice_beta_offset_div2,
                            &[],
                            &[],
                        );
                    }
                    assert_eq!(pic.luma, reference[..luma_len], "frame 0 luma");
                    assert_eq!(pic.cb, reference[luma_len..luma_len + chroma_len], "frame 0 Cb");
                    assert_eq!(
                        pic.cr,
                        reference[luma_len + chroma_len..luma_len + 2 * chroma_len],
                        "frame 0 Cr"
                    );
                    return;
                }
                _ => {}
            }
        }
        panic!("no IDR slice found in fixture");
    }

    /// The CLI's own real path -- [`decode_ip_stream_yuv`] plus
    /// [`crate::deblock::deblock_picture_luma`]/`deblock_picture_chroma`
    /// applied to *every* slice (I and P alike), compared against
    /// `ffmpeg`'s real default decode (deblocking on, unlike the
    /// `-skip_loop_filter all` reference the undeblocked tests above use).
    ///
    /// Found and fixed getting here: chroma deblocking did not exist at
    /// all (every fixture's chroma was measured only against an
    /// undeblocked reference, so the gap was invisible), and
    /// `deblock_picture_luma` refused any non-intra macroblock outright,
    /// so a P slice was never deblocked either. Clause 8.7.2.1's general
    /// `bS` derivation is now implemented for both, cross-checked bin by
    /// bin against a locally built, instrumented JM 19.1 reference decoder
    /// (`vcgit.hhi.fraunhofer.de/jvet/JM`, Tier A) rather than re-derived
    /// from the specification text alone a second time.
    ///
    /// That fix found a real, second bug while landing: the boundary
    /// strength helper indexed `MbSummary::residual.luma_ac` by the same
    /// raster position it uses for `MbSummary::mv_blocks`, but
    /// `luma_ac` is `luma4x4BlkIdx`-ordered (clause 6.4.3's z-scan) while
    /// `mv_blocks` is genuinely raster-ordered -- two different
    /// conventions on two fields of the same struct. Fixing the
    /// conversion (`raster_to_luma4x4_blk_idx`) took frame 0 (I slice) to
    /// a byte-exact match on all three planes and collapsed the P-frame
    /// drift by roughly two orders of magnitude (max sample error 5-15,
    /// growing every frame, down to 1-2 for the first several frames).
    ///
    /// **Closed, and the defect was in this harness, not in the filter.**
    /// The residual this doc spent a round chasing -- "up to 8 by frame
    /// 24, concentrated at specific macroblock-boundary edges whose two
    /// sides have different partitions/motion" -- came from passing `&[]`
    /// as `deblock_picture_luma`/`_chroma`'s `ref_list0_poc`. Clause
    /// 8.7.2.1 compares reference *pictures*, and with an empty list
    /// `crate::deblock`'s own `ref_poc` answers `None` for every
    /// `ref_idx`, so "the two sides use a different set of reference
    /// pictures" is unsatisfiable and every such edge comes out `bS = 0`
    /// where the answer is 1. The real decoder (`decoder.rs`) has always
    /// passed real POCs, and decodes this same fixture byte-exact through
    /// the registered `H264Decoder` -- which is why the hypothesis this
    /// doc used to record (a rounding or activity-condition difference in
    /// `filter_luma_line`/`EdgeThresholds::samples_pass`, or a second
    /// wrong `TC0_TABLE` entry) never found anything: there was nothing
    /// wrong there to find. The harness now builds one distinct identity
    /// per `RefPicList0` position and this assertion passes byte-exact on
    /// all 25 frames, all three planes.
    ///
    /// Another instance of a recurring shape: **a harness that measures
    /// the wrong thing outlives every hypothesis you form about the code
    /// it is measuring.** Worth checking a failing internal-API test
    /// against the same fixture through the public decoder before
    /// believing the failure.
    #[test]
    fn cabac_ip_simple_full_deblocking_matches_ffmpegs_real_decode() {
        use vaco_bitstream::{BitReader, annexb};
        use vaco_codec_cabac::CabacDecoder;
        use vaco_format_nalu::RbspBuf;
        use vaco_limits::{Budget, Limits};
        use vaco_parse_h264::{H264NalHeader, NalUnitType, ParameterSets, SliceHeader, SliceKind};

        let data: &[u8] = include_bytes!("../tests/fixtures/cabac_ip_simple.264");
        let reference: &[u8] = include_bytes!("../tests/fixtures/cabac_ip_simple_deblocked_ref.yuv");
        let (luma_len, chroma_len) = (64 * 64, 32 * 32);
        let frame_stride = luma_len + 2 * chroma_len;

        let mut params = ParameterSets::new();
        let mut budget = Budget::new(Limits::default());
        let mut rbsp = RbspBuf::new();
        let mut dpb: Vec<ReconstructedPicture> = Vec::new();
        let mut mismatch = [0usize; 3];
        let mut total = [0usize; 3];
        let mut frame_idx = 0usize;

        for nal in annexb::nal_units(data) {
            let Some(header) = H264NalHeader::parse(nal) else { continue };
            match header.nal_unit_type {
                NalUnitType::Sps => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let _ = params.add_sps(rbsp.as_slice(), &mut budget);
                }
                NalUnitType::Pps => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let _ = params.add_pps(rbsp.as_slice(), &mut budget);
                }
                NalUnitType::IdrSlice | NalUnitType::NonIdrSlice => {
                    rbsp.fill(nal, &mut budget).unwrap();
                    let payload = rbsp.as_slice();
                    let mut reader = BitReader::new(payload);
                    reader.skip(8);
                    let pps_id = {
                        let mut r2 = BitReader::new(payload);
                        r2.skip(8);
                        let mut g = vaco_codec_golomb::BoundedGolomb::new(&mut r2, &mut budget);
                        let _ = g.ue_v(u32::MAX).unwrap();
                        let _ = g.ue_v(9).unwrap();
                        g.ue_v(255).unwrap() as u8
                    };
                    let (pps, sps) = params.sps_for_pps(pps_id).unwrap();
                    let slice_header =
                        SliceHeader::parse_data(&mut reader, header, sps, pps, &mut budget).unwrap();
                    let mbs_wide = sps.pic_width_in_mbs;
                    let mbs_high = sps.pic_height_in_map_units * if sps.frame_mbs_only { 1 } else { 2 };
                    let mut cabac = CabacDecoder::from_reader(reader);
                    let stats =
                        crate::mb::decode_slice_cabac(&mut cabac, &mut budget, sps, pps, &slice_header, None).unwrap();
                    assert!(!cabac.malformed(), "frame {frame_idx}: CABAC engine reported malformed input");
                    let ref_list0: Vec<RefPicturePlanes<'_>> = if slice_header.kind == SliceKind::I {
                        Vec::new()
                    } else {
                        dpb.iter()
                            .rev()
                            .map(|p| RefPicturePlanes {
                                luma: RefPlane::Flat(&p.luma),
                                cb: RefPlane::Flat(&p.cb),
                                cr: RefPlane::Flat(&p.cr),
                            })
                            .collect()
                    };
                    // Clause 8.7.2.1 compares reference *pictures*, so
                    // `deblock_picture_luma`/`_chroma` need one distinct
                    // identity per `RefPicList0` position -- the real
                    // decoder passes POCs (`decoder.rs`). Passing `&[]`
                    // here, as this harness used to, makes
                    // `crate::deblock`'s own `ref_poc` answer `None` for
                    // *every* `ref_idx`, so "the two sides use different
                    // reference pictures" can never be true and every such
                    // edge comes out `bS = 0` where the answer is 1. The
                    // values only have to be distinct and stable per list
                    // position, never real POCs.
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_possible_wrap,
                        reason = "a test harness's own list index, bounded by this fixture's 25 pictures"
                    )]
                    let ref_list0_poc: Vec<i32> = (0..ref_list0.len()).map(|i| -(i as i32) - 1).collect();
                    let mut pic = reconstruct_picture(
                        &stats.macroblocks,
                        mbs_wide,
                        mbs_high,
                        pps.chroma_qp_index_offset,
                        pps.second_chroma_qp_index_offset,
                        &ref_list0,
                        &[],
                        &SliceWeightTables::from_table(slice_header.pred_weight_table.as_ref()),
                        BiPredMode::Default,
                        None,
                        &mut budget,
                    )
                    .unwrap();
                    drop(ref_list0);
                    crate::deblock::deblock_picture_luma(
                        &mut pic.luma,
                        &stats.macroblocks,
                        mbs_wide,
                        mbs_high,
                        slice_header.disable_deblocking_filter_idc,
                        slice_header.slice_alpha_c0_offset_div2,
                        slice_header.slice_beta_offset_div2,
                        &ref_list0_poc,
                        &[],
                    )
                    .unwrap();
                    for (chroma, offset) in
                        [(&mut pic.cb, pps.chroma_qp_index_offset), (&mut pic.cr, pps.second_chroma_qp_index_offset)]
                    {
                        crate::deblock::deblock_picture_chroma(
                            chroma,
                            &stats.macroblocks,
                            mbs_wide,
                            mbs_high,
                            offset,
                            slice_header.disable_deblocking_filter_idc,
                            slice_header.slice_alpha_c0_offset_div2,
                            slice_header.slice_beta_offset_div2,
                            &ref_list0_poc,
                            &[],
                        );
                    }
                    dpb.push(pic.clone());

                    let base = frame_idx * frame_stride;
                    let planes: [(&[u8], &[u8]); 3] = [
                        (&pic.luma, &reference[base..base + luma_len]),
                        (&pic.cb, &reference[base + luma_len..base + luma_len + chroma_len]),
                        (&pic.cr, &reference[base + luma_len + chroma_len..base + frame_stride]),
                    ];
                    for (plane_idx, (got, want)) in planes.iter().enumerate() {
                        for (&a, &b) in got.iter().zip(want.iter()) {
                            total[plane_idx] += 1;
                            if a != b {
                                mismatch[plane_idx] += 1;
                            }
                        }
                    }
                    frame_idx += 1;
                }
                _ => {}
            }
        }
        eprintln!(
            "cabac_ip_simple (full deblocking, {frame_idx} frames): Y {}/{} U {}/{} V {}/{} differ",
            mismatch[0], total[0], mismatch[1], total[1], mismatch[2], total[2]
        );
        assert_eq!(mismatch, [0, 0, 0], "byte-exact against ffmpeg's real (deblocked) decode");
    }

    /// [`cabac_ip_simple_decodes_and_reports_its_own_match_against_ffmpeg`]'s
    /// own chroma measurement -- Y, U and V compared *separately* against
    /// the same reference `.yuv` (already full 4:2:0 per
    /// `frame_stride`'s own `2 * 32 * 32` chroma term, unused for chroma
    /// until now): two chroma defects have hidden behind correct luma
    /// before.
    #[test]
    fn cabac_ip_simple_chroma_matches_ffmpeg_per_plane() {
        let data: &[u8] = include_bytes!("../tests/fixtures/cabac_ip_simple.264");
        let reference: &[u8] = include_bytes!("../tests/fixtures/cabac_ip_simple_ref.yuv");
        let frames = decode_ip_stream_yuv(data);
        let (luma_len, chroma_len) = (64 * 64, 32 * 32);
        let frame_stride = luma_len + 2 * chroma_len;
        let mut failed = 0usize;
        let mut mismatch = [0usize; 3];
        let mut total = [0usize; 3];
        for (idx, frame) in frames.iter().enumerate() {
            let Ok(pic) = frame else {
                failed += 1;
                eprintln!("cabac_ip_simple frame {idx}: {}", frame.as_ref().unwrap_err());
                continue;
            };
            let base = idx * frame_stride;
            let planes: [(&[u8], &[u8]); 3] = [
                (&pic.luma, &reference[base..base + luma_len]),
                (&pic.cb, &reference[base + luma_len..base + luma_len + chroma_len]),
                (
                    &pic.cr,
                    &reference[base + luma_len + chroma_len..base + luma_len + 2 * chroma_len],
                ),
            ];
            for (p, (ours, refp)) in planes.iter().enumerate() {
                let mut frame_mismatch = 0usize;
                for (&a, &b) in ours.iter().zip(refp.iter()) {
                    total[p] += 1;
                    if a != b {
                        mismatch[p] += 1;
                        frame_mismatch += 1;
                    }
                }
                if frame_mismatch > 0 {
                    eprintln!(
                        "  frame {idx} plane {}: {frame_mismatch} / {} differ",
                        ["Y", "Cb", "Cr"][p],
                        ours.len()
                    );
                }
            }
        }
        for (p, name) in ["Y", "Cb", "Cr"].iter().enumerate() {
            let pct = if total[p] == 0 {
                0.0
            } else {
                100.0 * (1.0 - mismatch[p] as f64 / total[p] as f64)
            };
            eprintln!(
                "cabac_ip_simple plane {name}: {} / {} samples differ ({pct:.2}% match)",
                mismatch[p], total[p]
            );
        }
        assert_eq!(failed, 0, "every frame must decode without a hard error");
        assert_eq!(mismatch, [0, 0, 0], "byte-exact against ffmpeg -skip_loop_filter all, every plane");
    }

    /// [`sample_luma_partition`] against [`sample_luma_block`], the
    /// wrapper-level oracle. `crate::interp`'s own
    /// `partition_matches_the_per_pixel_oracle_at_every_fractional_position_and_shape`
    /// already checks the pure interpolation math; this checks the layer
    /// above it that `crate::interp`'s test cannot reach: [`RefPlane`]
    /// dispatch, the `safe`/`clamped` fetch choice, and their own bounds
    /// arithmetic, generalised from a fixed 4x4 block to an arbitrary
    /// partition size.
    #[test]
    fn sample_luma_partition_matches_sample_luma_block_for_every_shape_and_edge_case() {
        use vaco_limits::{Budget, Limits};
        let ref_width = 24u32;
        let ref_height = 24u32;
        let mut plane = vec![0u8; (ref_width * ref_height) as usize];
        for (i, v) in plane.iter_mut().enumerate() {
            *v = u8::try_from((i * 37 + (i / 5) * 11) % 256).unwrap_or(0);
        }
        let mut budget = Budget::new(Limits::default());
        let mut scratch = ReadScratch::new(&mut budget).unwrap();

        // `(x, y)` combined with a partition's own `w`/`h` and `mv` decide
        // whether the fast (fully in-bounds) or clamped fetch path runs
        // inside `sample_luma_block`/`sample_luma_partition` alike --
        // covering both, plus a motion vector large enough to reach past
        // every edge on at least one shape below.
        for &(x, y) in &[(4u32, 4u32), (0, 0), (18, 18)] {
            for &(w, h) in &[(4usize, 4usize), (8, 8), (16, 16), (8, 16), (16, 8)] {
                for &mv in &[(0i16, 0i16), (3, 1), (-5, 6), (9, -9)] {
                    // Oracle: one `sample_luma_block` call per 4x4 sub-block
                    // of the partition, gathered into the same `[[u8;16];16]`
                    // shape `sample_luma_partition` returns.
                    let mut want = [[0u8; 16]; 16];
                    for sby in (0..h).step_by(4) {
                        for sbx in (0..w).step_by(4) {
                            let block = sample_luma_block(
                                RefPlane::Flat(&plane),
                                ref_width,
                                ref_height,
                                x + sbx as u32,
                                y + sby as u32,
                                mv,
                                &mut scratch,
                            );
                            for i in 0..4 {
                                for j in 0..4 {
                                    want[sby + i][sbx + j] = block[i][j];
                                }
                            }
                        }
                    }
                    let got = sample_luma_partition(RefPlane::Flat(&plane), ref_width, ref_height, x, y, w, h, mv, &mut scratch);
                    for oy in 0..h {
                        for ox in 0..w {
                            assert_eq!(
                                got[oy][ox], want[oy][ox],
                                "x={x} y={y} w={w} h={h} mv={mv:?} ox={ox} oy={oy}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// [`partition_rects`]'s own decomposition, checked directly against
    /// hand-built motion grids for the key shapes: a whole-macroblock
    /// 16x16 partition, a 16x8 top/bottom
    /// split, and a `P_8x8` macroblock whose four quadrants carry four
    /// different motion vectors (so no merge across quadrant boundaries is
    /// possible, exercising the "sixteen separate rectangles" end of the
    /// range as well as the "one" end).
    #[test]
    fn partition_rects_recovers_known_shapes() {
        let mv_at = |mvx: i16, mvy: i16| MvInfo::for_test_l0((mvx, mvy));

        // One 16x16 partition: every 4x4 cell shares one motion vector.
        let uniform = [mv_at(4, -2); 16];
        let (rects, n) = partition_rects(&uniform);
        assert_eq!(n, 1, "a uniform grid must merge into exactly one rectangle");
        assert_eq!((rects[0].bx, rects[0].by, rects[0].bw, rects[0].bh), (0, 0, 4, 4));

        // 16x8: top two 4x4 rows share one vector, bottom two share another.
        let mut split = [MvInfo::default(); 16];
        for by in 0..4u32 {
            for bx in 0..4u32 {
                split[(by * 4 + bx) as usize] = if by < 2 { mv_at(1, 1) } else { mv_at(-3, 2) };
            }
        }
        let (rects, n) = partition_rects(&split);
        assert_eq!(n, 2, "top/bottom 16x8 split must recover exactly two rectangles");
        let mut shapes: Vec<(u8, u8, u8, u8)> = rects[..n].iter().map(|r| (r.bx, r.by, r.bw, r.bh)).collect();
        shapes.sort_unstable();
        assert_eq!(shapes, vec![(0, 0, 4, 2), (0, 2, 4, 2)]);

        // Four P_8x8 quadrants, four different vectors: no merge possible.
        let mut quads = [MvInfo::default(); 16];
        for by in 0..4u32 {
            for bx in 0..4u32 {
                let q = (by / 2) * 2 + (bx / 2);
                quads[(by * 4 + bx) as usize] = mv_at(i16::try_from(q).unwrap(), 0);
            }
        }
        let (rects, n) = partition_rects(&quads);
        assert_eq!(n, 4, "four differently-moving quadrants must not merge");
        let mut shapes: Vec<(u8, u8, u8, u8)> = rects[..n].iter().map(|r| (r.bx, r.by, r.bw, r.bh)).collect();
        shapes.sort_unstable();
        assert_eq!(shapes, vec![(0, 0, 2, 2), (0, 2, 2, 2), (2, 0, 2, 2), (2, 2, 2, 2)]);
    }
}
