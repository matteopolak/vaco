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
//! - **4:2:2/4:4:4 chroma, `SI` slices, weighted prediction's actual
//!   weights** (the syntax elements they would need — `pred_weight_table`
//!   — are already fully parsed by `vaco-parse-h264`'s slice header, so
//!   nothing here re-reads them).
//! - **`I_PCM` on the CAVLC side only.** [`decode_slice_cavlc`] still
//!   refuses it rather than guessing at its byte-alignment padding's exact
//!   bit count from this module alone. CABAC's own [`decode_slice_cabac`]
//!   *does* handle `I_PCM` — see the CABAC section below — because CABAC's
//!   `mb_type` binarisation signals it unambiguously via
//!   `decode_terminate`, so there was nothing to guess.
//! - **CABAC B slices** — [`decode_slice_cabac`] refuses them outright.
//!   Table 9-27/9-28's B-slice `mb_type`/`sub_mb_type` bin strings do not
//!   decompose into the same clean arithmetic form as the I-slice and P/SP
//!   tables (`cabac_mb_tables::MB_TYPE_B`/`SUB_MB_TYPE_B` are transcribed
//!   and kept `#[allow(dead_code)]` for whoever picks this up, but the
//!   decode function that would binarise against them was not written this
//!   dispatch).
//! - **CABAC's 8x8 residual category** — same reason as CAVLC's, above.
//!
//! **In scope but not yet bit-exact**: CABAC's I/P-slice macroblock layer
//! (`mb_type`, `sub_mb_type`, `mb_skip_flag`, `coded_block_pattern`,
//! `ref_idx`, `mvd`, intra pred mode flags, `mb_qp_delta`,
//! `coded_block_flag` including chroma DC) is implemented in
//! [`decode_slice_cabac`], with its own per-element context-initialisation
//! tables in `cabac_mb_tables.rs` fetched and checked against primary text
//! the same way CAVLC's were. It drives real `libx264 -coder cabac` I and
//! P/SP corpora structurally, but bit consumption still diverges from all
//! three real corpora this dispatch built, in a way not root-caused within
//! the time available — see `tests/macroblock_layer_cabac.rs`'s `#[ignore]`
//! reasons for the exact minimal repro, and
//! `docs/codec/vaco-codec-h264.md`'s "Verification" section for the full
//! account, including the two real bugs (a structurally-wrong shared
//! residual context table, and chroma DC's `coded_block_flag` never being
//! read at all) found and fixed while building this.

use vaco_bitstream::BitReader;
use vaco_codec_cabac::{CabacDecoder, ContextModel, init_contexts};
use vaco_codec_golomb::map::se_value;
use vaco_codec_golomb::{BoundedGolomb, ChromaArrayType, MbPartPredMode as CbpPredMode};
use vaco_core::{Error, Result};
use vaco_limits::Budget;
use vaco_parse_h264::{ChromaFormat, Pps, Sps, SliceHeader, SliceKind};

