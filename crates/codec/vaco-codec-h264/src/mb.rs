//! Macroblock-layer syntax, CAVLC and CABAC, clause 7.3.5 / 7.4.5 — scoped
//! to reaching a real bit-exact-consumption measurement (#419's stated
//! goal for this dispatch), not reconstruction. Nothing here computes a
//! pixel; every syntax element that would feed prediction or the transform
//! is still fully *read* (so bit consumption is correct) but its value is
//! only kept when a later element's presence depends on it.
//!
//! # What is in scope, and what is explicitly not
//!
//! **In scope**: I/P/B slices, `mb_skip_run` (CAVLC) and `mb_skip_flag`
//! (CABAC), all of Table 7-8/7-10/7-11's macroblock types and Table
//! 7-14/7-15's sub-macroblock types, `ref_idx`/`mvd` presence and count per
//! partition, `coded_block_pattern` (both entropy modes), `mb_qp_delta`,
//! and the neighbour-derived `nC` (clause 9.2.1) CAVLC residual decode
//! needs. Multiple slices per picture (each slice gets its own fresh
//! neighbour grid — clause 6.4.8's "different slice" rule for
//! unavailability falls out of that for free, not a separate check).
//!
//! **Explicitly out of scope, not merely unimplemented**:
//!
//! - **The 8x8 luma transform** (`transform_size_8x8_flag`, `Intra_8x8`,
//!   High-profile-only): the primary source this crate's tables are
//!   verified against (`provenance/vaco-codec-h264.toml`'s
//!   `iso-iec-14496-10-2002-draft`) predates this entirely — its own
//!   `mb_pred()`/`macroblock_layer()` syntax tables have no
//!   `transform_size_8x8_flag` and no `Intra_8x8` case at all, the same
//!   "predates a later amendment" shape as 4:2:2 chroma DC and
//!   `level_prefix >= 16` found in the CAVLC table correction. The test
//!   corpus this module is verified against is encoded Main profile
//!   specifically to avoid emitting this (High profile turns it on by
//!   default), rather than leaving an unverified path in untested code.
//! - **MBAFF** (`mb_adaptive_frame_field`) and field pictures. Neighbour
//!   availability changes shape entirely under MBAFF (pairs of macroblocks,
//!   parity-dependent derivation) — [`decode_slice_cavlc`] refuses a slice
//!   whose SPS has `mb_adaptive_frame_field` set or whose header is a field
//!   picture, rather than silently getting frame-only neighbour derivation
//!   wrong for it.
//! - **`constrained_intra_pred_flag`'s neighbour substitution rule**
//!   (clause 9.2.1: an intra block does not read an inter neighbour's
//!   `TotalCoeff` at all when this flag is set). Not implemented; the test
//!   corpus is encoded with it off (x264's default), and
//!   [`decode_slice_cavlc`] refuses a slice whose PPS has it set.
//! - **4:2:2/4:4:4 chroma, `SI` slices, I_PCM, weighted prediction's actual
//!   weights** (the syntax elements they would need — `pred_weight_table`
//!   — are already fully parsed by `vaco-parse-h264`'s slice header, so
//!   nothing here re-reads them). `I_PCM` specifically refuses rather than
//!   guessing at its byte-alignment padding's exact bit count from this
//!   module alone.
//! - **CABAC's macroblock-layer context tables** — `mb_type`, `mb_skip_flag`,
//!   `coded_block_pattern`, `ref_idx`, `mvd`, intra pred mode flags,
//!   `mb_qp_delta`, `coded_block_flag`, `transform_size_8x8_flag` — are not
//!   implemented in this dispatch. Unlike the CAVLC syntax elements above
//!   (plain `ue(v)`/`se(v)`/`te(v)`/`me(v)` reads with no new
//!   hand-transcribed bit tables), CABAC's binarisation needs per-element
//!   context-initialisation tables this crate has not fetched and verified
//!   yet, and several `ctxIdxInc` derivations need exactly the neighbour
//!   state this module builds for CAVLC's `nC` — building it once and
//!   fabricating the context tables to reach a number would repeat the
//!   mistake this whole line of work has been about not repeating. See
//!   `docs/codec/vaco-codec-h264.md` for the precise state.

use vaco_bitstream::BitReader;
use vaco_codec_golomb::{BoundedGolomb, ChromaArrayType, MbPartPredMode as CbpPredMode};
use vaco_core::{Error, Result};
use vaco_limits::Budget;
use vaco_parse_h264::{ChromaFormat, Pps, Sps, SliceHeader, SliceKind};

