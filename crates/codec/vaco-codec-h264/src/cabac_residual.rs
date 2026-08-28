//! CABAC residual-block decoding, ITU-T H.264 clause 9.3 (binarisation:
//! 9.3.2; the decoding process and its context-index derivation: 9.3.3),
//! built over `vaco-codec-cabac`'s engine.
//!
//! This is CABAC's counterpart to [`crate::cavlc::residual_block_cavlc`]:
//! the same entropy-layer/macroblock-layer split, drawn in the same place.
//! `vaco-codec-cabac` itself says why `ctxIdxInc` derivation belongs to the
//! codec crate rather than the engine (its own module doc, "What is
//! deliberately not here") — this module is that codec-side half, scoped to
//! exactly the syntax elements that do not need a neighbouring macroblock to
//! derive: `coded_block_flag`'s condition term is taken as a caller-supplied
//! `bool` (the same separation [`crate::cavlc`] draws around `nC`), and
//! `significant_coeff_flag`/`last_significant_coeff_flag`/
//! `coeff_abs_level_minus1` all derive their own context purely from scan
//! position or from counters local to the current block.
//!
//! # What is deliberately not here, and why this dispatch stops at the line
//! it does
//!
//! `mb_type`, `mb_skip_flag`, `coded_block_pattern`, `mvd`, `ref_idx` and
//! every other macroblock-layer syntax element's context tables are not
//! transcribed here. Two independent reasons, not one: first, their
//! `ctxIdxInc` genuinely needs `condTermFlagA`/`condTermFlagB` from
//! neighbouring macroblocks — state #419 (the macroblock layer) produces,
//! not this crate; second, their initialisation tables belong with the
//! neighbour-derivation logic that will consume them, so splitting the two
//! across separate, independently-landed commits (this one and #419's) risks
//! exactly the kind of drift a table and its only caller landing together
//! avoids. `coded_block_flag` itself is the same shape — its own
//! `ctxIdxInc` needs the *above* and *left* block's own `coded_block_flag`,
//! which is why it is a parameter here, not a derivation.
//!
//! # An honest confidence note on the context tables below
//!
//! Transcribed from the published ITU-T H.264 text in a network-isolated
//! clean-room environment (D7), with no second copy to diff against — the
//! same caveat `cavlc_tables.rs` states at more length applies here, and if
//! anything more strongly: these `(m, n)` pairs are a less redundant, less
//! externally-checkable shape than a VLC code's bit length, and the
//! generalist context-selection *formulas* below (position-indexed
//! significance context, the `numDecodAbsLevelEq1`/`Gt1` counter rule for
//! `coeff_abs_level_minus1`) are held with materially higher confidence than
//! the specific `(m, n)` integers populating [`ContextSet::new`]. Treat the
//! latter as provisional pending an independent check, exactly as this
//! project already treats Teletext's Table 36 national subsets.
//!
//! # Scope within "4x4-shaped" categories
//!
//! `ctxBlockCat` 0 (luma DC), 1 (luma AC), 2 (luma 4x4), 4 (chroma AC) all
//! use the identity mapping ITU-T Table 9-43's base rows describe:
//! `ctxIdxInc(significant_coeff_flag) == levelListIdx`, and likewise for
//! `last_significant_coeff_flag`, both capped by the category's own
//! `maxNumCoeff - 1`. Chroma DC (`ctxBlockCat` 3, `maxNumCoeff` 4 or 8) and
//! 8x8 transform blocks (`ctxBlockCat` 5, High-profile-only) use different,
//! non-identity tables this module does not implement — [`ContextCategory`]
//! only names the four base categories, and 8x8/chroma-DC residual decode is
//! left for whoever lands transform-size selection in #419's High-profile
//! path.

use vaco_codec_cabac::{CabacDecoder, ContextInit, ContextModel, init_contexts};
use vaco_core::Result;
use vaco_limits::Budget;

/// The four residual categories this module covers — ITU-T's `ctxBlockCat`
/// 0, 1, 2 and 4. See the module doc for why 3 (chroma DC) and 5 (8x8) are
/// out of scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextCategory {
    /// `ctxBlockCat` 0: `Intra16x16DCLevel`, `maxNumCoeff == 16`.
    LumaDc,
    /// `ctxBlockCat` 1: `Intra16x16ACLevel`, `maxNumCoeff == 15`.
    LumaAc,
    /// `ctxBlockCat` 2: `LumaLevel4x4`, `maxNumCoeff == 16`.
    Luma4x4,
    /// `ctxBlockCat` 4: `ChromaACLevel`, `maxNumCoeff == 15`.
    ChromaAc,
}