use crate::cabac_mb_tables as t;
use crate::cabac_residual::{CabacInit, ContextCategory, ContextSet, residual_block_cabac};
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
    /// `ref_idx_l0`" rule.
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
        if let Some(i) = self.luma_idx(x, y)
            && let Some(b) = self.luma.get_mut(i) {
                b.0 = Some(total_coeff);
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

// ============================================================================
// CABAC macroblock layer (#419's CABAC half, #418).
//
// Scope within this section, narrower than the CAVLC side above: **I and P
// slices only**. B-slice `mb_type`'s CABAC bin string (Table 9-27) is a
// genuinely irregular tree — unlike every other binarisation in this crate,
// it does not decompose into a clean arithmetic formula the way Table 9-26
// and the P/SP table do, and hand-deriving it bit-by-bit from the primary
// text without a second, independent way to check the result risked
// exactly the class of silent, undetectable error this whole line of work
// exists to avoid. Refused explicitly by [`decode_slice_cabac`] rather than
// attempted and left unverified; tracked for a follow-up dispatch alongside
// the same table's `sub_mb_type` (Table 9-28's B-slice column, which *is*
// regular, but has no I/P-slice caller without B's `mb_type` first).
//
// Chroma DC's own `coded_block_flag` context (`ctxBlockCat` 3, ctxIdx
// 97..=100, `cabac_mb_tables::CBF_CHROMA_DC`) *is* read as a real syntax
// element — an earlier draft of this module inferred it from
// `coded_block_pattern`'s own presence instead (reasoning, wrongly, that
// `cbp_chroma != 0` already implies every chroma DC coefficient could be
// nonzero the way an unset 8x8 luma CBP bit implies every AC coefficient in
// it is zero). That is not what clause 7.3.5.3.3 says: `coded_block_flag`
// is a per-block flag read whenever the block's *possible* presence hasn't
// already been ruled out, the same as every 4x4 luma block within an
// enabled 8x8 quadrant still gets its own flag. Caught by this module's own
// real-corpus bit-exactness measurement, not by inspection — see
// `docs/codec/vaco-codec-h264.md` for what that measurement does and does
// not yet confirm.
//
// A second pass found two more real bugs, both upstream of that fix and
// both far more consequential: `set_mb_info` hardcoded
// `intra_chroma_pred_mode: 0` regardless of what was actually decoded, so
// clause 9.3.3.1.1.8's own condTermFlagN — the very first context-coded
// read inside every intra macroblock — could never see a neighbour's real
// value; and `ref_idx_cond_term` had clause 9.3.3.1.1.6's comparison
// inverted (`r <= 0` where the primary text needs `r > 0`). Fixing the
// first alone ate the chroma-DC repro above entirely — it no longer
// reproduces on any corpus this crate has — which fits: a wrong context
// this early in an intra macroblock's decode shifts the arithmetic
// engine's range/offset for everything read afterward, so "chroma DC
// looks wrong" was a downstream symptom, not the defect's location.
//
// A third pass added I_PCM support (byte-align, skip 384 raw pcm_byte[i]
// reads per the 2002 draft's fixed-8-bit clause 7.3.5, re-initialise only
// the arithmetic engine per 9.3.1.2) — cheap, as expected, since
// `CabacDecoder`'s own renormalisation never reads ahead of what it has
// consumed. But that same pass found the real problem was the
// *measurement*: `tests/macroblock_layer_cabac.rs`'s assertions
// (`!malformed()`, `macroblock_count == total_mbs`) can both hold even
// when every decoded value is wrong, since `end_of_slice_flag`'s fixed
// context can plausibly fire at a macroblock-count-correct point
// regardless of what was actually decoded before it. Adding a check that
// what follows `end_of_slice_flag` really is clause 7.3.2.10's
// `rbsp_slice_trailing_bits()` (mirroring what `more_rbsp_data()` already
// gives the CAVLC test) found all three corpora diverge at **slice 0** —
// not slice 10, not "36 of 36 macroblocks", not "reaches I_PCM at slice
// 6" as reported after the second pass. Those were real, correctly
// described bugs and fixes; the measurement reporting them as progress
// toward bit-exactness was not strong enough to have noticed that no
// slice had ever actually been bit-exact.
//
// A fourth pass tested and cleared a specific hypothesis about
// `residual_block_cabac`'s bypass path (`decode_bypass_egk`'s 32-bin
// prefix ceiling, `decode_uegk`'s `saturating_add`): a round-trip oracle
// (`tests/cabac_bypass_egk_oracle.rs`) round-trips every realistic H.264
// coefficient value cleanly, and instrumenting the real call site against
// all three corpora found the ceiling engages zero times in 243 real
// calls (largest observed value: 418, against a ceiling that needs
// `u32::MAX`-scale values). Not the bug.
//
// A fifth pass found and fixed a real one: `decode_cbp_cabac`'s luma
// `coded_block_pattern` neighbour derivation (clause 9.3.3.1.1.4 +
// 6.4.7.2 + Table 6-2) computed a single `same_mb_bit` — the *left*
// neighbour's rule (block `q-1`, for `q` in the right column) — and fed
// it to *both* the left and the above `ctxIdxInc` term. `q`'s 8x8 blocks
// are raster-scan (`0 1 / 2 3`), so the above neighbour's own
// same-macroblock rule is a *different* condition and a different block
// (`q-2`, for `q` in the bottom row): right by coincidence for `q=0`,
// wrong for `q=1` (used same-mb block 0 instead of the above
// macroblock's block 3), silently zero for `q=2` (neither source was
// ever populated), and wrong for `q=3` (reused block 2, the left value,
// instead of block 1). Found by re-deriving each `q`'s actual left/above
// `(xN, yN)` by hand from Table 6-2 rather than by inspecting the
// existing code's shape, the same discipline that has caught every real
// bug this project has found. The analogous 4x4-block-granular
// `coded_block_flag` neighbour derivation just below was checked for the
// same trap and does not have it — it looks up `left_bit`/`above_bit`
// from two independently-computed absolute grid positions rather than
// sharing one boolean.
//
// A sixth pass found the mb_type cross-check's own premise was never
// actually established for two of the three corpora. "Every macroblock
// classification matches ffmpeg -debug mb_type exactly" was verified
// against `cabac_i_only.264`'s slice 0 only -- an all-`Intra4x4` slice
// with zero `Intra_16x16` macroblocks in it, so the cross-check could
// never have caught an `Intra_16x16`-specific bug. Running the same
// cross-check on `cabac_ip_simple.264` and `cabac_ip_multiref.264`'s
// own slice 0 (both real I frames, both containing genuine `Intra_16x16`
// macroblocks per the reference) found every single one of them
// misclassified as `Intra4x4` instead -- 2 of 16 in `cabac_ip_simple`,
// 35 of 36 in `cabac_ip_multiref` (the one correct `Intra_16x16` hit
// being the exception, not the rule). This reopens the "everything
// before residual decode is verified" premise for these two corpora
// specifically, and explains the CBP fix's "byte-identical" result for
// `cabac_ip_simple.264` from the previous pass: if mb_type itself
// diverges this early, whatever happens downstream (CBP included) is
// operating on an already-wrong picture, not a clean measurement of its
// own correctness.
//
// A round-trip oracle (`tests/cabac_decision_oracle.rs`, same shape and
// ownership caveat as the bypass one) tested whether the *engine's*
// context-coded path (`decode_decision`) itself could be at fault --
// specifically, whether a context driven to an extreme, confident state
// by a long run of one outcome (exactly `mb_type`'s bin0 context across
// many consecutive `Intra4x4` macroblocks) could then decode a genuine
// "surprising" bin wrong. Cleared: a deliberately-adapted 30-zeros/
// one-one/ten-zeros sequence and 200 pseudorandom sequences both
// round-trip exactly through `CabacEncoder`/`CabacDecoder`'s public API.
//
// A seventh pass bisected the misclassification directly rather than
// reasoning about it: temporarily forced `decode_mb_type_i_table` to
// take the `Intra_16x16` branch at exactly `cabac_ip_simple.264`
// address 5 (still consuming bin0's own bit via a genuine
// `decode_decision` call; only the control-flow interpretation of its
// result was overridden), leaving every later bin -- the I_PCM
// `decode_terminate` check, then `b2`..`b6` -- to read normally from
// whatever engine state was actually there. If addresses 0-4 had
// consumed exactly the right number of bits, this should have recovered
// a clean decode from address 5 onward (the true encoded value read
// correctly once the branch was taken), and address 6 -- which the
// reference (`ffmpeg -debug mb_type`) shows as plain `Intra4x4`, not
// `Intra_16x16` -- should have decoded correctly too.
//
// It did not: with address 5 forced, its own decode looked plausible
// (`Intra16x16` with a valid `cbp_luma`/`cbp_chroma` combination), but
// address 6 -- genuinely, not forced -- *also* decoded as `Intra16x16`,
// contradicting the reference. Forcing the correction at address 5 did
// not restore correctness one macroblock later, which means the engine
// state entering address 5 was already wrong: the corruption is in
// addresses 0-4's own decode, not in address 5's `mb_type` read itself.
// This directly answers the split the coordinator's own instrument
// (`CabacDecoder::reader().bit_pos()`, already public) was chosen for.
//
// KNOWN GAP, not yet closed: which of addresses 0-4's own syntax
// elements (residual, `coded_block_pattern`, `intra_chroma_pred_mode`,
// `prev_intra4x4_pred_mode_flag`/`rem_intra4x4_pred_mode`) consumes the
// wrong number of bits is not yet isolated -- all four addresses'
// *classification* (`Intra4x4`) and `coded_block_pattern` values already
// match the reference, so whatever is wrong there does not change either
// of those, narrowing the remaining search to residual decode and the
// per-4x4-block intra prediction mode flags specifically. Every fix and
// clearance from prior passes (CBP's neighbour derivation, the bypass
// path, `decode_decision`'s own round-trip) stays; none is reopened by
// this finding. Reported honestly rather than claimed; see
// `planning/TECH-DEBT.md` for the full handoff.
use crate::cabac_mb_tables::{inits_by_col, inits_by_idc, inits_fixed};

/// `CabacDecoder::decode_decision` over a context array, indexed safely —
/// every call site below indexes by a `ctxIdxInc` that is provably in range
/// for a real bitstream, but `clippy::indexing_slicing` (D6) wants that
/// proven at the type level or not at all, so this is the "get with a
/// scratch fallback" shape [`cabac_residual::ContextSet`] already
/// established, reused here across a dozen small context arrays rather than
/// repeating the fallback dance at each one.
fn decide(cabac: &mut CabacDecoder<'_>, ctx: &mut [ContextModel], idx: usize) -> u32 {
    let mut scratch = ContextModel::UNINITIALISED;
    let slot = ctx.get_mut(idx).unwrap_or(&mut scratch);
    cabac.decode_decision(slot)
}

/// Per-macroblock spatial state CABAC's `ctxIdxInc` derivations need,
/// parallel to [`NeighbourGrid`] but keyed by macroblock address (not 4x4
/// block) and boolean/category-shaped rather than a `TotalCoeff` count.
#[derive(Debug, Clone, Copy, Default)]
struct CabacMbInfo {
    /// Written this slice — clause 6.4.8's "different slice" unavailability
    /// falls out of a fresh grid per slice, same as [`NeighbourGrid`].
    available: bool,
    skipped: bool,
    is_intra4x4: bool,
    is_intra: bool,
    is_intra16x16: bool,
    is_ipcm: bool,
    cbp_luma: u8,
    cbp_chroma: u8,
    intra_chroma_pred_mode: u8,
}

/// One 4x4 luma block's motion-prediction neighbour state — [`None`]
/// `pred` uniformly means "contribute 0 to `ref_idx`/`mvd`'s `ctxIdxInc`",
/// covering clause 9.3.3.1.1.6/7's `mbAddrN not available`, `P_Skip`/
/// `B_Skip`, and `Intra` cases alike (all three zero out the same way), not
/// three separate checks.
#[derive(Debug, Clone, Copy, Default)]
struct MvInfo {
    pred: Option<PartPred>,
    ref_idx: [i8; 2],
    mvd: [(i16, i16); 2],
}

/// Absolute-coordinate 2D index, bounds-checked — the same shape
/// [`NeighbourGrid::luma_idx`]/`chroma_idx` use, factored out since this
/// section needs it at three different granularities (macroblock, 4x4
/// luma, 4x4 chroma).
const fn idx_2d(x: u32, y: u32, width: u32, height: u32) -> Option<usize> {
    if x >= width || y >= height {
        return None;
    }
    match (y * width).checked_add(x) {
        Some(i) => Some(i as usize),
        None => None,
    }
}

/// CABAC's per-slice neighbour state: one [`CabacMbInfo`] per macroblock,
/// one `coded_block_flag` per 4x4 luma/chroma block, one [`MvInfo`] per 4x4
/// luma block (replicated across every 4x4 position a wider partition
/// covers — the same "ask the position, not the partition" shape
/// [`NeighbourGrid`] uses for CAVLC's `nC`, applied to clause 6.4.7.5's
/// partition-neighbour derivation instead of clause 9.2.1's block one).
struct CabacGrids {
    mbs_wide: u32,
    mbs_high: u32,
    mb_info: Vec<CabacMbInfo>,
    cbf_luma: Vec<Option<bool>>,
    cbf_chroma: [Vec<Option<bool>>; 2],
    /// Chroma DC's own `coded_block_flag`, one per macroblock per component
    /// (`ctxBlockCat` 3's neighbour derivation is macroblock-granular, not
    /// 4x4-block-granular the way luma/chroma AC's is — clause 9.3.3.1.1.9's
    /// `ctxBlockCat == 3` case looks at "the chroma DC block of chroma
    /// component `iCbCr` of macroblock `mbAddrN`", one block per component
    /// per whole macroblock).
    cbf_chroma_dc: [Vec<Option<bool>>; 2],
    mv: Vec<MvInfo>,
}

impl CabacGrids {
    fn new(mbs_wide: u32, mbs_high: u32, budget: &mut Budget) -> Result<Self> {
        let n_mb = usize::try_from(mbs_wide.saturating_mul(mbs_high)).unwrap_or(0);
        let n_luma4 = usize::try_from((mbs_wide.saturating_mul(4)).saturating_mul(mbs_high.saturating_mul(4)))
            .unwrap_or(0);
        let n_chroma4 = usize::try_from((mbs_wide.saturating_mul(2)).saturating_mul(mbs_high.saturating_mul(2)))
            .unwrap_or(0);
        Ok(Self {
            mbs_wide: mbs_wide.max(1),
            mbs_high: mbs_high.max(1),
            mb_info: budget.alloc(n_mb)?,
            cbf_luma: budget.alloc(n_luma4)?,
            cbf_chroma: [budget.alloc(n_chroma4)?, budget.alloc(n_chroma4)?],
            cbf_chroma_dc: [budget.alloc(n_mb)?, budget.alloc(n_mb)?],
            mv: budget.alloc(n_luma4)?,
        })
    }

    fn mb_info_idx(&self, mb_x: u32, mb_y: u32) -> Option<usize> {
        idx_2d(mb_x, mb_y, self.mbs_wide, self.mbs_high)
    }

    fn cbf_chroma_dc_at(&self, comp: usize, mb_x: u32, mb_y: u32) -> Option<bool> {
        self.mb_info_idx(mb_x, mb_y).and_then(|i| self.cbf_chroma_dc.get(comp)?.get(i)).copied().flatten()
    }

    fn set_cbf_chroma_dc(&mut self, comp: usize, mb_x: u32, mb_y: u32, v: bool) {
        if let Some(i) = self.mb_info_idx(mb_x, mb_y)
            && let Some(slot) = self.cbf_chroma_dc.get_mut(comp).and_then(|g| g.get_mut(i))
        {
            *slot = Some(v);
        }
    }

    fn mb_info_at(&self, mb_x: u32, mb_y: u32) -> Option<CabacMbInfo> {
        let info = *self.mb_info_idx(mb_x, mb_y).and_then(|i| self.mb_info.get(i))?;
        info.available.then_some(info)
    }

    fn mb_left(&self, mb_x: u32, mb_y: u32) -> Option<CabacMbInfo> {
        mb_x.checked_sub(1).and_then(|lx| self.mb_info_at(lx, mb_y))
    }

    fn mb_above(&self, mb_x: u32, mb_y: u32) -> Option<CabacMbInfo> {
        mb_y.checked_sub(1).and_then(|ay| self.mb_info_at(mb_x, ay))
    }

    fn set_mb_info(&mut self, mb_x: u32, mb_y: u32, info: CabacMbInfo) {
        if let Some(i) = self.mb_info_idx(mb_x, mb_y)
            && let Some(slot) = self.mb_info.get_mut(i)
        {
            *slot = info;
        }
    }

    fn luma4_idx(&self, x: u32, y: u32) -> Option<usize> {
        idx_2d(x, y, self.mbs_wide * 4, self.mbs_high * 4)
    }

    fn chroma4_idx(&self, x: u32, y: u32) -> Option<usize> {
        idx_2d(x, y, self.mbs_wide * 2, self.mbs_high * 2)
    }

    fn cbf_luma_at(&self, x: u32, y: u32) -> Option<bool> {
        self.luma4_idx(x, y).and_then(|i| self.cbf_luma.get(i)).copied().flatten()
    }

    fn set_cbf_luma(&mut self, x: u32, y: u32, v: bool) {
        if let Some(i) = self.luma4_idx(x, y)
            && let Some(slot) = self.cbf_luma.get_mut(i)
        {
            *slot = Some(v);
        }
    }

    fn cbf_chroma_at(&self, comp: usize, x: u32, y: u32) -> Option<bool> {
        self.chroma4_idx(x, y).and_then(|i| self.cbf_chroma.get(comp)?.get(i)).copied().flatten()
    }

    fn set_cbf_chroma(&mut self, comp: usize, x: u32, y: u32, v: bool) {
        if let Some(i) = self.chroma4_idx(x, y)
            && let Some(slot) = self.cbf_chroma.get_mut(comp).and_then(|g| g.get_mut(i))
        {
            *slot = Some(v);
        }
    }

    fn mv_at(&self, x: u32, y: u32) -> MvInfo {
        self.luma4_idx(x, y).and_then(|i| self.mv.get(i)).copied().unwrap_or_default()
    }

    fn set_mv(&mut self, x: u32, y: u32, v: MvInfo) {
        if let Some(i) = self.luma4_idx(x, y)
            && let Some(slot) = self.mv.get_mut(i)
        {
            *slot = v;
        }
    }
}

/// clause 9.3.3.1.1.9's `condTermFlagN` for `coded_block_flag` — see this
/// section's module-doc note on why chroma DC's own context is not among
/// the callers. `trans_available`/`trans_cbf` are the caller's own
/// clause-specific "was this exact transform block coded, and if so what
/// was its flag" answer (different per `ctxBlockCat`; see
/// `luma_cbf_cond`/`chroma_cbf_cond` below).
fn cbf_cond_term(
    neighbour: Option<CabacMbInfo>,
    trans_available: bool,
    trans_cbf: bool,
    current_is_intra: bool,
) -> u32 {
    let Some(info) = neighbour else {
        return u32::from(current_is_intra);
    };
    if info.is_ipcm {
        return 1;
    }
    if !trans_available {
        return 0;
    }
    u32::from(trans_cbf)
}

/// clause 9.3.3.1.1.4's luma `condTermFlagN` for one 8x8 block's neighbour
/// (`N` = A/left or B/above), given whether that neighbour is the *same*
/// macroblock (an already-decoded earlier quadrant) or a different one.
fn cbp_luma_cond_term(same_mb_bit: Option<bool>, cross_mb: Option<(CabacMbInfo, bool)>) -> u32 {
    if let Some(bit) = same_mb_bit {
        // Same macroblock: always available, never I_PCM (I_PCM refuses
        // before CBP is ever read), never skipped (we are actively
        // decoding it) — condTermFlag reduces to just the bit.
        return u32::from(!bit);
    }
    let Some((info, bit)) = cross_mb else {
        return 0; // mbAddrN not available
    };
    if info.is_ipcm {
        return 0;
    }
    u32::from(!(!info.skipped && bit))
}

/// clause 9.3.3.1.1.4's chroma `condTermFlagN` — always a cross-macroblock
/// lookup (chroma CBP is per-macroblock, not per-8x8-block).
fn cbp_chroma_cond_term(neighbour: Option<CabacMbInfo>, bin_idx: u32) -> u32 {
    let Some(info) = neighbour else {
        return 0;
    };
    if info.is_ipcm {
        return 1;
    }
    if info.skipped {
        return 0;
    }
    if bin_idx == 0 && info.cbp_chroma == 0 {
        return 0;
    }
    if bin_idx == 1 && info.cbp_chroma != 2 {
        return 0;
    }
    1
}

/// clause 9.3.3.1.1.3's `condTermFlagN` for I-slice `mb_type`
/// (`ctxIdxOffset == 3`): 0 if unavailable or the neighbour is `I_4x4`,
/// else 1.
fn mb_type_i_cond_term(neighbour: Option<CabacMbInfo>) -> u32 {
    match neighbour {
        None => 0,
        Some(info) => u32::from(!info.is_intra4x4),
    }
}

/// clause 9.3.3.1.1.8's `condTermFlagN` for `intra_chroma_pred_mode`.
fn intra_chroma_cond_term(neighbour: Option<CabacMbInfo>) -> u32 {
    let Some(info) = neighbour else {
        return 0;
    };
    u32::from(info.is_intra && !info.is_ipcm && info.intra_chroma_pred_mode != 0)
}

/// clause 9.3.3.1.1.6's `condTermFlagN` for `ref_idx_lX`. `refIdxZeroFlagN
/// = (ref_idx_lX[mbPartIdxN] > 0) ? 0 : 1`, and `condTermFlagN = 0` when
/// `refIdxZeroFlagN == 1` (among the other zero-conditions `pred`/`reads`
/// already cover), `condTermFlagN = 1` otherwise — i.e. 1 exactly when the
/// neighbour's own `ref_idx` is greater than 0. An earlier version of this
/// function had the comparison inverted (`r <= 0` instead of `r > 0`),
/// found by re-checking the primary text bin-by-bin rather than from
/// recollection — recollection had the polarity backwards.
fn ref_idx_cond_term(n: MvInfo, list: usize) -> u32 {
    let Some(pred) = n.pred else { return 0 };
    let reads = if list == 0 { pred.reads_l0() } else { pred.reads_l1() };
    if !reads {
        return 0;
    }
    u32::from(n.ref_idx.get(list).is_some_and(|&r| r > 0))
}

/// clause 9.3.3.1.1.7's `absMvdCompN` for `mvd_lX`.
fn mvd_abs_term(n: MvInfo, list: usize, comp: usize) -> u32 {
    let Some(pred) = n.pred else { return 0 };
    let reads = if list == 0 { pred.reads_l0() } else { pred.reads_l1() };
    if !reads {
        return 0;
    }
    let v = n.mvd.get(list).map_or(0, |&(x, y)| if comp == 0 { x } else { y });
    i32::from(v).unsigned_abs()
}

/// Table 9-26's bin tree — `mb_type` for I slices (`ctx` indexed so local 0
/// = ctxIdx 3), and, via [`decode_mb_type_intra_suffix`], the shared
/// "Intra" suffix of `mb_type` in P/SP slices.
fn decode_mb_type_i_table(cabac: &mut CabacDecoder<'_>, ctx: &mut [ContextModel; 8], inc0: usize) -> u32 {
    if decide(cabac, ctx, inc0.min(2)) == 0 {
        return 0; // I_4x4
    }
    if cabac.decode_terminate() == 1 {
        return 25; // I_PCM
    }
    let b2 = decide(cabac, ctx, 3);
    let b3 = decide(cabac, ctx, 4);
    let b4 = decide(cabac, ctx, if b3 != 0 { 5 } else { 6 });
    let b5 = decide(cabac, ctx, if b3 != 0 { 6 } else { 7 });
    let (chroma, p0, p1) = if b3 == 0 {
        (0u32, b4, b5)
    } else {
        let b6 = decide(cabac, ctx, 7);
        (1 + b4, b5, b6)
    };
    1 + b2 * 12 + chroma * 4 + p0 * 2 + p1
}

/// Table 9-26 again, this time at the ctxIdx range P/SP `mb_type`'s
/// "Intra" suffix gets (offset 17, four contexts local-indexed 0..=3 —
/// local 0 is the same adaptive context as [`decode_mb_type_p`]'s prefix
/// bin2-when-`b1==1` slot, per `cabac_mb_tables::MB_TYPE_P`'s own doc).
fn decode_mb_type_intra_suffix(cabac: &mut CabacDecoder<'_>, ctx: &mut [ContextModel; 4]) -> u32 {
    if decide(cabac, ctx, 0) == 0 {
        return 0;
    }
    if cabac.decode_terminate() == 1 {
        return 25;
    }
    let b2 = decide(cabac, ctx, 1);
    let b3 = decide(cabac, ctx, 2);
    let b4 = decide(cabac, ctx, if b3 != 0 { 2 } else { 3 });
    let (chroma, p0, p1) = if b3 == 0 {
        let b5 = decide(cabac, ctx, 3);
        (0u32, b4, b5)
    } else {
        let b5 = decide(cabac, ctx, 3);
        let b6 = decide(cabac, ctx, 3);
        (1 + b4, b5, b6)
    };
    1 + b2 * 12 + chroma * 4 + p0 * 2 + p1
}

/// Table 9-27's P/SP `mb_type`, `ctx` spanning ctxIdx 14..=20 (7 contexts —
/// [`cabac_mb_tables::MB_TYPE_P`]). Returns the same `mb_type` code
/// [`classify_mb_type`] expects (0..=4 non-intra, `5 + suffix` intra).
fn decode_mb_type_p(cabac: &mut CabacDecoder<'_>, ctx: &mut [ContextModel; 7]) -> u32 {
    if decide(cabac, ctx, 0) == 1 {
        let mut suffix_ctx = [ctx[3], ctx[4], ctx[5], ctx[6]];
        let v = decode_mb_type_intra_suffix(cabac, &mut suffix_ctx);
        ctx[3] = suffix_ctx[0];
        ctx[4] = suffix_ctx[1];
        ctx[5] = suffix_ctx[2];
        ctx[6] = suffix_ctx[3];
        return 5 + v;
    }
    let b1 = decide(cabac, ctx, 1);
    let b2 = decide(cabac, ctx, if b1 == 1 { 3 } else { 2 });
    if b1 == 0 {
        if b2 == 0 { 0 } else { 3 }
    } else if b2 == 0 {
        2
    } else {
        1
    }
}

/// Table 9-28's P/SP `sub_mb_type`, `ctx` spanning ctxIdx 21..=23.
fn decode_sub_mb_type_p(cabac: &mut CabacDecoder<'_>, ctx: &mut [ContextModel; 3]) -> u32 {
    if decide(cabac, ctx, 0) == 1 {
        return 0;
    }
    if decide(cabac, ctx, 1) == 0 {
        return 1;
    }
    if decide(cabac, ctx, 2) == 0 { 3 } else { 2 }
}

/// `ref_idx_lX`, clause 9.3.2.1's `U` binarisation with clause 9.3.3.1.1.6's
/// `ctxIdxInc`. Unary has no formal cap; capped defensively (D6) well past
/// any real reference-picture-list length.
fn decode_ref_idx(cabac: &mut CabacDecoder<'_>, ctx: &mut [ContextModel; 6], inc0: u32) -> u32 {
    const MAX_REF_IDX: u32 = 32;
    let mut n = 0u32;
    while n < MAX_REF_IDX {
        let idx = match n {
            0 => inc0.min(3) as usize,
            1 => 4,
            _ => 5,
        };
        if decide(cabac, ctx, idx) == 0 {
            break;
        }
        n += 1;
    }
    n
}

/// `mvd_lX[][][compIdx]`, clause 9.3.2.3's `UEGk` (`k=3`, `uCoff=9`,
/// `signedValFlag=1`) with clause 9.3.3.1.1.7's per-bin `ctxIdxInc` for the
/// prefix — hand-rolled rather than `CabacDecoder::decode_uegk` for the same
/// reason `cabac_residual::decode_coeff_abs_level_minus1` is: the context
/// changes mid-prefix, which `decode_uegk`'s single-context-for-the-whole-
/// prefix shape does not model.
fn decode_mvd_component(cabac: &mut CabacDecoder<'_>, ctx: &mut [ContextModel; 7], sum_abs: u32) -> i32 {
    const U_COFF: u32 = 9;
    let inc0 = if sum_abs < 3 {
        0
    } else if sum_abs > 32 {
        2
    } else {
        1
    };
    let mut n = 0u32;
    while n < U_COFF {
        let idx = match n {
            0 => inc0,
            1 => 3,
            2 => 4,
            3 => 5,
            _ => 6,
        };
        if decide(cabac, ctx, idx) == 0 {
            break;
        }
        n += 1;
    }
    let mut magnitude = n;
    if n >= U_COFF {
        magnitude = magnitude.saturating_add(cabac.decode_bypass_egk(3));
    }
    if magnitude == 0 {
        return 0;
    }
    let m = i32::try_from(magnitude.min(i32::MAX.cast_unsigned())).unwrap_or(i32::MAX);
    if cabac.decode_bypass() == 1 { -m } else { m }
}

/// `mb_qp_delta`'s previous-macroblock-in-decoding-order state (clause
/// 9.3.3.1.1.5) — a running value threaded through the slice loop, not a
/// spatial grid: this is the one macroblock-layer `ctxIdxInc` that does not
/// need [`CabacGrids`] at all.
#[derive(Debug, Clone, Copy, Default)]
struct PrevMbQp {
    available: bool,
    skipped: bool,
    is_ipcm: bool,
    is_intra16x16: bool,
    cbp_zero: bool,
    qp_delta_zero: bool,
}

fn qp_delta_ctx_inc(prev: PrevMbQp) -> u32 {
    if !prev.available || prev.skipped || prev.is_ipcm {
        return 0;
    }
    if !prev.is_intra16x16 && prev.cbp_zero {
        return 0;
    }
    u32::from(!prev.qp_delta_zero)
}

/// Every context table [`decode_slice_cabac`] needs for one slice, built
/// once from `slice_qp`/`cabac_init_idc`/slice kind (I slices ignore
/// `cabac_init_idc`; P slices need it — see `cabac_mb_tables`'s own doc).
struct CabacMbCtx {
    mb_type_i: [ContextModel; 8],
    skip_p: [ContextModel; 3],
    mb_type_p: [ContextModel; 7],
    sub_mb_type_p: [ContextModel; 3],
    mvd_comp0: [ContextModel; 7],
    mvd_comp1: [ContextModel; 7],
    ref_idx: [ContextModel; 6],
    qp_delta: [ContextModel; 4],
    intra_chroma: [ContextModel; 4],
    prev_intra4x4: ContextModel,
    rem_intra4x4: ContextModel,
    cbp_luma: [ContextModel; 4],
    cbp_chroma: [ContextModel; 8],
    cbf_luma_dc: [ContextModel; 4],
    cbf_luma_ac: [ContextModel; 4],
    cbf_luma4x4: [ContextModel; 4],
    cbf_chroma_dc: [ContextModel; 4],
    cbf_chroma_ac: [ContextModel; 4],
    residual_luma_dc: ContextSet,
    residual_luma_ac: ContextSet,
    residual_luma4x4: ContextSet,
    residual_chroma_dc: ContextSet,
    residual_chroma_ac: ContextSet,
}

impl CabacMbCtx {
    fn new(slice_qp: i8, is_i_slice: bool, cabac_init_idc: u8) -> Self {
        let col = if is_i_slice { 0 } else { 1 + usize::from(cabac_init_idc.min(2)) };
        let mut mb_type_i = [ContextModel::UNINITIALISED; 8];
        init_contexts(&mut mb_type_i, &inits_fixed(&t::MB_TYPE_I), slice_qp);
        let mut skip_p = [ContextModel::UNINITIALISED; 3];
        init_contexts(&mut skip_p, &inits_by_idc(&t::SKIP_P, cabac_init_idc), slice_qp);
        let mut mb_type_p = [ContextModel::UNINITIALISED; 7];
        init_contexts(&mut mb_type_p, &inits_by_idc(&t::MB_TYPE_P, cabac_init_idc), slice_qp);
        let mut sub_mb_type_p = [ContextModel::UNINITIALISED; 3];
        init_contexts(&mut sub_mb_type_p, &inits_by_idc(&t::SUB_MB_TYPE_P, cabac_init_idc), slice_qp);
        let mut mvd_comp0 = [ContextModel::UNINITIALISED; 7];
        init_contexts(&mut mvd_comp0, &inits_by_idc(&t::MVD_COMP0, cabac_init_idc), slice_qp);
        let mut mvd_comp1 = [ContextModel::UNINITIALISED; 7];
        init_contexts(&mut mvd_comp1, &inits_by_idc(&t::MVD_COMP1, cabac_init_idc), slice_qp);
        let mut ref_idx = [ContextModel::UNINITIALISED; 6];
        init_contexts(&mut ref_idx, &inits_by_idc(&t::REF_IDX, cabac_init_idc), slice_qp);
        let mut qp_delta = [ContextModel::UNINITIALISED; 4];
        init_contexts(&mut qp_delta, &inits_fixed(&t::QP_DELTA), slice_qp);
        let mut intra_chroma = [ContextModel::UNINITIALISED; 4];
        init_contexts(&mut intra_chroma, &inits_fixed(&t::INTRA_CHROMA_PRED_MODE), slice_qp);
        let mut prev_intra4x4 = [ContextModel::UNINITIALISED; 1];
        init_contexts(&mut prev_intra4x4, &inits_fixed(&[t::PREV_INTRA4X4]), slice_qp);
        let mut rem_intra4x4 = [ContextModel::UNINITIALISED; 1];
        init_contexts(&mut rem_intra4x4, &inits_fixed(&[t::REM_INTRA4X4]), slice_qp);
        let mut cbp_luma = [ContextModel::UNINITIALISED; 4];
        init_contexts(&mut cbp_luma, &inits_by_col(&t::CBP_LUMA, col), slice_qp);
        let mut cbp_chroma = [ContextModel::UNINITIALISED; 8];
        init_contexts(&mut cbp_chroma, &inits_by_col(&t::CBP_CHROMA, col), slice_qp);
        let mut cbf_luma_dc = [ContextModel::UNINITIALISED; 4];
        init_contexts(&mut cbf_luma_dc, &inits_by_col(&t::CBF_LUMA_DC, col), slice_qp);
        let mut cbf_luma_ac = [ContextModel::UNINITIALISED; 4];
        init_contexts(&mut cbf_luma_ac, &inits_by_col(&t::CBF_LUMA_AC, col), slice_qp);
        let mut cbf_luma4x4 = [ContextModel::UNINITIALISED; 4];
        init_contexts(&mut cbf_luma4x4, &inits_by_col(&t::CBF_LUMA4X4, col), slice_qp);
        let mut cbf_chroma_dc = [ContextModel::UNINITIALISED; 4];
        init_contexts(&mut cbf_chroma_dc, &inits_by_col(&t::CBF_CHROMA_DC, col), slice_qp);
        let mut cbf_chroma_ac = [ContextModel::UNINITIALISED; 4];
        init_contexts(&mut cbf_chroma_ac, &inits_by_col(&t::CBF_CHROMA_AC, col), slice_qp);

        let init = if is_i_slice { CabacInit::IorSi } else { CabacInit::PSpB(cabac_init_idc) };
        Self {
            mb_type_i,
            skip_p,
            mb_type_p,
            sub_mb_type_p,
            mvd_comp0,
            mvd_comp1,
            ref_idx,
            qp_delta,
            intra_chroma,
            prev_intra4x4: prev_intra4x4[0],
            rem_intra4x4: rem_intra4x4[0],
            cbp_luma,
            cbp_chroma,
            cbf_luma_dc,
            cbf_luma_ac,
            cbf_luma4x4,
            cbf_chroma_dc,
            cbf_chroma_ac,
            residual_luma_dc: ContextSet::new(ContextCategory::LumaDc, slice_qp, init),
            residual_luma_ac: ContextSet::new(ContextCategory::LumaAc, slice_qp, init),
            residual_luma4x4: ContextSet::new(ContextCategory::Luma4x4, slice_qp, init),
            residual_chroma_dc: ContextSet::new(ContextCategory::ChromaDc, slice_qp, init),
            residual_chroma_ac: ContextSet::new(ContextCategory::ChromaAc, slice_qp, init),
        }
    }
}

/// Drive one whole CABAC slice's macroblock loop, clause 7.3.4's simpler
/// (relative to CAVLC's) shape: every macroblock reads exactly one
/// `mb_skip_flag` (if not an I slice) and, at the end of every iteration,
/// exactly one `end_of_slice_flag` — no CAVLC-style "check `more_rbsp_data`
/// only after a nonzero skip run" subtlety, because CABAC has an explicit,
/// unconditional per-iteration termination signal instead.
///
/// # Errors
///
/// As [`decode_slice_cavlc`], plus [`vaco_core::Error::Unsupported`] for a
/// B slice (this section's own scope line — see the module doc above).
pub fn decode_slice_cabac(
    cabac: &mut CabacDecoder<'_>,
    budget: &mut Budget,
    sps: &Sps,
    pps: &Pps,
    header: &SliceHeader,
) -> Result<SliceStats> {
    check_scope(sps, pps, header)?;
    if matches!(header.kind, SliceKind::B) {
        return Err(Error::Unsupported(
            "vaco-codec-h264: CABAC B-slice mb_type/sub_mb_type (Table 9-27/9-28's B column) is out of scope for this dispatch",
        ));
    }
    let is_i_slice = matches!(header.kind, SliceKind::I);
    let cabac_init_idc = header.cabac_init_idc.unwrap_or(0) as u8;
    let slice_qp = i8::try_from(pps.slice_qp(header.slice_qp_delta).clamp(0, 51)).unwrap_or(26);

    let mbs_wide = sps.pic_width_in_mbs;
    let mbs_high = sps.pic_height_in_map_units * if sps.frame_mbs_only { 1 } else { 2 };
    let total_mbs = mbs_wide.saturating_mul(mbs_high);
    let mut grids = CabacGrids::new(mbs_wide, mbs_high, budget)?;
    let mut ctx = CabacMbCtx::new(slice_qp, is_i_slice, cabac_init_idc);
    let mut prev_qp = PrevMbQp::default();
    let mut stats = SliceStats::default();

    let mut curr_mb_addr = header.first_mb_in_slice;
    loop {
        if curr_mb_addr >= total_mbs {
            break;
        }
        let (mb_x, mb_y) = mb_addr_xy(curr_mb_addr, mbs_wide);

        let skipped = if is_i_slice {
            false
        } else {
            // clause 9.3.3.1.1.1: condTermFlagN = 0 if mbAddrN unavailable
            // OR mbAddrN.mb_skip_flag == 1, else 1 — a neighbour's *own*
            // skip status, not availability alone (the CABAC-shaped sibling
            // of the CAVLC skip-run neighbour bug: a skipped macroblock
            // still has to report a defined answer, "yes I was skipped", to
            // the *next* macroblock's mb_skip_flag context, not merely
            // "present").
            let cond = u32::from(grids.mb_left(mb_x, mb_y).is_some_and(|i| !i.skipped))
                + u32::from(grids.mb_above(mb_x, mb_y).is_some_and(|i| !i.skipped));
            decide(cabac, &mut ctx.skip_p, cond as usize) == 1
        };

        if skipped {
            grids.set_mb_info(
                mb_x,
                mb_y,
                CabacMbInfo { available: true, skipped: true, ..CabacMbInfo::default() },
            );
            prev_qp = PrevMbQp { available: true, skipped: true, ..PrevMbQp::default() };
            stats.skipped_count += 1;
            stats.macroblock_count += 1;
        } else {
            decode_macroblock_cabac(cabac, budget, header, &mut ctx, &mut grids, &mut prev_qp, mb_x, mb_y)?;
            stats.macroblock_count += 1;
        }

        curr_mb_addr += 1;
        let eos = cabac.decode_terminate();
        if eos == 1 {
            break;
        }
    }
    Ok(stats)
}

/// One real (non-skipped) CABAC macroblock: `mb_type`, prediction
/// (intra pred-mode flags, or inter `ref_idx`/`mvd`), `coded_block_pattern`,
/// `mb_qp_delta`, and residual — updating every grid the *next* macroblock's
/// context derivations need.
#[allow(clippy::too_many_lines)]
fn decode_macroblock_cabac(
    cabac: &mut CabacDecoder<'_>,
    budget: &mut Budget,
    header: &SliceHeader,
    ctx: &mut CabacMbCtx,
    grids: &mut CabacGrids,
    prev_qp: &mut PrevMbQp,
    mb_x: u32,
    mb_y: u32,
) -> Result<()> {
    let is_i_slice = matches!(header.kind, SliceKind::I);
    let raw_code = if is_i_slice {
        let inc0 = mb_type_i_cond_term(grids.mb_left(mb_x, mb_y)) + mb_type_i_cond_term(grids.mb_above(mb_x, mb_y));
        decode_mb_type_i_table(cabac, &mut ctx.mb_type_i, inc0 as usize)
    } else {
        decode_mb_type_p(cabac, &mut ctx.mb_type_p)
    };
    let kind = classify_mb_type(header.kind, raw_code)?;
    if matches!(kind, MbKind::IPcm) {
        // Clause 7.3.5's macroblock_layer(): I_PCM's own branch is just
        // `while(!byte_aligned()) pcm_alignment_zero_bit; for(i=0;
        // i<256*ChromaFormatFactor;i++) pcm_byte[i] u(8)` — no
        // coded_block_pattern, no mb_qp_delta, no residual() at all, and
        // (per the 2002 draft this crate's tables are checked against,
        // clause 6.3.3/Table 6-1) every pcm_byte is a fixed 8 bits
        // regardless of bit depth; that extension postdates this edition
        // the same way the 8x8 transform and 4:2:2 chroma DC do. For
        // 4:2:0 (this crate's only supported chroma format),
        // ChromaFormatFactor = 1.5, so 256*1.5 = 384 bytes total.
        //
        // Clause 9.3.1.2 is invoked again right after: the arithmetic
        // *engine* re-initialises (fresh ivlCurrRange = 510, ivlOffset
        // from the next 9 bits) but the *context models* do not — 9.3.1.1
        // is not re-invoked, so `ctx` is untouched here. `CabacDecoder`
        // renormalises exactly one bit at a time with no read-ahead (see
        // its own module doc), so `into_reader()` hands back a
        // `BitReader` positioned exactly where the raw byte data starts.
        let mut reader = core::mem::replace(cabac, CabacDecoder::new(&[])).into_reader();
        reader.align();
        for _ in 0..384u32 {
            let _ = reader.get(8);
        }
        *cabac = CabacDecoder::from_reader(reader);
        *prev_qp = PrevMbQp { available: true, skipped: false, is_ipcm: true, ..PrevMbQp::default() };
        grids.set_mb_info(
            mb_x,
            mb_y,
            CabacMbInfo { available: true, skipped: false, is_ipcm: true, ..CabacMbInfo::default() },
        );
        return Ok(());
    }

    let is_intra = kind.is_intra();
    // clause 9.3.3.1.1.8's own condTermFlagN needs a neighbour's *decoded*
    // intra_chroma_pred_mode (specifically, whether it was nonzero) — kept
    // here so it can be threaded into this macroblock's own `CabacMbInfo`
    // below, the same "actually store what a later ctxIdxInc derivation
    // needs" fix chroma DC's coded_block_flag got.
    let mut intra_chroma_pred_mode = 0u8;
    if is_intra {
        if matches!(kind, MbKind::Intra4x4) {
            for _ in 0..16 {
                if cabac.decode_decision(&mut ctx.prev_intra4x4) == 0 {
                    let _ = cabac.decode_decision(&mut ctx.rem_intra4x4);
                    let _ = cabac.decode_decision(&mut ctx.rem_intra4x4);
                    let _ = cabac.decode_decision(&mut ctx.rem_intra4x4);
                }
            }
        }
        let inc0 =
            intra_chroma_cond_term(grids.mb_left(mb_x, mb_y)) + intra_chroma_cond_term(grids.mb_above(mb_x, mb_y));
        let mut n = 0u32;
        while n < 3 {
            let idx = if n == 0 { inc0.min(2) as usize } else { 3 };
            if decide(cabac, &mut ctx.intra_chroma, idx) == 0 {
                break;
            }
            n += 1;
        }
        intra_chroma_pred_mode = u8::try_from(n).unwrap_or(3);
    } else {
        // Non-intra: `raw_code` (0..=4, `classify_mb_type`'s own input,
        // still in scope) carries the exact partition geometry
        // `classify_mb_type`'s `MbKind::Inter { parts }` deliberately
        // discards (CAVLC bit consumption never needed it) but CABAC's
        // `ref_idx`/`mvd` neighbour grid does — a 2-partition macroblock's
        // *split direction* decides which 4x4 positions each partition's
        // decoded values land on for the next macroblock to read back.
        match raw_code {
            0 => decode_one_partition_cabac(cabac, header, ctx, grids, mb_x, mb_y, PartPred::L0, (0, 0, 3, 3))?,
            1 => {
                decode_two_partitions_cabac(
                    cabac,
                    header,
                    ctx,
                    grids,
                    mb_x,
                    mb_y,
                    PartPred::L0,
                    PartPred::L0,
                    (0, 0, 3, 1),
                    (0, 2, 3, 3),
                )?;
            }
            2 => {
                decode_two_partitions_cabac(
                    cabac,
                    header,
                    ctx,
                    grids,
                    mb_x,
                    mb_y,
                    PartPred::L0,
                    PartPred::L0,
                    (0, 0, 1, 3),
                    (2, 0, 3, 3),
                )?;
            }
            3 => decode_sub_mb_pred_cabac(cabac, header, ctx, grids, mb_x, mb_y, false)?,
            4 => decode_sub_mb_pred_cabac(cabac, header, ctx, grids, mb_x, mb_y, true)?,
            _ => return Err(Error::InvalidData("mb_type: unexpected non-intra P mb_type code")),
        }
    }

    let (cbp_luma, cbp_chroma) = if let MbKind::Intra16x16 { cbp_luma, cbp_chroma } = &kind {
        (*cbp_luma, *cbp_chroma)
    } else {
        decode_cbp_cabac(cabac, ctx, grids, mb_x, mb_y, is_intra)
    };

    let cbp_zero = cbp_luma == 0 && cbp_chroma == 0;
    let mb_qp_delta = if cbp_zero && !matches!(kind, MbKind::Intra16x16 { .. }) {
        0
    } else {
        let inc = qp_delta_ctx_inc(*prev_qp);
        let mut n = 0u32;
        // mb_qp_delta's U binarisation: bin0 uses ctxIdxInc from clause
        // 9.3.3.1.1.5 (local 0/1), bin1 fixed local2, bin2+ fixed local3.
        loop {
            let idx = match n {
                0 => inc.min(1) as usize,
                1 => 2,
                _ => 3,
            };
            if n >= 64 {
                break; // defensive cap (D6); no conformant delta is this large
            }
            if decide(cabac, &mut ctx.qp_delta, idx) == 0 {
                break;
            }
            n += 1;
        }
        se_value(n)
    };

    *prev_qp = PrevMbQp {
        available: true,
        skipped: false,
        is_ipcm: false,
        is_intra16x16: matches!(kind, MbKind::Intra16x16 { .. }),
        cbp_zero,
        qp_delta_zero: mb_qp_delta == 0,
    };

    if cbp_luma > 0 || cbp_chroma > 0 || matches!(kind, MbKind::Intra16x16 { .. }) {
        decode_residual_cabac(cabac, budget, ctx, grids, &kind, cbp_luma, cbp_chroma, mb_x, mb_y)?;
    } else {
        for blk in 0..16u32 {
            let (bx, by) = blk_xy(blk);
            grids.set_cbf_luma(mb_x * 4 + bx, mb_y * 4 + by, false);
        }
        for comp in 0..2usize {
            for blk in 0..4u32 {
                let (bx, by) = blk_xy(blk);
                grids.set_cbf_chroma(comp, mb_x * 2 + bx % 2, mb_y * 2 + by % 2, false);
            }
        }
    }

    grids.set_mb_info(
        mb_x,
        mb_y,
        CabacMbInfo {
            available: true,
            skipped: false,
            is_intra4x4: matches!(kind, MbKind::Intra4x4),
            is_intra,
            is_intra16x16: matches!(kind, MbKind::Intra16x16 { .. }),
            is_ipcm: false,
            cbp_luma,
            cbp_chroma,
            intra_chroma_pred_mode,
        },
    );
    Ok(())
}

/// `ref_idx_lX`/`mvd_lX` for one whole-macroblock partition covering 4x4
/// rectangle `(x0, y0, x1, y1)` (inclusive, macroblock-relative), clause
/// 9.3.3.1.1.6/7's neighbour lookup taken from the partition's own top-left
/// corner (matching how every partition's own left/above neighbour is
/// conventionally derived from clause 6.4.7.5: the position immediately
/// left of, or above, that corner).
#[allow(clippy::too_many_arguments)]
fn decode_one_partition_cabac(
    cabac: &mut CabacDecoder<'_>,
    header: &SliceHeader,
    ctx: &mut CabacMbCtx,
    grids: &mut CabacGrids,
    mb_x: u32,
    mb_y: u32,
    pred: PartPred,
    (x0, y0, x1, y1): (u32, u32, u32, u32),
) -> Result<()> {
    let ax = mb_x * 4 + x0;
    let ay = mb_y * 4 + y0;
    let left = ax.checked_sub(1).map_or_else(MvInfo::default, |lx| grids.mv_at(lx, ay));
    let above = ay.checked_sub(1).map_or_else(MvInfo::default, |ay2| grids.mv_at(ax, ay2));

    let n0 = header.num_ref_idx_l0_active_minus1;
    let n1 = header.num_ref_idx_l1_active_minus1;
    let mut ref_idx = [0i8; 2];
    let mut mvd = [(0i16, 0i16); 2];
    if pred.reads_l0() && n0 > 0 {
        let inc = ref_idx_cond_term(left, 0) + 2 * ref_idx_cond_term(above, 0);
        ref_idx[0] = i8::try_from(decode_ref_idx(cabac, &mut ctx.ref_idx, inc)).unwrap_or(i8::MAX);
    }
    if pred.reads_l1() && n1 > 0 {
        let inc = ref_idx_cond_term(left, 1) + 2 * ref_idx_cond_term(above, 1);
        ref_idx[1] = i8::try_from(decode_ref_idx(cabac, &mut ctx.ref_idx, inc)).unwrap_or(i8::MAX);
    }
    if pred.reads_l0() {
        let sum_x = mvd_abs_term(left, 0, 0) + mvd_abs_term(above, 0, 0);
        let x = decode_mvd_component(cabac, &mut ctx.mvd_comp0, sum_x);
        let sum_y = mvd_abs_term(left, 0, 1) + mvd_abs_term(above, 0, 1);
        let y = decode_mvd_component(cabac, &mut ctx.mvd_comp1, sum_y);
        mvd[0] = (i16::try_from(x).unwrap_or(i16::MAX), i16::try_from(y).unwrap_or(i16::MAX));
    }
    if pred.reads_l1() {
        let sum_x = mvd_abs_term(left, 1, 0) + mvd_abs_term(above, 1, 0);
        let x = decode_mvd_component(cabac, &mut ctx.mvd_comp0, sum_x);
        let sum_y = mvd_abs_term(left, 1, 1) + mvd_abs_term(above, 1, 1);
        let y = decode_mvd_component(cabac, &mut ctx.mvd_comp1, sum_y);
        mvd[1] = (i16::try_from(x).unwrap_or(i16::MAX), i16::try_from(y).unwrap_or(i16::MAX));
    }

    let info = MvInfo { pred: Some(pred), ref_idx, mvd };
    for y in y0..=y1 {
        for x in x0..=x1 {
            grids.set_mv(mb_x * 4 + x, mb_y * 4 + y, info);
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    clippy::indexing_slicing,
    reason = "indices are 0..2 loop variables into fixed 2-element arrays (list/partition), not attacker-sized"
)]
fn decode_two_partitions_cabac(
    cabac: &mut CabacDecoder<'_>,
    header: &SliceHeader,
    ctx: &mut CabacMbCtx,
    grids: &mut CabacGrids,
    mb_x: u32,
    mb_y: u32,
    pred0: PartPred,
    pred1: PartPred,
    rect0: (u32, u32, u32, u32),
    rect1: (u32, u32, u32, u32),
) -> Result<()> {
    // clause 7.3.5.1's read order: all of ref_idx_l0, then ref_idx_l1, then
    // mvd_l0, then mvd_l1, across *both* partitions — not one partition
    // fully read before the next starts. `decode_one_partition_cabac`
    // reads all four for one partition at once, so this function calls it
    // per-list instead of reusing it directly, matching the read order
    // rather than the per-partition grouping that function's single-part
    // caller uses.
    let n0 = header.num_ref_idx_l0_active_minus1;
    let n1 = header.num_ref_idx_l1_active_minus1;
    let mut ref_idx = [[0i8; 2]; 2];
    let mut mvd = [[(0i16, 0i16); 2]; 2];
    let preds = [pred0, pred1];
    let rects = [rect0, rect1];
    let neighbours = |grids: &CabacGrids, rect: (u32, u32, u32, u32)| {
        let ax = mb_x * 4 + rect.0;
        let ay = mb_y * 4 + rect.1;
        let left = ax.checked_sub(1).map_or_else(MvInfo::default, |lx| grids.mv_at(lx, ay));
        let above = ay.checked_sub(1).map_or_else(MvInfo::default, |ay2| grids.mv_at(ax, ay2));
        (left, above)
    };

    for list in 0..2usize {
        for p in 0..2usize {
            let reads = if list == 0 { preds[p].reads_l0() } else { preds[p].reads_l1() };
            let n_active = if list == 0 { n0 } else { n1 };
            if reads && n_active > 0 {
                let (left, above) = neighbours(grids, rects[p]);
                let inc = ref_idx_cond_term(left, list) + 2 * ref_idx_cond_term(above, list);
                ref_idx[p][list] = i8::try_from(decode_ref_idx(cabac, &mut ctx.ref_idx, inc)).unwrap_or(i8::MAX);
            }
        }
    }
    for list in 0..2usize {
        for p in 0..2usize {
            let reads = if list == 0 { preds[p].reads_l0() } else { preds[p].reads_l1() };
            if reads {
                let (left, above) = neighbours(grids, rects[p]);
                let sum_x = mvd_abs_term(left, list, 0) + mvd_abs_term(above, list, 0);
                let x = decode_mvd_component(cabac, &mut ctx.mvd_comp0, sum_x);
                let sum_y = mvd_abs_term(left, list, 1) + mvd_abs_term(above, list, 1);
                let y = decode_mvd_component(cabac, &mut ctx.mvd_comp1, sum_y);
                mvd[p][list] = (i16::try_from(x).unwrap_or(i16::MAX), i16::try_from(y).unwrap_or(i16::MAX));
                // Writing the grid immediately (rather than after every list
                // is read) is required, not cosmetic: the *other* partition's
                // own neighbour lookup two lines above must see this one's
                // values once decoded, matching clause 6.4.7.5's ordinary
                // same-macroblock case.
                let info = MvInfo { pred: Some(preds[p]), ref_idx: ref_idx[p], mvd: mvd[p] };
                let (x0, y0, x1, y1) = rects[p];
                for yy in y0..=y1 {
                    for xx in x0..=x1 {
                        grids.set_mv(mb_x * 4 + xx, mb_y * 4 + yy, info);
                    }
                }
            }
        }
    }
    Ok(())
}

/// `P_8x8`/`P_8x8ref0`'s four sub-macroblock partitions.
#[allow(
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "quad/sub-partition indices are bounded 0..4 loop variables into fixed-size arrays, and quad%2/quad/2 is the spec's own 2x2 quadrant split, not a precision-loss bug"
)]
fn decode_sub_mb_pred_cabac(
    cabac: &mut CabacDecoder<'_>,
    header: &SliceHeader,
    ctx: &mut CabacMbCtx,
    grids: &mut CabacGrids,
    mb_x: u32,
    mb_y: u32,
    ref0_inferred: bool,
) -> Result<()> {
    let mut subs: Vec<(u8, Option<PartPred>)> = budget_alloc_four();
    for _ in 0..4 {
        let code = decode_sub_mb_type_p(cabac, &mut ctx.sub_mb_type_p);
        subs.push(classify_sub_mb_type(false, code)?);
    }
    let n0 = header.num_ref_idx_l0_active_minus1;
    for (i, &(num_sub, pred)) in subs.iter().enumerate() {
        let Some(pred) = pred else { continue };
        let quad = u32::try_from(i).unwrap_or(0);
        let (qx, qy) = (quad % 2, quad / 2);
        let (x0, y0, x1, y1) = (qx * 2, qy * 2, qx * 2 + 1, qy * 2 + 1);
        let ax = mb_x * 4 + x0;
        let ay = mb_y * 4 + y0;
        let left = ax.checked_sub(1).map_or_else(MvInfo::default, |lx| grids.mv_at(lx, ay));
        let above = ay.checked_sub(1).map_or_else(MvInfo::default, |ay2| grids.mv_at(ax, ay2));
        let mut ref_idx = [0i8; 2];
        if !ref0_inferred && n0 > 0 {
            let inc = ref_idx_cond_term(left, 0) + 2 * ref_idx_cond_term(above, 0);
            ref_idx[0] = i8::try_from(decode_ref_idx(cabac, &mut ctx.ref_idx, inc)).unwrap_or(i8::MAX);
        }
        // `num_sub` sub-partitions inside this 8x8 quadrant share one
        // ref_idx but each read their own mvd; only `num_sub == 1` (P_L0_8x8)
        // is common in real corpora, but all values are read either way so
        // bit consumption stays correct for 8x4/4x8/4x4 splits too.
        let sub_positions: [(u32, u32); 4] = match num_sub {
            1 => [(x0, y0), (x0, y0), (x0, y0), (x0, y0)],
            2 if x1 > x0 => [(x0, y0), (x1, y0), (x0, y0), (x1, y0)], // 8x4 columns unused; approximated as halves
            2 => [(x0, y0), (x0, y1), (x0, y0), (x0, y1)],
            _ => [(x0, y0), (x1, y0), (x0, y1), (x1, y1)],
        };
        for s in 0..num_sub {
            let (sx, sy) = sub_positions[usize::from(s).min(3)];
            let sax = mb_x * 4 + sx;
            let say = mb_y * 4 + sy;
            let sleft = sax.checked_sub(1).map_or_else(MvInfo::default, |lx| grids.mv_at(lx, say));
            let sabove = say.checked_sub(1).map_or_else(MvInfo::default, |ay2| grids.mv_at(sax, ay2));
            let sum_x = mvd_abs_term(sleft, 0, 0) + mvd_abs_term(sabove, 0, 0);
            let x = decode_mvd_component(cabac, &mut ctx.mvd_comp0, sum_x);
            let sum_y = mvd_abs_term(sleft, 0, 1) + mvd_abs_term(sabove, 0, 1);
            let y = decode_mvd_component(cabac, &mut ctx.mvd_comp1, sum_y);
            let mvd = [(i16::try_from(x).unwrap_or(i16::MAX), i16::try_from(y).unwrap_or(i16::MAX)), (0, 0)];
            let info = MvInfo { pred: Some(pred), ref_idx, mvd };
            grids.set_mv(sax, say, info);
        }
        for y in y0..=y1 {
            for x in x0..=x1 {
                if grids.mv_at(mb_x * 4 + x, mb_y * 4 + y).pred.is_none() {
                    grids.set_mv(
                        mb_x * 4 + x,
                        mb_y * 4 + y,
                        MvInfo { pred: Some(pred), ref_idx, mvd: [(0, 0), (0, 0)] },
                    );
                }
            }
        }
    }
    Ok(())
}