use crate::cavlc::residual_block_cavlc;

/// One partition's prediction-list membership — all that bit consumption
/// needs to know (which of `ref_idx_l0`/`l1` and `mvd_l0`/`l1` are present),
/// not which exact named shape (`B_L0_L1_16x8` vs `B_L0_L1_8x16`) it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartPred {
    L0,
    L1,
    Bi,
}

impl PartPred {
    const fn reads_l0(self) -> bool {
        !matches!(self, Self::L1)
    }
    const fn reads_l1(self) -> bool {
        !matches!(self, Self::L0)
    }
}

/// A classified `mb_type`, collapsed onto exactly what bit consumption
/// needs — see the module doc for why the full 24-row `Intra_16x16` table
/// and every named B-partition shape don't need their own variants.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MbKind {
    Intra4x4,
    Intra16x16 { cbp_luma: u8, cbp_chroma: u8 },
    IPcm,
    /// `P_L0_16x16`, `P_L0_L0_16x8`/`8x16`, and every non-direct, non-8x8 B
    /// shape (`B_L0_16x16` .. `B_Bi_Bi_8x16`) — one or two whole-macroblock
    /// partitions, each with its own prediction-list membership.
    Inter { parts: Vec<PartPred> },
    /// `P_8x8`/`P_8x8ref0` — four sub-macroblock partitions, `sub_mb_type`
    /// read for each; `ref0_inferred` is `P_8x8ref0`'s "never read
    /// ref_idx_l0" rule.
    P8x8 { ref0_inferred: bool },
    /// `B_8x8` — four sub-macroblock partitions, `sub_mb_type` read for
    /// each (which may itself be `B_Direct_8x8`, reading nothing further).
    B8x8,
    /// `B_Direct_16x16` — no `ref_idx`/`mvd` at all.
    BDirect16x16,
}

impl MbKind {
    const fn is_intra(&self) -> bool {
        matches!(self, Self::Intra4x4 | Self::Intra16x16 { .. } | Self::IPcm)
    }
}

/// `mb_type` decode + classification, clause 7.4.5 / Table 7-8/7-10/7-11.
///
/// `code` is the raw `ue(v)` value already read from the bitstream; slice
/// -type offsetting (P/SP: subtract 5 for `mb_type >= 5`; B: subtract 23
/// for `mb_type >= 23`) happens here rather than at the call site, per
/// clause 7.4.5's own text.
#[allow(
    clippy::integer_division,
    reason = "Table 7-8's cbp_chroma = (idx/4)%3 and Table 7-11's pair_idx = (v-4)/2 \
              are the specification's own formulas, not a precision-loss bug"
)]
fn classify_mb_type(kind: SliceKind, code: u32) -> Result<MbKind> {
    let i_slice_mb_type = |v: u32| -> Result<MbKind> {
        match v {
            0 => Ok(MbKind::Intra4x4),
            1..=24 => {
                let idx = v - 1;
                let cbp_luma = if idx >= 12 { 15 } else { 0 };
                let cbp_chroma = ((idx / 4) % 3) as u8;
                Ok(MbKind::Intra16x16 { cbp_luma, cbp_chroma })
            }
            25 => Ok(MbKind::IPcm),
            _ => Err(Error::InvalidData("mb_type: out of range for Table 7-8")),
        }
    };
    match kind {
        SliceKind::I => i_slice_mb_type(code),
        SliceKind::P | SliceKind::Sp => match code {
            0 => Ok(MbKind::Inter { parts: vec![PartPred::L0] }),
            1 | 2 => Ok(MbKind::Inter { parts: vec![PartPred::L0, PartPred::L0] }),
            3 => Ok(MbKind::P8x8 { ref0_inferred: false }),
            4 => Ok(MbKind::P8x8 { ref0_inferred: true }),
            v if v >= 5 => i_slice_mb_type(v - 5),
            _ => unreachable!(),
        },
        SliceKind::B => match code {
            0 => Ok(MbKind::BDirect16x16),
            1 => Ok(MbKind::Inter { parts: vec![PartPred::L0] }),
            2 => Ok(MbKind::Inter { parts: vec![PartPred::L1] }),
            3 => Ok(MbKind::Inter { parts: vec![PartPred::Bi] }),
            v @ 4..=21 => {
                // Table 7-11, rows 4..21: two-partition shapes, two rows
                // (16x8, 8x16) per (pred0, pred1) pair — the partition
                // *shape* does not affect bit consumption, only which of
                // the two prediction lists each of the two partitions
                // reads. The nine pairs are in this exact row order in the
                // primary text (verified against
                // `provenance/vaco-codec-h264.toml`'s
                // `iso-iec-14496-10-2002-draft`, lines 4593-4627): it is
                // *not* a lexicographic 3x3 grid over {L0, L1, Bi} — an
                // earlier draft of this function assumed it was and got
                // pair 1 (B_L1_L1, not B_L0_L1) wrong.
                use PartPred::{Bi, L0, L1};
                const PAIRS: [(PartPred, PartPred); 9] =
                    [(L0, L0), (L1, L1), (L0, L1), (L1, L0), (L0, Bi), (L1, Bi), (Bi, L0), (Bi, L1), (Bi, Bi)];
                let pair_idx = usize::try_from((v - 4) / 2).unwrap_or(0).min(8); // 0..=8
                let (p0, p1) = PAIRS.get(pair_idx).copied().unwrap_or((L0, L0));
                Ok(MbKind::Inter { parts: vec![p0, p1] })
            }
            22 => Ok(MbKind::B8x8),
            v if v >= 23 => i_slice_mb_type(v - 23),
            _ => Err(Error::InvalidData("mb_type: out of range for Table 7-11")),
        },
        SliceKind::Si => Err(Error::Unsupported("vaco-codec-h264: SI slices are out of scope")),
    }
}