/// One block's context set: significance-map contexts, sized for the
/// largest category (16 positions, only 15 of which are ever asked for —
/// the last position needs no flag), plus the two `coeff_abs_level_minus1`
/// context groups (clause 9.3.3.1.3: 5 contexts selecting on
/// `Min(4, 1 + numDecodAbsLevelEq1)` (or 0, if `numDecodAbsLevelGt1 != 0`)
/// for `binIdx == 0`, 5 more selecting on `Min(4, numDecodAbsLevelGt1)` for
/// `binIdx >= 1`).
///
/// One [`ContextSet`] is shared across every block of a given category
/// within a slice — CABAC contexts adapt across the whole slice, not per
/// block, same as every other syntax element.
#[derive(Debug, Clone)]
pub struct ContextSet {
    significant_coeff_flag: [ContextModel; 15],
    last_significant_coeff_flag: [ContextModel; 15],
    coeff_abs_level_minus1_bin0: [ContextModel; 5],
    coeff_abs_level_minus1_binn: [ContextModel; 5],
    /// A fallback slot for `slice_mut`'s out-of-range case, which cannot
    /// occur for any category this module supports today (every index is a
    /// scan position bounded by `max_num_coeff <= 16` against a 15-entry
    /// array, or a `Min(4, ...)` count against a 5-entry one) but which
    /// `clippy::indexing_slicing` requires a real fallback for regardless of
    /// that invariant, rather than a panicking index operation.
    scratch: ContextModel,
}

impl ContextSet {
    /// Build and initialise from `slice_qp`, clause 9.3.1.1. `(m, n)`
    /// literals below: see the module doc's confidence note.
    #[must_use]
    pub fn new(slice_qp: i8) -> Self {
        #[rustfmt::skip]
        const SIG_INIT: [(i16, i16); 15] = [
            (24, 0), (24, -11), (23, -8), (23, -6), (23, -3), (23, -1),
            (0, 26), (18, -13), (23, -10), (24, -12), (26, -19), (30, -25),
            (33, -30), (37, -37), (35, -32),
        ];
        #[rustfmt::skip]
        const LAST_INIT: [(i16, i16); 15] = [
            (14, 3), (13, 11), (10, 21), (12, 20), (12, 20), (12, 19),
            (17, 8), (17, 11), (18, 8), (20, 6), (21, 5), (22, 3),
            (23, 2), (24, 0), (24, -1),
        ];
        #[rustfmt::skip]
        const ABS_BIN0_INIT: [(i16, i16); 5] = [
            (14, 30), (16, 16), (14, 8), (10, 6), (7, 6),
        ];
        #[rustfmt::skip]
        const ABS_BINN_INIT: [(i16, i16); 5] = [
            (14, 6), (9, 14), (7, 14), (5, 14), (3, 14),
        ];

        let build = |pairs: &[(i16, i16)], dst: &mut [ContextModel]| {
            let inits: Vec<ContextInit> = pairs.iter().map(|&(m, n)| ContextInit::new(m, n)).collect();
            init_contexts(dst, &inits, slice_qp);
        };
        let mut s = Self {
            significant_coeff_flag: [ContextModel::UNINITIALISED; 15],
            last_significant_coeff_flag: [ContextModel::UNINITIALISED; 15],
            coeff_abs_level_minus1_bin0: [ContextModel::UNINITIALISED; 5],
            coeff_abs_level_minus1_binn: [ContextModel::UNINITIALISED; 5],
            scratch: ContextModel::UNINITIALISED,
        };
        build(&SIG_INIT, &mut s.significant_coeff_flag);
        build(&LAST_INIT, &mut s.last_significant_coeff_flag);
        build(&ABS_BIN0_INIT, &mut s.coeff_abs_level_minus1_bin0);
        build(&ABS_BINN_INIT, &mut s.coeff_abs_level_minus1_binn);
        s
    }
}

impl ContextSet {
    fn sig_mut(&mut self, i: u8) -> &mut ContextModel {
        let scratch = &mut self.scratch;
        self.significant_coeff_flag.get_mut(usize::from(i)).unwrap_or(scratch)
    }

    fn last_sig_mut(&mut self, i: u8) -> &mut ContextModel {
        let scratch = &mut self.scratch;
        self.last_significant_coeff_flag.get_mut(usize::from(i)).unwrap_or(scratch)
    }

    fn abs_level_bin0_mut(&mut self, idx: u32) -> &mut ContextModel {
        let scratch = &mut self.scratch;
        self.coeff_abs_level_minus1_bin0
            .get_mut(idx as usize)
            .unwrap_or(scratch)
    }