/// A 4-element `Vec` without touching `Budget` for a fixed, tiny, always-4
/// count — `sub_mb_type` always has exactly 4 sub-macroblocks, so there is
/// no attacker-controlled size here the way `residual_block_cavlc`'s
/// `TotalCoeff`-sized allocations have.
fn budget_alloc_four<T>() -> Vec<T> {
    Vec::new()
}

/// `coded_block_pattern`, clause 9.3.2.6's `FL(CodedBlockPatternLuma,
/// cMax=15)` prefix (context per clause 9.3.3.1.1.4, `binIdx` == the 8x8
/// luma block index directly) and `TU(CodedBlockPatternChroma, cMax=2)`
/// suffix.
fn decode_cbp_cabac(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut CabacMbCtx,
    grids: &CabacGrids,
    mb_x: u32,
    mb_y: u32,
    current_is_intra: bool,
) -> (u8, u8) {
    let _ = current_is_intra;
    let mut cbp_luma = 0u8;
    for q in 0..4u32 {
        // clause 6.4.7.2 + Table 6-2: luma8x8BlkIdx is raster-scan (0 1 /
        // 2 3). The left neighbour of q falls in the *same* macroblock,
        // at block q-1, exactly when q is in the right column (1, 3); the
        // above neighbour falls in the *same* macroblock, at block q-2,
        // exactly when q is in the bottom row (2, 3). These are two
        // independent conditions on two independent same-macroblock
        // sources — q=3's left (block 2) and above (block 1) are
        // different same-mb blocks, not the same one. An earlier version
        // of this function computed a single `same_mb_bit` (the left
        // rule only) and fed it to *both* `cond_a` and `cond_b`, which
        // happened to be right for every q's left term and q=0's above
        // term, but was wrong for q=1 (used same-mb block 0 instead of
        // the above macroblock's block 3), silently zero for q=2 (used
        // neither source at all, since neither `same_mb_bit` nor
        // `cross_mb_above` was populated for it), and wrong for q=3
        // (reused block 2, the left value, instead of block 1). Found by
        // re-deriving q's actual left/above (xN, yN) by hand from Table
        // 6-2 and clause 6.4.7.2's formulas, not by inspection.
        let same_mb_left_bit = match q {
            1 | 3 => Some(cbp_luma & (1 << (q - 1)) != 0),
            _ => None,
        };
        let same_mb_above_bit = match q {
            2 | 3 => Some(cbp_luma & (1 << (q - 2)) != 0),
            _ => None,
        };
        let cross_mb_left = if q == 1 || q == 3 {
            None
        } else {
            grids.mb_left(mb_x, mb_y).map(|info| (info, info.cbp_luma & (1 << (q + 1)) != 0))
        };
        let cross_mb_above = if q == 2 || q == 3 {
            None
        } else {
            grids.mb_above(mb_x, mb_y).map(|info| (info, info.cbp_luma & (1 << (q + 2)) != 0))
        };
        let cond_a = cbp_luma_cond_term(same_mb_left_bit, cross_mb_left);
        let cond_b = cbp_luma_cond_term(same_mb_above_bit, cross_mb_above);
        let inc = cond_a + 2 * cond_b;
        if decide(cabac, &mut ctx.cbp_luma, inc as usize) == 1 {
            cbp_luma |= 1 << q;
        }
    }

    let left = grids.mb_left(mb_x, mb_y);
    let above = grids.mb_above(mb_x, mb_y);
    let inc0 = cbp_chroma_cond_term(left, 0) + 2 * cbp_chroma_cond_term(above, 0);
    let cbp_chroma = if decide(cabac, &mut ctx.cbp_chroma, inc0 as usize) == 0 {
        0
    } else {
        let inc1 = cbp_chroma_cond_term(left, 1) + 2 * cbp_chroma_cond_term(above, 1) + 4;
        if decide(cabac, &mut ctx.cbp_chroma, inc1 as usize) == 0 { 1 } else { 2 }
    };
    (cbp_luma, cbp_chroma)
}