/// `sub_mb_type` decode, clause 7.4.5.2 / Table 7-14 (P) or 7-15 (B).
/// Returns `(num_sub_parts, pred)` — `pred` is `None` only for
/// `B_Direct_8x8`, which reads no `ref_idx`/`mvd` for that sub-macroblock.
fn classify_sub_mb_type(is_b: bool, code: u32) -> Result<(u8, Option<PartPred>)> {
    if is_b {
        match code {
            0 => Ok((4, None)), // B_Direct_8x8: NumSubMbPart is "na"; treated
            // as 4 only so a caller's generic "how many 4x4-equivalent
            // blocks" question has an answer — no mvd/ref_idx is ever read
            // for it regardless, per the `pred == None` case.
            1 => Ok((1, Some(PartPred::L0))),
            2 => Ok((1, Some(PartPred::L1))),
            3 => Ok((1, Some(PartPred::Bi))),
            4 | 5 => Ok((2, Some(PartPred::L0))),
            6 | 7 => Ok((2, Some(PartPred::L1))),
            8 | 9 => Ok((2, Some(PartPred::Bi))),
            10 => Ok((4, Some(PartPred::L0))),
            11 => Ok((4, Some(PartPred::L1))),
            12 => Ok((4, Some(PartPred::Bi))),
            _ => Err(Error::InvalidData("sub_mb_type: out of range for Table 7-15")),
        }
    } else {
        match code {
            0 => Ok((1, Some(PartPred::L0))),
            1 | 2 => Ok((2, Some(PartPred::L0))),
            3 => Ok((4, Some(PartPred::L0))),
            _ => Err(Error::InvalidData("sub_mb_type: out of range for Table 7-14")),
        }
    }
}

/// One 4x4 luma or chroma block's decoded `TotalCoeff`, for `nC`
/// derivation. `None` means "not yet written this slice" — clause 6.4.8's
/// "belongs to a different slice" unavailability rule falls out of giving
/// each slice its own fresh grid, rather than being checked explicitly.
#[derive(Clone, Copy, Default)]
struct NBlock(Option<u8>);

/// Per-slice neighbour state: one grid entry per 4x4 luma block and per
/// 4x4 chroma-AC block (two chroma components), in absolute frame
/// coordinates so a left/above lookup never needs a macroblock-boundary
/// special case — it is just `x - 1`/`y - 1` into the same grid.
struct NeighbourGrid {
    mbs_wide: u32,
    mbs_high: u32,
    luma: Vec<NBlock>,
    chroma: [Vec<NBlock>; 2],
}