    fn abs_level_binn_mut(&mut self, idx: u32) -> &mut ContextModel {
        let scratch = &mut self.scratch;
        self.coeff_abs_level_minus1_binn
            .get_mut(idx as usize)
            .unwrap_or(scratch)
    }
}

/// A decoded residual block, in forward scan order this time (unlike
/// [`crate::cavlc::CavlcResidual`]'s reverse order) — CABAC decodes the
/// significance map forward, so there is no reversal to mirror.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CabacResidual {
    /// `coeff_level[i]` for every `i` the significance map marked nonzero,
    /// forward scan order, sign applied. Zero coefficients are implicit —
    /// the caller reconstructs the full `maxNumCoeff`-length array from
    /// [`Self::positions`] and this.
    pub levels: Vec<i32>,
    /// The scan position of each entry in [`Self::levels`], same order,
    /// strictly increasing.
    pub positions: Vec<u8>,
}

/// `residual_block_cabac()`'s coefficient half, clause 7.3.5.3.3 combined
/// with 9.3.3.1.3's binarisations — everything after `coded_block_flag`,
/// which the caller has already decoded (its own `ctxIdxInc` needs neighbour
/// state this module does not have).
///
/// # Errors
///
/// [`vaco_core::Error::LimitExceeded`] if `budget` is exhausted. This function cannot
/// observe a CABAC engine desync as an error — `vaco-codec-cabac` never
/// fails a read, by design (clause 9.3.3.2's engine cannot represent "ran
/// out of bits" any differently from "read a zero"); [`CabacDecoder`]'s own
/// `malformed()` is what a caller checks once, after the whole slice, per
/// its own documented convention.
pub fn residual_block_cabac(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextSet,
    category: ContextCategory,
    max_num_coeff: u8,
    budget: &mut Budget,
) -> Result<CabacResidual> {
    let mut positions: Vec<u8> = budget.alloc(usize::from(max_num_coeff))?;
    positions.clear();
    let _ = category; // Reserved: the four base categories share one table
                       // shape today; a future category with its own table
                       // (chroma DC, 8x8) would switch on it here.

    // clause 7.3.5.3.3: significant_coeff_flag/last_significant_coeff_flag
    // are read for scan positions 0..maxNumCoeff-2 inclusive; the final
    // position's significance is never signalled — it is implied by having
    // reached it without an earlier last_significant_coeff_flag == 1.
    debug_assert!(max_num_coeff >= 1, "residual_block_cabac: max_num_coeff must be >= 1");
    let last_scan_idx = max_num_coeff.saturating_sub(1);
    'scan: for i in 0..last_scan_idx {
        if cabac.decode_decision(ctx.sig_mut(i)) == 1 {
            positions.push(i);
            if cabac.decode_decision(ctx.last_sig_mut(i)) == 1 {
                break 'scan;
            }
        }
        if i + 1 == last_scan_idx {
            positions.push(last_scan_idx);
        }
    }
    if last_scan_idx == 0 {
        // A one-coefficient block (not reached by any of this module's four
        // in-scope categories today, but not undefined either): position 0
        // is always significant, no flag read.
        positions.push(0);
    }

    let mut levels: Vec<i32> = budget.alloc(positions.len())?;
    levels.clear();
    let mut num_eq1: u32 = 0;
    let mut num_gt1: u32 = 0;
    // Levels are coded in *reverse* scan order (highest position first),
    // clause 7.3.5.3.3's `for (i = numCoeff - 1; i >= 0; i--)`.
    for _ in 0..positions.len() {
        let magnitude =
            decode_coeff_abs_level_minus1(cabac, ctx, &mut num_eq1, &mut num_gt1).saturating_add(1);
        let sign = cabac.decode_bypass();
        let magnitude_signed = magnitude.cast_signed();
        levels.push(if sign == 1 { -magnitude_signed } else { magnitude_signed });
    }
    levels.reverse();

    Ok(CabacResidual { levels, positions })
}