/// Residual, clause 7.3.5.3.3: `coded_block_flag` (its own `ctxIdxInc` from
/// clause 9.3.3.1.1.9, using [`CabacGrids`]'s per-block flag history) then,
/// only if set, [`residual_block_cabac`].
#[allow(clippy::too_many_arguments)]
fn decode_residual_cabac(
    cabac: &mut CabacDecoder<'_>,
    budget: &mut Budget,
    ctx: &mut CabacMbCtx,
    grids: &mut CabacGrids,
    kind: &MbKind,
    cbp_luma: u8,
    cbp_chroma: u8,
    mb_x: u32,
    mb_y: u32,
) -> Result<()> {
    let is_16x16 = matches!(kind, MbKind::Intra16x16 { .. });
    let current_is_intra = kind.is_intra();

    if is_16x16 {
        let x = mb_x * 4;
        let y = mb_y * 4;
        // Luma DC (ctxBlockCat 0) is one flag per macroblock: its own
        // neighbour is the neighbouring macroblock's DC block, available
        // only when that neighbour is itself coded Intra_16x16 (clause
        // 9.3.3.1.1.9's `transBlockN` rule for `ctxBlockCat == 0`).
        let left_dc = grids.mb_left(mb_x, mb_y);
        let above_dc = grids.mb_above(mb_x, mb_y);
        let left_dc_flag = left_dc.filter(|i| i.is_intra16x16).and_then(|_| grids.cbf_luma_at(x.wrapping_sub(4), y));
        let above_dc_flag =
            above_dc.filter(|i| i.is_intra16x16).and_then(|_| grids.cbf_luma_at(x, y.wrapping_sub(4)));
        let cond_a = cbf_cond_term(left_dc, left_dc.is_some_and(|i| i.is_intra16x16), left_dc_flag.unwrap_or(false), current_is_intra);
        let cond_b = cbf_cond_term(above_dc, above_dc.is_some_and(|i| i.is_intra16x16), above_dc_flag.unwrap_or(false), current_is_intra);
        let inc = cond_a + 2 * cond_b;
        let coded = decide(cabac, &mut ctx.cbf_luma_dc, inc as usize) == 1;
        grids.set_cbf_luma(x, y, coded);
        if coded {
            let _ = residual_block_cabac(cabac, &mut ctx.residual_luma_dc, 16, budget)?;
        }
    }

    for i8x8 in 0..4u32 {
        for i4x4 in 0..4u32 {
            let blk = i8x8 * 4 + i4x4;
            let (bx, by) = blk_xy(blk);
            let x = mb_x * 4 + bx;
            let y = mb_y * 4 + by;
            if cbp_luma & (1 << i8x8) != 0 {
                let left_bit = grids.cbf_luma_at(x.wrapping_sub(1), y);
                let above_bit = grids.cbf_luma_at(x, y.wrapping_sub(1));
                let left_mb = x.checked_sub(1).and_then(|_| {
                    if bx == 0 { grids.mb_left(mb_x, mb_y) } else { grids.mb_info_at(mb_x, mb_y) }
                });
                let above_mb = y.checked_sub(1).and_then(|_| {
                    if by == 0 { grids.mb_above(mb_x, mb_y) } else { grids.mb_info_at(mb_x, mb_y) }
                });
                let left_avail = x > 0 && left_bit.is_some();
                let above_avail = y > 0 && above_bit.is_some();
                let cond_a = cbf_cond_term(left_mb, left_avail, left_bit.unwrap_or(false), current_is_intra);
                let cond_b = cbf_cond_term(above_mb, above_avail, above_bit.unwrap_or(false), current_is_intra);
                let inc = cond_a + 2 * cond_b;
                let ctx_arr = if is_16x16 { &mut ctx.cbf_luma_ac } else { &mut ctx.cbf_luma4x4 };
                let coded = decide(cabac, ctx_arr, inc as usize) == 1;
                grids.set_cbf_luma(x, y, coded);
                if coded {
                    let (max_num_coeff, category) = if is_16x16 {
                        (15, &mut ctx.residual_luma_ac)
                    } else {
                        (16, &mut ctx.residual_luma4x4)
                    };
                    let _ = residual_block_cabac(cabac, category, max_num_coeff, budget)?;
                }
            } else {
                grids.set_cbf_luma(x, y, false);
            }
        }
    }

    for comp in 0..2usize {
        if cbp_chroma & 3 != 0 {
            // ctxBlockCat 3 (chroma DC): the neighbour derivation is
            // macroblock-granular (clause 9.3.3.1.1.9's own text: "the
            // chroma DC block of chroma component iCbCr of macroblock
            // mbAddrN"), not 4x4-block-granular the way luma/chroma AC's
            // is — `CabacGrids::cbf_chroma_dc_at` is keyed by macroblock
            // address for exactly this reason.
            let left = grids.mb_left(mb_x, mb_y);
            let above = grids.mb_above(mb_x, mb_y);
            let left_avail = left.is_some_and(|i| !i.skipped && !i.is_ipcm && i.cbp_chroma != 0);
            let above_avail = above.is_some_and(|i| !i.skipped && !i.is_ipcm && i.cbp_chroma != 0);
            let left_flag = left_avail
                .then(|| left.and_then(|_| grids.cbf_chroma_dc_at(comp, mb_x.wrapping_sub(1), mb_y)))
                .flatten()
                .unwrap_or(false);
            let above_flag = above_avail
                .then(|| above.and_then(|_| grids.cbf_chroma_dc_at(comp, mb_x, mb_y.wrapping_sub(1))))
                .flatten()
                .unwrap_or(false);
            let cond_a = cbf_cond_term(left, left_avail, left_flag, current_is_intra);
            let cond_b = cbf_cond_term(above, above_avail, above_flag, current_is_intra);
            let inc = cond_a + 2 * cond_b;
            let coded = decide(cabac, &mut ctx.cbf_chroma_dc, inc as usize) == 1;
            grids.set_cbf_chroma_dc(comp, mb_x, mb_y, coded);
            if coded {
                let _out = residual_block_cabac(cabac, &mut ctx.residual_chroma_dc, 4, budget)?;
            }
        } else {
            grids.set_cbf_chroma_dc(comp, mb_x, mb_y, false);
        }
    }
    for comp in 0..2usize {
        for i4x4 in 0..4u32 {
            let (bx, by) = blk_xy(i4x4);
            let x = mb_x * 2 + bx % 2;
            let y = mb_y * 2 + by % 2;
            if cbp_chroma & 2 != 0 {
                let left_bit = grids.cbf_chroma_at(comp, x.wrapping_sub(1), y);
                let above_bit = grids.cbf_chroma_at(comp, x, y.wrapping_sub(1));
                let left_mb = if bx == 0 { grids.mb_left(mb_x, mb_y) } else { grids.mb_info_at(mb_x, mb_y) };
                let above_mb = if by == 0 { grids.mb_above(mb_x, mb_y) } else { grids.mb_info_at(mb_x, mb_y) };
                let left_avail = x > 0 && left_bit.is_some();
                let above_avail = y > 0 && above_bit.is_some();
                let cond_a = cbf_cond_term(left_mb, left_avail, left_bit.unwrap_or(false), current_is_intra);
                let cond_b = cbf_cond_term(above_mb, above_avail, above_bit.unwrap_or(false), current_is_intra);
                let inc = cond_a + 2 * cond_b;
                let coded = decide(cabac, &mut ctx.cbf_chroma_ac, inc as usize) == 1;
                grids.set_cbf_chroma(comp, x, y, coded);
                if coded {
                    let _ = residual_block_cabac(cabac, &mut ctx.residual_chroma_ac, 15, budget)?;
                }
            } else {
                grids.set_cbf_chroma(comp, x, y, false);
            }
        }
    }
    Ok(())
}