/// The standard luma/chroma 4x4-block scan order within one macroblock
/// (clause 6.4.3): two bits each select the 8x8 quadrant and the 4x4
/// position within it, both in raster order — `blk_idx >> 2` is the
/// quadrant (0=top-left, 1=top-right, 2=bottom-left, 3=bottom-right),
/// `blk_idx & 3` is the raster position inside it.
const fn blk_xy(blk_idx: u32) -> (u32, u32) {
    let quadrant = blk_idx >> 2;
    let within = blk_idx & 3;
    let qx = quadrant & 1;
    let qy = quadrant >> 1;
    let wx = within & 1;
    let wy = within >> 1;
    (qx * 2 + wx, qy * 2 + wy)
}

impl NeighbourGrid {
    fn new(mbs_wide: u32, mbs_high: u32) -> Self {
        let luma_len = usize::try_from(mbs_wide * 4).unwrap_or(0) * usize::try_from(mbs_high * 4).unwrap_or(0);
        let chroma_len = usize::try_from(mbs_wide * 2).unwrap_or(0) * usize::try_from(mbs_high * 2).unwrap_or(0);
        Self {
            mbs_wide,
            mbs_high,
            luma: vec![NBlock::default(); luma_len],
            chroma: [vec![NBlock::default(); chroma_len], vec![NBlock::default(); chroma_len]],
        }
    }

    fn luma_idx(&self, x: u32, y: u32) -> Option<usize> {
        if x >= self.mbs_wide * 4 || y >= self.mbs_high * 4 {
            return None;
        }
        Some(usize::try_from(y * self.mbs_wide * 4 + x).unwrap_or(usize::MAX))
    }

    fn chroma_idx(&self, x: u32, y: u32) -> Option<usize> {
        if x >= self.mbs_wide * 2 || y >= self.mbs_high * 2 {
            return None;
        }
        Some(usize::try_from(y * self.mbs_wide * 2 + x).unwrap_or(usize::MAX))
    }

    /// `nC`, clause 9.2.1's general (non-chroma-DC) case: average of the
    /// left/above neighbours' `TotalCoeff`, whichever are available (the
    /// picture edge and "different slice" both read as unavailable here —
    /// see the module doc), 0 if neither is.
    fn nc_luma(&self, x: u32, y: u32) -> i32 {
        let left = x.checked_sub(1).and_then(|lx| self.luma_idx(lx, y)).and_then(|i| self.luma.get(i)).and_then(|b| b.0);
        let above = y.checked_sub(1).and_then(|ay| self.luma_idx(x, ay)).and_then(|i| self.luma.get(i)).and_then(|b| b.0);
        nc_from_neighbours(left, above)
    }

    fn nc_chroma(&self, comp: usize, x: u32, y: u32) -> i32 {
        let Some(g) = self.chroma.get(comp) else {
            return nc_from_neighbours(None, None);
        };
        let left = x.checked_sub(1).and_then(|lx| self.chroma_idx(lx, y)).and_then(|i| g.get(i)).and_then(|b| b.0);
        let above = y.checked_sub(1).and_then(|ay| self.chroma_idx(x, ay)).and_then(|i| g.get(i)).and_then(|b| b.0);
        nc_from_neighbours(left, above)
    }

    fn set_luma(&mut self, x: u32, y: u32, total_coeff: u8) {
        if let Some(i) = self.luma_idx(x, y) {
            if let Some(b) = self.luma.get_mut(i) {
                b.0 = Some(total_coeff);
            }
        }
    }

    fn set_chroma(&mut self, comp: usize, x: u32, y: u32, total_coeff: u8) {
        if let Some(i) = self.chroma_idx(x, y)
            && let Some(b) = self.chroma.get_mut(comp).and_then(|g| g.get_mut(i))
        {
            b.0 = Some(total_coeff);
        }
    }
}

/// `CurrMbAddr` to `(mb_x, mb_y)` in a frame-only (no MBAFF) raster-scan
/// picture: clause 6.4.1's `MbToSliceGroupMap`-free special case, since
/// [`check_scope`] already refuses MBAFF/field pictures before this is ever
/// called.
#[allow(
    clippy::integer_division,
    reason = "addr / mbs_wide is the raster-scan row number, not a precision-loss bug"
)]
const fn mb_addr_xy(addr: u32, mbs_wide: u32) -> (u32, u32) {
    (addr % mbs_wide, addr / mbs_wide)
}

const fn nc_from_neighbours(left: Option<u8>, above: Option<u8>) -> i32 {
    match (left, above) {
        (Some(a), Some(b)) => ((a as i32) + (b as i32) + 1) >> 1,
        (Some(a), None) => a as i32,
        (None, Some(b)) => b as i32,
        (None, None) => 0,
    }
}