/// `coeff_abs_level_minus1`, clause 9.3.2.3 (`UEGk`, `k=0`, `uCoff=14`) with
/// 9.3.3.1.3's context derivation — reimplemented bin-by-bin rather than
/// through [`CabacDecoder::decode_uegk`], because that helper uses one
/// context for its whole truncated-unary prefix and this syntax element's
/// prefix does not: `binIdx == 0` and `binIdx >= 1` select from two disjoint
/// context groups.
fn decode_coeff_abs_level_minus1(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextSet,
    num_eq1: &mut u32,
    num_gt1: &mut u32,
) -> u32 {
    const U_COFF: u32 = 14;
    let mut prefix = 0u32;
    while prefix < U_COFF {
        let bin = if prefix == 0 {
            let idx = if *num_gt1 != 0 { 0 } else { (1 + *num_eq1).min(4) };
            cabac.decode_decision(ctx.abs_level_bin0_mut(idx))
        } else {
            let idx = (*num_gt1).min(4);
            cabac.decode_decision(ctx.abs_level_binn_mut(idx))
        };
        if bin == 0 {
            break;
        }
        prefix += 1;
    }
    let value = if prefix >= U_COFF {
        // `decode_bypass_egk` saturates internally (its own doc) rather than
        // erroring on an adversarial all-ones bypass run, so its *result*
        // can sit near `u32::MAX` — found by fuzzing (`h264_entropy`, an
        // all-`0xff` input): a plain `+` here then overflows. `saturating_add`
        // matches the callee's own error-free-input contract instead of
        // introducing the panic the callee was written specifically to avoid.
        prefix.saturating_add(cabac.decode_bypass_egk(0))
    } else {
        prefix
    };
    if value == 0 {
        *num_eq1 += 1;
    } else {
        *num_gt1 += 1;
    }
    value
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::items_after_statements, reason = "test code")]
mod tests {
    use super::*;
    use vaco_codec_cabac::CabacEncoder;
    use vaco_limits::Limits;

    fn budget() -> Budget {
        Budget::new(Limits::default())
    }

    /// A test-only encoder mirroring [`residual_block_cabac`]'s exact
    /// bin sequence and context-selection formulas, for the same reason
    /// `vaco-codec-cabac` itself has an encoder: an arithmetic coder cannot
    /// be exercised against a hand-written bit pattern, only against its
    /// own inverse. This proves the *structure* (bin order, which context
    /// group each bin selects) is self-consistent; it does not independently
    /// verify the `(m, n)` initialisation values themselves — see the
    /// module doc's confidence note.
    fn encode_fixture(
        enc: &mut CabacEncoder,
        ctx: &mut ContextSet,
        max_num_coeff: u8,
        positions: &[u8],
        levels: &[i32],
    ) {
        let last_scan_idx = max_num_coeff - 1;
        for i in 0..last_scan_idx {
            let sig = u32::from(positions.contains(&i));
            enc.encode_decision(&mut ctx.significant_coeff_flag[usize::from(i)], sig);
            if sig == 1 {
                let is_last = positions.last() == Some(&i);
                enc.encode_decision(
                    &mut ctx.last_significant_coeff_flag[usize::from(i)],
                    u32::from(is_last),
                );
                if is_last {
                    break;
                }
            }
        }
        let mut num_eq1 = 0u32;
        let mut num_gt1 = 0u32;
        for &level in levels.iter().rev() {
            let magnitude = level.unsigned_abs() - 1;
            const U_COFF: u32 = 14;
            let prefix = magnitude.min(U_COFF);
            for k in 0..prefix {
                let c = if k == 0 {
                    let idx = if num_gt1 != 0 { 0 } else { (1 + num_eq1).min(4) };
                    &mut ctx.coeff_abs_level_minus1_bin0[idx as usize]
                } else {
                    let idx = num_gt1.min(4);
                    &mut ctx.coeff_abs_level_minus1_binn[idx as usize]
                };
                enc.encode_decision(c, 1);
            }
            if prefix < U_COFF {
                let c = if prefix == 0 {
                    let idx = if num_gt1 != 0 { 0 } else { (1 + num_eq1).min(4) };
                    &mut ctx.coeff_abs_level_minus1_bin0[idx as usize]
                } else {
                    let idx = num_gt1.min(4);
                    &mut ctx.coeff_abs_level_minus1_binn[idx as usize]
                };
                enc.encode_decision(c, 0);
            } else {
                enc.encode_bypass_egk(0, magnitude - U_COFF);
            }
            if magnitude == 0 {
                num_eq1 += 1;
            } else {
                num_gt1 += 1;
            }
            enc.encode_bypass(u32::from(level < 0));
        }
    }