/// Everything one call to [`decode_slice_cavlc`] measured.
#[derive(Debug, Default)]
pub struct SliceStats {
    pub macroblock_count: u32,
    pub skipped_count: u32,
}

/// Refusal reasons this module names explicitly rather than trying and
/// getting wrong — see the module doc's "explicitly not" list.
fn check_scope(sps: &Sps, pps: &Pps, header: &SliceHeader) -> Result<()> {
    if sps.mb_adaptive_frame_field || header.field_pic {
        return Err(Error::Unsupported("vaco-codec-h264: MBAFF/field pictures are out of scope for #419"));
    }
    if pps.constrained_intra_pred {
        return Err(Error::Unsupported(
            "vaco-codec-h264: constrained_intra_pred_flag's neighbour substitution is out of scope for #419",
        ));
    }
    if pps.transform_8x8_mode {
        return Err(Error::Unsupported(
            "vaco-codec-h264: transform_size_8x8_flag/Intra_8x8 (High profile) is out of scope for #419 \
             — this crate's tables are verified against a source that predates it",
        ));
    }
    if !matches!(sps.chroma_format, ChromaFormat::Yuv420) {
        return Err(Error::Unsupported("vaco-codec-h264: only 4:2:0 is in scope for #419"));
    }
    if matches!(header.kind, SliceKind::Si) {
        return Err(Error::Unsupported("vaco-codec-h264: SI slices are out of scope for #419"));
    }
    Ok(())
}

/// Drive one CAVLC slice's `slice_data()` (clause 7.3.4) from a reader
/// already positioned at its first bit, consuming exactly the bits the
/// macroblock loop declares — the bit-exact-consumption measurement #419
/// exists for. Returns [`SliceStats`] on success, an error naming exactly
/// what stopped it otherwise (including every explicit scope refusal in
/// the module doc).
///
/// # Errors
///
/// As the module doc's scope list, plus whatever the underlying syntax
/// element reads (`Error::InvalidData`/`Error::UnexpectedEof`/
/// `Error::LimitExceeded`) return.
pub fn decode_slice_cavlc(
    r: &mut BitReader<'_>,
    budget: &mut Budget,
    sps: &Sps,
    pps: &Pps,
    header: &SliceHeader,
) -> Result<SliceStats> {
    check_scope(sps, pps, header)?;
    let mbs_wide = sps.pic_width_in_mbs;
    let mbs_high = sps.pic_height_in_map_units * if sps.frame_mbs_only { 1 } else { 2 };
    let mut grid = NeighbourGrid::new(mbs_wide.max(1), mbs_high.max(1));
    let mut stats = SliceStats::default();

    let is_b = matches!(header.kind, SliceKind::B);
    let is_i_or_si = matches!(header.kind, SliceKind::I | SliceKind::Si);
    let total_mbs = mbs_wide.saturating_mul(mbs_high);
    let mut curr_mb_addr = header.first_mb_in_slice;

    loop {
        if curr_mb_addr >= total_mbs {
            break;
        }

        if !is_i_or_si {
            let skip_run = {
                let mut g = BoundedGolomb::new(r, budget);
                g.ue_v(total_mbs)?
            };
            stats.skipped_count += skip_run;
            stats.macroblock_count += skip_run;
            // Clause 9.2.1: a skipped macroblock's TotalCoeff is inferred to
            // be 0 for every block it owns, exactly like an explicit CBP of
            // 0 — the next real macroblock's nC derivation depends on this.
            for skipped in 0..skip_run {
                let addr = curr_mb_addr + skipped;
                let (sx, sy) = mb_addr_xy(addr, mbs_wide);
                zero_out_mb_neighbours(&mut grid, sx, sy);
            }
            curr_mb_addr += skip_run;
            if curr_mb_addr >= total_mbs {
                break;
            }
            // Clause 7.3.4's `slice_data()`: `moreDataFlag = more_rbsp_data()`
            // is checked immediately after a *nonzero* `mb_skip_run`, before
            // ever deciding to call `macroblock_layer()` for the macroblock
            // the skip run landed on. A multi-slice picture's non-final
            // slice can end with exactly this shape — a skip run that
            // consumes the rest of *this slice's own* macroblocks, with
            // nothing left in `slice_data()` but `rbsp_slice_trailing_bits()`
            // — and `CurrMbAddr` is still `< total_mbs` at that point only
            // because `total_mbs` is the whole *picture's* macroblock count,
            // not this slice's own last macroblock. Skipping this check
            // (an earlier version of this function did) reads straight into
            // the next slice's own NAL-worth of bits as if it were more of
            // this one, a bit-exact-consumption bug this module's own
            // real-corpus test caught at exactly this shape (a two-slice
            // picture's non-final slice ending mid skip-run).
            if skip_run > 0 && !more_rbsp_data(r) {
                break;
            }
        }
        let (mb_x, mb_y) = mb_addr_xy(curr_mb_addr, mbs_wide);

        decode_macroblock_cavlc(r, budget, pps, header, &mut grid, mb_x, mb_y, is_b)?;
        stats.macroblock_count += 1;
        curr_mb_addr += 1;

        if !more_rbsp_data(r) {
            break;
        }
    }
    Ok(stats)
}

/// `more_rbsp_data()`, clause 7.2 — whether anything other than
/// `rbsp_trailing_bits()` remains. Approximated the same way a byte-level
/// framer can: if what is left is entirely the trailing pattern (a single
/// `1` bit then zero-or-more `0` bits, byte-aligned at the end), there is
/// no more real data. `vaco-parse-h264` already trims RBSP emulation
/// prevention bytes before this reader ever sees the buffer, so this does
/// not need to.
fn more_rbsp_data(r: &BitReader<'_>) -> bool {
    let remaining = r.remaining_bytes();
    // Find the last set bit across the remaining bytes; if it is not the
    // very first bit remaining, there is more than just the trailing
    // pattern left.
    let Some(idx) = remaining.iter().rposition(|&b| b != 0) else {
        return false;
    };
    let Some((last_nonzero, rest)) = remaining.get(idx).zip(remaining.get(..idx)) else {
        return false;
    };
    let last_nonzero = *last_nonzero;
    if !rest.is_empty() {
        return true;
    }
    // Exactly one nonzero byte remains (the rest, if any, are already all
    // zero and thus part of the trailing pattern by construction): more
    // data remains unless that byte's only set bit is its lowest one
    // (`rbsp_stop_one_bit` written LSB-last in byte order is MSB-first in
    // bit order — i.e. the byte is a power of two written as `1000...0`,
    // which as a plain integer is not a single bit unless it is exactly
    // the byte's own top bit chain reduced to one `1`).
    last_nonzero.count_ones() > 1 || !is_trailing_pattern(last_nonzero)
}

const fn is_trailing_pattern(byte: u8) -> bool {
    // `1` bit followed by zero or more `0` bits, MSB-first: 0b1000_0000,
    // 0b0100_0000, ..., 0b0000_0001 are the only eight valid patterns for
    // a byte that is *entirely* rbsp_trailing_bits().
    byte.is_power_of_two()
}

#[allow(clippy::too_many_arguments)]
fn decode_macroblock_cavlc(
    r: &mut BitReader<'_>,
    budget: &mut Budget,
    _pps: &Pps,
    header: &SliceHeader,
    grid: &mut NeighbourGrid,
    mb_x: u32,
    mb_y: u32,
    is_b: bool,
) -> Result<()> {
    let kind = {
        let mut g = BoundedGolomb::new(r, budget);
        let code = g.ue_v(48)?;
        classify_mb_type(header.kind, code)?
    };

    if matches!(kind, MbKind::IPcm) {
        return Err(Error::Unsupported("vaco-codec-h264: I_PCM is out of scope for #419"));
    }

    if kind.is_intra() {
        decode_mb_pred_intra(r, budget, &kind, grid, mb_x, mb_y)?;
    } else {
        decode_mb_pred_inter(r, budget, header, &kind, is_b)?;
    }

    let (cbp_luma, cbp_chroma) = if let MbKind::Intra16x16 { cbp_luma, cbp_chroma } = &kind {
        (*cbp_luma, *cbp_chroma)
    } else {
        let mut g = BoundedGolomb::new(r, budget);
        let pred_mode = if kind.is_intra() { CbpPredMode::Intra } else { CbpPredMode::Inter };
        let cbp = g.me_v(ChromaArrayType::WithChroma, pred_mode)?;
        ((cbp & 0xF) as u8, (cbp >> 4) as u8)
    };
    if cbp_luma > 0 || cbp_chroma > 0 || matches!(kind, MbKind::Intra16x16 { .. }) {
        let mut g = BoundedGolomb::new(r, budget);
        let _mb_qp_delta = g.se_v(-26, 25)?;
        decode_residual(r, budget, &kind, cbp_luma, cbp_chroma, grid, mb_x, mb_y)?;
    } else {
        // No residual at all: every block this macroblock owns reports
        // TotalCoeff 0 to its neighbours (clause 9.2.1's "not coded"
        // substitution), same as an explicit CBP of 0 would.
        zero_out_mb_neighbours(grid, mb_x, mb_y);
    }

    Ok(())
}