    #[test]
    fn residual_block_cabac_round_trips_through_its_own_test_encoder() {
        let positions = [2u8, 5, 9];
        let levels = [3i32, -1, 5];
        let mut enc = CabacEncoder::new();
        let mut ctx = ContextSet::new(26);
        encode_fixture(&mut enc, &mut ctx, 16, &positions, &levels);
        enc.encode_terminate(1);
        let bytes = enc.finish();

        let mut dec = CabacDecoder::new(&bytes);
        let mut ctx2 = ContextSet::new(26);
        let mut b = budget();
        let out = residual_block_cabac(&mut dec, &mut ctx2, ContextCategory::Luma4x4, 16, &mut b)
            .unwrap();
        assert_eq!(out.positions, positions);
        assert_eq!(out.levels, levels);
    }

    #[test]
    fn residual_block_cabac_single_coefficient() {
        let positions = [0u8];
        let levels = [1i32];
        let mut enc = CabacEncoder::new();
        let mut ctx = ContextSet::new(26);
        encode_fixture(&mut enc, &mut ctx, 16, &positions, &levels);
        enc.encode_terminate(1);
        let bytes = enc.finish();

        let mut dec = CabacDecoder::new(&bytes);
        let mut ctx2 = ContextSet::new(26);
        let mut b = budget();
        let out = residual_block_cabac(&mut dec, &mut ctx2, ContextCategory::LumaDc, 16, &mut b)
            .unwrap();
        assert_eq!(out.positions, positions);
        assert_eq!(out.levels, levels);
    }

    #[test]
    fn residual_block_cabac_last_position_significant_with_no_explicit_flag() {
        // Every position up to and including the final one is significant —
        // the last one's flag is never read (see the module doc).
        let positions: Vec<u8> = (0u8..15).collect();
        let levels = vec![1i32; 15];
        let mut enc = CabacEncoder::new();
        let mut ctx = ContextSet::new(26);
        encode_fixture(&mut enc, &mut ctx, 15, &positions, &levels);
        enc.encode_terminate(1);
        let bytes = enc.finish();

        let mut dec = CabacDecoder::new(&bytes);
        let mut ctx2 = ContextSet::new(26);
        let mut b = budget();
        let out = residual_block_cabac(&mut dec, &mut ctx2, ContextCategory::LumaAc, 15, &mut b)
            .unwrap();
        assert_eq!(out.positions, positions);
        assert_eq!(out.levels, levels);
    }

    #[test]
    fn residual_block_cabac_large_level_exercises_the_egk_suffix() {
        // magnitude - 1 == 20 > U_COFF (14), forcing the EGk suffix path.
        let positions = [7u8];
        let levels = [21i32];
        let mut enc = CabacEncoder::new();
        let mut ctx = ContextSet::new(26);
        encode_fixture(&mut enc, &mut ctx, 16, &positions, &levels);
        enc.encode_terminate(1);
        let bytes = enc.finish();

        let mut dec = CabacDecoder::new(&bytes);
        let mut ctx2 = ContextSet::new(26);
        let mut b = budget();
        let out = residual_block_cabac(&mut dec, &mut ctx2, ContextCategory::Luma4x4, 16, &mut b)
            .unwrap();
        assert_eq!(out.positions, positions);
        assert_eq!(out.levels, levels);
    }

    #[test]
    fn context_set_new_produces_valid_states_across_the_full_qp_range() {
        for qp in 0..=51i8 {
            let ctx = ContextSet::new(qp);
            for c in ctx
                .significant_coeff_flag
                .iter()
                .chain(&ctx.last_significant_coeff_flag)
                .chain(&ctx.coeff_abs_level_minus1_bin0)
                .chain(&ctx.coeff_abs_level_minus1_binn)
            {
                assert!(c.state_idx() <= 63);
            }
        }
    }

    /// Regression for a real bug the `h264_entropy` fuzz target found: an
    /// all-`0xff` bypass stream drives `decode_bypass_egk`'s own saturating
    /// accumulator up near `u32::MAX`, and `prefix + decode_bypass_egk(0)`
    /// (then `+ 1` for `magnitude`) overflowed rather than saturating to
    /// match. Exact crashing input: `[3, 255, 255, 255, 255, 255, 255, 255,
    /// 255, 255, 255]` (CABAC mode, `LumaDc`, `max_num_coeff=4`).
    #[test]
    fn residual_block_cabac_does_not_panic_on_an_all_ones_bypass_stream() {
        let data = [0xffu8; 10];
        let mut dec = CabacDecoder::new(&data);
        let mut ctx = ContextSet::new(3);
        let mut b = budget();
        // Must not panic; the decoded content of adversarial input carries
        // no correctness guarantee (`CabacDecoder` itself never fails a
        // read — see `residual_block_cabac`'s own doc on `malformed()`).
        let _ = residual_block_cabac(&mut dec, &mut ctx, ContextCategory::LumaDc, 4, &mut b);
    }
}