/// Record a macroblock's `TotalCoeff` as 0 for every 4x4 luma and chroma
/// block it owns, for the neighbour derivation clause 9.2.1 requires of any
/// macroblock that carries no residual — an explicit CBP of 0, an
/// `Intra16x16` macroblock with no AC/DC coded, and — the case this exists
/// for — a macroblock skipped via `mb_skip_run`/`mb_skip_flag`. A skipped
/// macroblock never calls [`decode_macroblock_cavlc`] at all, so without
/// this, its neighbour blocks stay `NBlock(None)` (unavailable) forever,
/// silently steering the *next* macroblock's `nC` derivation onto the wrong
/// row of the `coeff_token` table.
fn zero_out_mb_neighbours(grid: &mut NeighbourGrid, mb_x: u32, mb_y: u32) {
    for blk in 0..16 {
        let (bx, by) = blk_xy(blk);
        grid.set_luma(mb_x * 4 + bx, mb_y * 4 + by, 0);
    }
    for comp in 0..2 {
        for blk in 0..4 {
            let (bx, by) = blk_xy(blk);
            grid.set_chroma(comp, mb_x * 2 + bx % 2, mb_y * 2 + by % 2, 0);
        }
    }
}

fn decode_mb_pred_intra(
    r: &mut BitReader<'_>,
    budget: &mut Budget,
    kind: &MbKind,
    grid: &NeighbourGrid,
    mb_x: u32,
    mb_y: u32,
) -> Result<()> {
    let _ = (grid, mb_x, mb_y);
    if matches!(kind, MbKind::Intra4x4) {
        for _ in 0..16 {
            let flag = r.try_get(1)?;
            if flag == 0 {
                let _rem = r.try_get(3)?;
            }
        }
    }
    let mut g = BoundedGolomb::new(r, budget);
    let _intra_chroma_pred_mode = g.ue_v(3)?;
    Ok(())
}

fn decode_mb_pred_inter(
    r: &mut BitReader<'_>,
    budget: &mut Budget,
    header: &SliceHeader,
    kind: &MbKind,
    is_b: bool,
) -> Result<()> {
    match kind {
        MbKind::BDirect16x16 => Ok(()),
        MbKind::Inter { parts } => decode_parts(r, budget, header, parts, &[true; 4]),
        MbKind::P8x8 { ref0_inferred } => decode_sub_mb_pred(r, budget, header, false, *ref0_inferred),
        MbKind::B8x8 => decode_sub_mb_pred(r, budget, header, true, false),
        _ => {
            let _ = is_b;
            Ok(())
        }
    }
}

/// `ref_idx`/`mvd` for a fixed list of whole-macroblock partitions (§7.3.5.1's
/// `mb_pred()` inter branch). `mvd_present` lets the sub-macroblock caller
/// suppress `mvd` reads for a `B_Direct_8x8` sub-partition while still
/// reusing this same per-list loop shape for `ref_idx`.
fn decode_parts(
    r: &mut BitReader<'_>,
    budget: &mut Budget,
    header: &SliceHeader,
    parts: &[PartPred],
    mvd_present: &[bool],
) -> Result<()> {
    let n0 = header.num_ref_idx_l0_active_minus1;
    let n1 = header.num_ref_idx_l1_active_minus1;
    for &p in parts {
        if p.reads_l0() && n0 > 0 {
            let mut g = BoundedGolomb::new(r, budget);
            let _ = g.te_v(n0)?;
        }
    }
    for &p in parts {
        if p.reads_l1() && n1 > 0 {
            let mut g = BoundedGolomb::new(r, budget);
            let _ = g.te_v(n1)?;
        }
    }
    for (i, &p) in parts.iter().enumerate() {
        if p.reads_l0() && mvd_present.get(i).copied().unwrap_or(true) {
            let mut g = BoundedGolomb::new(r, budget);
            let _ = g.se_v(-8192, 8191)?;
            let _ = g.se_v(-8192, 8191)?;
        }
    }
    for (i, &p) in parts.iter().enumerate() {
        if p.reads_l1() && mvd_present.get(i).copied().unwrap_or(true) {
            let mut g = BoundedGolomb::new(r, budget);
            let _ = g.se_v(-8192, 8191)?;
            let _ = g.se_v(-8192, 8191)?;
        }
    }
    Ok(())
}

fn decode_sub_mb_pred(
    r: &mut BitReader<'_>,
    budget: &mut Budget,
    header: &SliceHeader,
    is_b: bool,
    ref0_inferred: bool,
) -> Result<()> {
    let mut subs: Vec<(u8, Option<PartPred>)> = budget.alloc(4)?;
    subs.clear();
    for _ in 0..4 {
        let code = {
            let mut g = BoundedGolomb::new(r, budget);
            g.ue_v(12)?
        };
        subs.push(classify_sub_mb_type(is_b, code)?);
    }

    let n0 = header.num_ref_idx_l0_active_minus1;
    let n1 = header.num_ref_idx_l1_active_minus1;
    if !ref0_inferred {
        for &(_, pred) in &subs {
            if pred.is_some_and(PartPred::reads_l0) && n0 > 0 {
                let mut g = BoundedGolomb::new(r, budget);
                let _ = g.te_v(n0)?;
            }
        }
    }
    for &(_, pred) in &subs {
        if pred.is_some_and(PartPred::reads_l1) && n1 > 0 {
            let mut g = BoundedGolomb::new(r, budget);
            let _ = g.te_v(n1)?;
        }
    }
    for &(num_sub, pred) in &subs {
        let Some(p) = pred else { continue };
        if p.reads_l0() {
            for _ in 0..num_sub {
                let mut g = BoundedGolomb::new(r, budget);
                let _ = g.se_v(-8192, 8191)?;
                let _ = g.se_v(-8192, 8191)?;
            }
        }
    }
    for &(num_sub, pred) in &subs {
        let Some(p) = pred else { continue };
        if p.reads_l1() {
            for _ in 0..num_sub {
                let mut g = BoundedGolomb::new(r, budget);
                let _ = g.se_v(-8192, 8191)?;
                let _ = g.se_v(-8192, 8191)?;
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn decode_residual(
    r: &mut BitReader<'_>,
    budget: &mut Budget,
    kind: &MbKind,
    cbp_luma: u8,
    cbp_chroma: u8,
    grid: &mut NeighbourGrid,
    mb_x: u32,
    mb_y: u32,
) -> Result<()> {
    let is_16x16 = matches!(kind, MbKind::Intra16x16 { .. });

    if is_16x16 {
        let nc = grid.nc_luma(mb_x * 4, mb_y * 4);
        let out = residual_block_cavlc(r, nc, 16, budget)?;
        grid.set_luma(mb_x * 4, mb_y * 4, out.total_coeff);
    }

    for i8x8 in 0..4u32 {
        for i4x4 in 0..4u32 {
            let blk = i8x8 * 4 + i4x4;
            let (bx, by) = blk_xy(blk);
            let x = mb_x * 4 + bx;
            let y = mb_y * 4 + by;
            if cbp_luma & (1 << i8x8) != 0 {
                let nc = grid.nc_luma(x, y);
                let max_num_coeff = if is_16x16 { 15 } else { 16 };
                let out = residual_block_cavlc(r, nc, max_num_coeff, budget)?;
                grid.set_luma(x, y, out.total_coeff);
            } else {
                grid.set_luma(x, y, 0);
            }
        }
    }

    for _comp in 0..2usize {
        if cbp_chroma & 3 != 0 {
            let _out = residual_block_cavlc(r, -1, 4, budget)?;
        }
    }
    for comp in 0..2usize {
        for i4x4 in 0..4u32 {
            let (bx, by) = blk_xy(i4x4);
            let x = mb_x * 2 + bx % 2;
            let y = mb_y * 2 + by % 2;
            if cbp_chroma & 2 != 0 {
                let nc = grid.nc_chroma(comp, x, y);
                let out = residual_block_cavlc(r, nc, 15, budget)?;
                grid.set_chroma(comp, x, y, out.total_coeff);
            } else {
                grid.set_chroma(comp, x, y, 0);
            }
        }
    }
    Ok(())
}
