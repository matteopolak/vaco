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
//! # The context tables: now checked against primary text, one gap remains
//!
//! First transcribed from recollection in a network-isolated clean-room
//! environment (D7), with the same weaker-than-ideal confidence
//! `cavlc_tables.rs` describes at more length. Re-verified while building
//! the CABAC macroblock layer (#419), against the same primary source
//! (`provenance/vaco-codec-h264.toml`'s `iso-iec-14496-10-2002-draft`) that
//! source's CAVLC tables were checked against — and that check found the
//! *first* pass here was not merely imprecise but structurally wrong: it
//! used one 15-row `(m, n)` table shared across all four categories and
//! ignoring `cabac_init_idc` entirely, when the primary text gives four
//! categories their own `ctxIdxBlockCatOffset` (Table 9-30) into four
//! different `(m, n)` tables selected by slice type / `cabac_init_idc`
//! (Table 9-11). [`ContextSet::new`]'s current `(m, n)` literals are
//! transcribed row-by-row from that source's Tables 9-19/9-20/9-21, not
//! from recollection. The generalist context-selection *formulas*
//! (position-indexed significance context, the `numDecodAbsLevelEq1`/`Gt1`
//! counter rule for `coeff_abs_level_minus1`) were already correct and are
//! unchanged.
//!
//! As with the CAVLC tables, only one primary source was available in this
//! environment, not two independently cross-checked — recorded as a real
//! limitation, not elided.
//!
//! # Scope within "4x4-shaped" categories
//!
//! `ctxBlockCat` 0 (luma DC), 1 (luma AC), 2 (luma 4x4), 3 (chroma DC), 4
//! (chroma AC) all use the identity mapping ITU-T Table 9-43's base rows
//! describe: `ctxIdxInc(significant_coeff_flag) == levelListIdx`, and
//! likewise for `last_significant_coeff_flag`, both capped by the
//! category's own `maxNumCoeff - 1`. 8x8 transform blocks (`ctxBlockCat` 5,
//! High-profile only) use different tables this module does not implement,
//! the same Main-profile-corpus reason `mb.rs` gives for not reaching them.
//!
//! Chroma DC (`ctxBlockCat` 3, `maxNumCoeff == 4` for 4:2:0) is *not*
//! deferred the way 8x8 is: it appears in almost every real macroblock with
//! any chroma residual at all, so skipping it would fail on real content
//! immediately rather than on a deliberately-avoided corner. It needed one
//! genuine puzzle solved first: Table 9-30 gives it a `ctxIdxBlockCatOffset`
//! of 30 for `coeff_abs_level_minus1` against 39 for the next category —
//! nine contexts spanning that gap, not the ten every other category gets,
//! and nothing in the fetched primary text explains why. Worked out from
//! first principles rather than guessed: `coeff_abs_level_minus1` decodes
//! in *reverse* scan order, and a 4-coefficient block has at most 3 earlier
//! coefficients to have counted toward `numDecodAbsLevelGt1` by the time any
//! one of them is decoded, so `binIdx >= 1`'s `5 + Min(4, numDecodAbsLevelGt1)`
//! can never actually reach `ctxIdxInc == 9` for this category — the tenth
//! slot is unreachable by construction, not omitted by mistake. See
//! [`ContextCategory::ChromaDc`] and [`ContextSet::new`] for where this
//! lands in the array shapes.

use vaco_codec_cabac::{CabacDecoder, ContextInit, ContextModel, init_contexts};
use vaco_core::Result;
use vaco_limits::Budget;

/// The five residual categories this module covers — ITU-T's `ctxBlockCat`
/// 0, 1, 2, 3 and 4. See the module doc for why 5 (8x8, High-profile only)
/// is out of scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextCategory {
    /// `ctxBlockCat` 0: `Intra16x16DCLevel`, `maxNumCoeff == 16`.
    LumaDc,
    /// `ctxBlockCat` 1: `Intra16x16ACLevel`, `maxNumCoeff == 15`.
    LumaAc,
    /// `ctxBlockCat` 2: `LumaLevel4x4`, `maxNumCoeff == 16`.
    Luma4x4,
    /// `ctxBlockCat` 3: `ChromaDCLevel`, `maxNumCoeff == 4` (4:2:0 only —
    /// this crate's whole scope is 4:2:0, see `mb.rs`'s `check_scope`).
    /// Unlike the 8x8 category, this one is *not* deferred: chroma DC
    /// appears in almost every real macroblock with any chroma residual, so
    /// skipping it would fail on real content immediately rather than on a
    /// deliberately-avoided corner. `coeff_abs_level_minus1`'s `binIdx >= 1`
    /// context array is 4 wide here, not 5 — see [`ContextSet::new`]'s own
    /// comment on why, worked out from first principles rather than copied,
    /// since the primary text's Table 9-30 does not explain the gap.
    ChromaDc,
    /// `ctxBlockCat` 4: `ChromaACLevel`, `maxNumCoeff == 15`.
    ChromaAc,
    /// `ctxBlockCat` 5: the High-profile 8x8 luma transform's own residual
    /// (`maxNumCoeff == 64`). Unlike the four categories above,
    /// `significant_coeff_flag`/`last_significant_coeff_flag`'s own
    /// `ctxIdxInc` is *not* the scan position itself -- it is a many-to-one
    /// mapping (Table 9-43's own 8x8 row) onto a much smaller physical
    /// context count (15 for significance, 9 for "last"), reusing the same
    /// context across multiple scan positions. [`ContextSet::sig_mut`]/
    /// [`Self::last_sig_mut`] apply that remap for this category only; see
    /// [`POS2CTX_MAP8X8`]/[`POS2CTX_LAST8X8`]'s own doc for where the
    /// mapping itself comes from.
    ///
    /// There is also no separate `coded_block_flag` for this category at
    /// all (clause 7.3.5.3.3's own `residual_luma()`/`residual_block_cabac`
    /// syntax): an 8x8 block's presence is already fully determined by
    /// `CodedBlockPatternLuma`'s own per-quadrant bit (unlike the 4x4 case,
    /// where CBP is coarser than the individual 4x4 blocks it gates, so a
    /// further flag is genuinely needed to disambiguate which of the four
    /// actually carry a nonzero coefficient) -- confirmed against JM 19.1's
    /// `read_comp_cabac.c::readCompCoeff8x8_CABAC`, which reads a level
    /// directly (via `readRunLevel_CABAC`, the same significance-map+level
    /// chain [`residual_block_cabac`] already implements) with no
    /// `coded_block_flag`-shaped read anywhere in it, gated only by its
    /// caller's own `currMB->cbp & (1 << b8)` check. `crate::mb` reflects
    /// this directly: it calls [`residual_block_cabac`] for this category
    /// whenever `CodedBlockPatternLuma`'s bit is set, with no separate
    /// flag read of its own.
    Luma8x8,
}

/// One block's context set: significance-map contexts, sized for the
/// largest category (16 positions, only 15 of which are ever asked for —
/// the last position needs no flag), plus the two `coeff_abs_level_minus1`
/// context groups (clause 9.3.3.1.3: 5 contexts selecting on
/// `Min(4, 1 + numDecodAbsLevelEq1)` (or 0, if `numDecodAbsLevelGt1 != 0`)
/// for `binIdx == 0`, 5 more selecting on `Min(4, numDecodAbsLevelGt1)` for
/// `binIdx >= 1`).
///
/// One [`ContextSet`] is shared across every block of a *given category*
/// within a slice — CABAC contexts adapt across the whole slice, not per
/// block, same as every other syntax element. A slice needs one
/// [`ContextSet`] per [`ContextCategory`] it exercises (five today), each
/// built with that category's own `(m, n)` initialisation table — clause
/// 9.3.1.1's Table 9-30 assigns each `ctxBlockCat` its own
/// `ctxIdxBlockCatOffset` into the shared `significant_coeff_flag`/
/// `last_significant_coeff_flag`/`coeff_abs_level_minus1` ctxIdx ranges,
/// which is a different `(m, n)` row per category, not a shared one.
///
/// # A real bug this replaced
///
/// The first version of this struct used one 15-row `(m, n)` table shared
/// across every category, and one fixed table regardless of slice type
/// — neither matches the primary text (`provenance/vaco-codec-h264.toml`'s
/// `iso-iec-14496-10-2002-draft`): Table 9-30 gives `significant_coeff_flag`
/// five *different* `ctxIdxBlockCatOffset` values (0, 15, 29, 44, 47) for the
/// five in-scope categories, and Table 9-11 says P/SP/B slices additionally
/// select among three more `(m, n)` tables by `cabac_init_idc` (I/SI slices
/// use a fourth, fixed table) — the "four context-table sets" a real
/// `libx264 -coder cabac` stream actually exercises. The original values
/// did not match *any* of these four tables at *any* offset checked against
/// the primary text; found while building the CABAC macroblock layer (#419)
/// needed to reach a real bit-exact measurement at all. Every `(m, n)` pair
/// below is transcribed directly from that source's Tables 9-19/9-20/9-21,
/// not from recollection.
#[derive(Debug, Clone)]
pub struct ContextSet {
    /// Which category this set was built for -- needed at read time (not
    /// just construction) so [`Self::sig_mut`]/[`Self::last_sig_mut`] know
    /// whether to apply [`POS2CTX_MAP8X8`]/[`POS2CTX_LAST8X8`]'s remap
    /// (`ctxBlockCat` 5 only) or pass the scan position straight through
    /// (every other category, `ctxIdxInc == levelListIdx` directly).
    category: ContextCategory,
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

/// Which `(m, n)` table column clause 9.3.1.1 says to initialise from.
/// `IorSi` is the one fixed table I/SI slices always use; P/SP/B slices
/// select one of three tables by `cabac_init_idc` (clause 7.3.3's slice
/// header field, `PPS`/slice-header already parse it — this module just
/// takes the resolved value).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CabacInit {
    IorSi,
    PSpB(u8),
}

// These 20 tables (`SIG_*`/`LAST_*`/`ABS_BIN0_*`/`ABS_BINN_*` across the
// five `ContextCategory` variants) were originally local `const`s inside
// `ContextSet::new` below -- moved to module scope so a duplicate-table
// test (mirroring `cabac_mb_tables.rs`'s `table_distinctness` module,
// added after that file's own `CBF_CHROMA_AC`/`CBF_CHROMA_DC` copy-paste
// bug was found) can see them. No behavioural change: `ContextSet::new`
// still reads the same names, just from module scope instead of its own
// function body.
// Each row is `[I_or_SI, cabac_init_idc=0, =1, =2]` for one ctxIdx,
// in category order (`LumaDc`, `LumaAc`, `Luma4x4`, `ChromaAc`).
#[rustfmt::skip]
const SIG_LUMA_DC: [[(i16, i16); 4]; 15] = [
    [(-7,93),(-2,85),(-13,103),(-4,86)], [(-11,87),(-6,78),(-13,91),(-12,88)],
    [(-3,77),(-1,75),(-9,89),(-5,82)], [(-5,71),(-7,77),(-14,92),(-3,72)],
    [(-4,63),(2,54),(-8,76),(-4,67)], [(-4,68),(5,50),(-12,87),(-8,72)],
    [(-12,84),(-3,68),(-23,110),(-16,89)], [(-7,62),(1,50),(-24,105),(-9,69)],
    [(-7,65),(6,42),(-10,78),(-1,59)], [(8,61),(-4,81),(-20,112),(5,66)],
    [(5,56),(1,63),(-17,99),(4,57)], [(-2,66),(-4,70),(-78,127),(-4,71)],
    [(1,64),(0,67),(-70,127),(-2,71)], [(0,61),(2,57),(-50,127),(2,58)],
    [(-2,78),(-2,76),(-46,127),(-1,74)],
];
#[rustfmt::skip]
const SIG_LUMA_AC: [[(i16, i16); 4]; 14] = [
    [(1,50),(11,35),(-4,66),(-4,44)], [(7,52),(4,64),(-5,78),(-1,69)],
    [(10,35),(1,61),(-4,71),(0,62)], [(0,44),(11,35),(-8,72),(-7,51)],
    [(11,38),(18,25),(2,59),(-4,47)], [(1,45),(12,24),(-1,55),(-6,42)],
    [(0,46),(13,29),(-7,70),(-3,41)], [(5,44),(13,36),(-6,75),(-6,53)],
    [(31,17),(-10,93),(-8,89),(8,76)], [(1,51),(-7,73),(-34,119),(-9,78)],
    [(7,50),(-2,73),(-3,75),(-11,83)], [(28,19),(13,46),(32,20),(9,52)],
    [(16,33),(9,49),(30,22),(0,67)], [(14,62),(-7,100),(-44,127),(-5,90)],
];
#[rustfmt::skip]
const SIG_LUMA4X4: [[(i16, i16); 4]; 15] = [
    [(-13,108),(9,53),(0,54),(1,67)], [(-15,100),(2,53),(-5,61),(-15,72)],
    [(-13,101),(5,53),(0,58),(-5,75)], [(-13,91),(-2,61),(-1,60),(-8,80)],
    [(-12,94),(0,56),(-3,61),(-21,83)], [(-10,88),(0,56),(-8,67),(-21,64)],
    [(-16,84),(-13,63),(-25,84),(-13,31)], [(-10,86),(-5,60),(-14,74),(-25,64)],
    [(-7,83),(-1,62),(-5,65),(-29,94)], [(-13,87),(4,57),(5,52),(9,75)],
    [(-19,94),(-6,69),(2,57),(17,63)], [(1,70),(4,57),(0,61),(-8,74)],
    [(0,72),(14,39),(-9,69),(-5,35)], [(-5,74),(4,51),(-11,70),(-2,27)],
    [(18,59),(13,68),(18,55),(13,91)],
];
#[rustfmt::skip]
const SIG_CHROMA_AC: [[(i16, i16); 4]; 14] = [
    [(-4,75),(7,50),(9,41),(-10,66)], [(2,72),(16,39),(18,25),(3,62)],
    [(-11,75),(5,44),(9,32),(-3,68)], [(-3,71),(4,52),(5,43),(-20,81)],
    [(15,46),(11,48),(9,47),(0,30)], [(-13,69),(-5,60),(0,44),(1,7)],
    [(0,62),(-1,59),(0,51),(-3,23)], [(0,65),(0,59),(2,46),(-21,74)],
    [(21,37),(22,33),(19,38),(16,66)], [(-15,72),(5,44),(-4,66),(-23,124)],
    [(9,57),(14,43),(15,38),(17,37)], [(16,54),(-1,78),(12,42),(44,-18)],
    [(0,62),(0,60),(9,34),(50,-34)], [(12,72),(9,69),(0,89),(-22,127)],
];

#[rustfmt::skip]
const LAST_LUMA_DC: [[(i16, i16); 4]; 15] = [
    [(24,0),(11,28),(4,45),(4,39)], [(15,9),(2,40),(10,28),(0,42)],
    [(8,25),(3,44),(10,31),(7,34)], [(13,18),(0,49),(33,-11),(11,29)],
    [(15,9),(0,46),(52,-43),(8,31)], [(13,19),(2,44),(18,15),(6,37)],
    [(10,37),(2,51),(28,0),(7,42)], [(12,18),(0,47),(35,-22),(3,40)],
    [(6,29),(4,39),(38,-25),(8,33)], [(20,33),(2,62),(34,0),(13,43)],
    [(15,30),(6,46),(39,-18),(13,36)], [(4,45),(0,54),(32,-12),(4,47)],
    [(1,58),(3,54),(102,-94),(3,55)], [(0,62),(2,58),(0,0),(2,58)],
    [(7,61),(4,63),(56,-15),(6,60)],
];
#[rustfmt::skip]
const LAST_LUMA_AC: [[(i16, i16); 4]; 14] = [
    [(12,38),(6,51),(33,-4),(8,44)], [(11,45),(6,57),(29,10),(11,44)],
    [(15,39),(7,53),(37,-5),(14,42)], [(11,42),(6,52),(51,-29),(7,48)],
    [(13,44),(6,55),(39,-9),(4,56)], [(16,45),(11,45),(52,-34),(4,52)],
    [(12,41),(14,36),(69,-58),(13,37)], [(10,49),(8,53),(67,-63),(9,49)],
    [(30,34),(-1,82),(44,-5),(19,58)], [(18,42),(7,55),(32,7),(10,48)],
    [(10,55),(-3,78),(55,-29),(12,45)], [(17,51),(15,46),(32,1),(0,69)],
    [(17,46),(22,31),(0,0),(20,33)], [(0,89),(-1,84),(27,36),(8,63)],
];
#[rustfmt::skip]
const LAST_LUMA4X4: [[(i16, i16); 4]; 15] = [
    [(26,-19),(25,7),(33,-25),(35,-18)], [(22,-17),(30,-7),(34,-30),(33,-25)],
    [(26,-17),(28,3),(36,-28),(28,-3)], [(30,-25),(28,4),(38,-28),(24,10)],
    [(28,-20),(32,0),(38,-27),(27,0)], [(33,-23),(34,-1),(34,-18),(34,-14)],
    [(37,-27),(30,6),(35,-16),(52,-44)], [(33,-23),(30,6),(34,-14),(39,-24)],
    [(40,-28),(32,9),(32,-8),(19,17)], [(38,-17),(31,19),(37,-6),(31,25)],
    [(33,-11),(26,27),(35,0),(36,29)], [(40,-15),(26,30),(30,10),(24,33)],
    [(41,-6),(37,20),(28,18),(34,15)], [(38,1),(28,34),(26,25),(30,20)],
    [(41,17),(17,70),(29,41),(22,73)],
];
#[rustfmt::skip]
const LAST_CHROMA_AC: [[(i16, i16); 4]; 14] = [
    [(37,-16),(16,30),(14,35),(19,16)], [(35,-4),(18,32),(18,31),(15,36)],
    [(38,-8),(18,35),(17,35),(15,36)], [(38,-3),(22,29),(21,30),(21,28)],
    [(37,3),(24,31),(17,45),(25,21)], [(38,5),(23,38),(20,42),(30,20)],
    [(42,0),(18,43),(18,45),(31,12)], [(35,16),(20,41),(27,26),(27,16)],
    [(39,22),(11,63),(16,54),(24,42)], [(14,48),(9,59),(7,66),(0,93)],
    [(27,37),(9,64),(16,56),(14,56)], [(21,60),(-1,94),(11,73),(15,57)],
    [(12,68),(-2,89),(10,67),(26,38)], [(2,97),(-9,108),(-10,116),(-24,127)],
];

#[rustfmt::skip]
const ABS_BIN0_LUMA_DC: [[(i16, i16); 4]; 5] = [
    [(-3,71),(-6,76),(-23,112),(-24,115)], [(-6,42),(-2,44),(-15,71),(-22,82)],
    [(-5,50),(0,45),(-7,61),(-9,62)], [(-3,54),(0,52),(0,53),(0,53)],
    [(-2,62),(-3,64),(-5,66),(0,59)],
];
#[rustfmt::skip]
const ABS_BINN_LUMA_DC: [[(i16, i16); 4]; 5] = [
    [(0,58),(-2,59),(-11,77),(-14,85)], [(1,63),(-4,70),(-9,80),(-13,89)],
    [(-2,72),(-4,75),(-9,84),(-13,94)], [(-1,74),(-8,82),(-10,87),(-11,92)],
    [(-9,91),(-17,102),(-34,127),(-29,127)],
];
#[rustfmt::skip]
const ABS_BIN0_LUMA_AC: [[(i16, i16); 4]; 5] = [
    [(-5,67),(-9,77),(-21,101),(-21,100)], [(-5,27),(3,24),(-3,39),(-14,57)],
    [(-3,39),(0,42),(-5,53),(-12,67)], [(-2,44),(0,48),(-7,61),(-11,71)],
    [(0,46),(0,55),(-11,75),(-10,77)],
];
#[rustfmt::skip]
const ABS_BINN_LUMA_AC: [[(i16, i16); 4]; 5] = [
    [(-16,64),(-6,59),(-15,77),(-21,85)], [(-8,68),(-7,71),(-17,91),(-16,88)],
    [(-10,78),(-12,83),(-25,107),(-23,104)], [(-6,77),(-11,87),(-25,111),(-15,98)],
    [(-10,86),(-30,119),(-28,122),(-37,127)],
];
#[rustfmt::skip]
const ABS_BIN0_LUMA4X4: [[(i16, i16); 4]; 5] = [
    [(-12,92),(1,58),(-11,76),(-10,82)], [(-15,55),(-3,29),(-10,44),(-8,48)],
    [(-10,60),(-1,36),(-10,52),(-8,61)], [(-6,62),(1,38),(-10,57),(-8,66)],
    [(-4,65),(2,43),(-9,58),(-7,70)],
];
#[rustfmt::skip]
const ABS_BINN_LUMA4X4: [[(i16, i16); 4]; 5] = [
    [(-12,73),(-6,55),(-16,72),(-14,75)], [(-8,76),(0,58),(-7,69),(-10,79)],
    [(-7,80),(0,64),(-4,69),(-9,83)], [(-9,88),(-3,74),(-5,74),(-12,92)],
    [(-17,110),(-10,90),(-9,86),(-18,108)],
];
#[rustfmt::skip]
const ABS_BIN0_CHROMA_AC: [[(i16, i16); 4]; 5] = [
    [(-8,78),(0,58),(3,52),(-13,81)], [(-5,33),(8,5),(7,4),(-6,38)],
    [(-4,48),(10,14),(10,8),(-13,62)], [(-2,53),(14,18),(17,8),(-6,58)],
    [(-3,62),(13,27),(16,19),(-2,59)],
];
#[rustfmt::skip]
const ABS_BINN_CHROMA_AC: [[(i16, i16); 4]; 5] = [
    [(-13,71),(2,40),(3,37),(-16,73)], [(-10,79),(0,58),(-1,61),(-10,76)],
    [(-12,86),(-3,70),(-5,73),(-13,86)], [(-13,90),(-6,79),(-1,70),(-9,83)],
    [(-14,97),(-8,85),(-4,78),(-10,87)],
];

// Chroma DC (`ctxBlockCat` 3), `maxNumCoeff == 4`: only 3
// significant/last positions (0..maxNumCoeff-2) and, per Table 9-30,
// only 9 `coeff_abs_level_minus1` contexts rather than the 10 every
// other category gets (offset 30 for this category, 39 for the
// next). Worked out from first principles, not copied from anywhere:
// `coeff_abs_level_minus1` is decoded in *reverse* scan order, so
// `numDecodAbsLevelGt1` when decoding any one of at most 4
// coefficients has at most 3 *earlier* coefficients to have counted
// — it can never reach 4, so binIdx>=1's `5 + Min(4, ...)` formula's
// cap is never the binding constraint here and never produces
// `ctxIdxInc == 9`. bin0 keeps the full 5 slots (`numDecodAbsLevelEq1`
// can reach 3, and the `numDecodAbsLevelGt1 != 0` branch reaches 0
// independently, so all of 0..4 are reachable); binn gets only 4
// (`ctxIdxInc` 5..8). The fifth `coeff_abs_level_minus1_binn` slot
// this category's 4-row table leaves uninitialised is accordingly
// unreachable for any conformant encoder's output; `abs_level_binn_mut`
// would still return it rather than panic if adversarial input ever
// asked, which is the same graceful-degradation trade every other
// `.get_mut().unwrap_or(scratch)` fallback in this module makes.
#[rustfmt::skip]
const SIG_CHROMA_DC: [[(i16, i16); 4]; 3] = [
    [(-8,102),(3,64),(-4,71),(3,65)], [(-15,100),(1,61),(0,58),(-7,69)],
    [(0,95),(9,63),(7,61),(8,77)],
];
#[rustfmt::skip]
const LAST_CHROMA_DC: [[(i16, i16); 4]; 3] = [
    [(30,-6),(1,67),(0,75),(20,34)], [(27,3),(5,59),(2,72),(19,31)],
    [(26,22),(9,67),(8,77),(27,44)],
];
#[rustfmt::skip]
const ABS_BIN0_CHROMA_DC: [[(i16, i16); 4]; 5] = [
    [(-11,97),(0,70),(2,66),(-4,79)], [(-20,84),(-4,29),(-9,34),(-22,69)],
    [(-11,79),(5,31),(1,32),(-16,75)], [(-6,73),(7,42),(11,31),(-2,58)],
    [(-4,74),(1,59),(5,52),(1,58)],
];
#[rustfmt::skip]
const ABS_BINN_CHROMA_DC: [[(i16, i16); 4]; 4] = [
    [(-13,86),(-2,58),(-2,55),(-13,78)], [(-13,96),(-3,72),(-2,67),(-9,83)],
    [(-11,97),(-3,81),(0,73),(-4,81)], [(-19,117),(-11,97),(-8,89),(-13,99)],
];

// `ctxBlockCat` 5 (8x8 luma, High profile). This crate's on-hand
// `iso-iec-14496-10-2002-draft` source predates the 8x8 transform entirely
// (the same gap `mb.rs`'s own module doc names for the transform itself),
// so none of the six tables below are transcribed from that primary text.
// They are instead read from JM 19.1's `lib/lcommon/ctx_tables.h`
// (BSD/Tier A per `provenance/sources.toml`) -- `INIT_MAP`/`INIT_LAST`/
// `INIT_ONE`/`INIT_ABS`, row index 2 (`LUMA_8x8` in that file's own
// `defines.h` enum), each of the four `[I_or_SI, idc0, idc1, idc2]`
// columns read the same way `ContextSet::new`'s existing five categories
// already are. Cross-checked two ways before trusting the row index:
// row 0 (`LUMA_16DC`) and row 5 (`LUMA_4x4`) of the *same* JM tables match
// this module's own already-landed `SIG_LUMA_DC`/`SIG_LUMA4X4` (etc.)
// columns exactly, so the row layout this module reads `LUMA_8x8` from is
// independently confirmed correct by two categories whose values were
// already verified against the primary spec text.
#[rustfmt::skip]
const SIG_LUMA8X8: [[(i16, i16); 4]; 15] = [
    [(-17,120),(-4,79),(-5,85),(-3,78)], [(-20,112),(-7,71),(-6,81),(-8,74)],
    [(-18,114),(-5,69),(-10,77),(-9,72)], [(-11,85),(-9,70),(-7,81),(-10,72)],
    [(-15,92),(-8,66),(-17,80),(-18,75)], [(-14,89),(-10,68),(-18,73),(-12,71)],
    [(-26,71),(-19,73),(-4,74),(-11,63)], [(-15,81),(-12,69),(-10,83),(-5,70)],
    [(-14,80),(-16,70),(-9,71),(-17,75)], [(0,68),(-15,67),(-9,67),(-14,72)],
    [(-14,70),(-20,62),(-1,61),(-16,67)], [(-24,56),(-19,70),(-8,66),(-8,53)],
    [(-23,68),(-16,66),(-14,66),(-14,59)], [(-24,50),(-22,65),(0,59),(-9,52)],
    [(-11,74),(-20,63),(2,59),(-11,68)],
];
#[rustfmt::skip]
const LAST_LUMA8X8: [[(i16, i16); 4]; 9] = [
    [(23,-13),(9,-2),(17,-10),(9,-2)], [(26,-13),(26,-9),(32,-13),(30,-10)],
    [(40,-15),(33,-9),(42,-9),(31,-4)], [(49,-14),(39,-7),(49,-5),(33,-1)],
    [(44,3),(41,-2),(53,0),(33,7)], [(45,6),(45,3),(64,3),(31,12)],
    [(44,34),(49,9),(68,10),(37,23)], [(33,54),(45,27),(66,27),(31,38)],
    [(19,82),(36,59),(47,57),(20,64)],
];
#[rustfmt::skip]
const ABS_BIN0_LUMA8X8: [[(i16, i16); 4]; 5] = [
    [(-3,75),(-6,66),(-5,71),(-9,71)], [(-1,23),(-7,35),(0,24),(-7,37)],
    [(1,34),(-7,42),(-1,36),(-8,44)], [(1,43),(-8,45),(-2,42),(-11,49)],
    [(0,54),(-5,48),(-2,52),(-10,56)],
];
#[rustfmt::skip]
const ABS_BINN_LUMA8X8: [[(i16, i16); 4]; 5] = [
    [(-2,55),(-12,56),(-9,57),(-12,59)], [(0,61),(-6,60),(-6,63),(-8,63)],
    [(1,64),(-5,62),(-4,65),(-9,67)], [(0,68),(-8,66),(-4,67),(-6,68)],
    [(-9,92),(-8,76),(-7,82),(-10,79)],
];

/// Table 9-43's own `ctxIdxInc` mapping for `significant_coeff_flag` when
/// `ctxBlockCat == 5` (frame-coded, i.e. the "zig-zag scan" row -- this
/// crate is frame-only, `mb.rs`'s own `check_scope` refuses MBAFF/field
/// pictures, so the interlace row is never needed and not transcribed).
/// 63 entries, one per scan position `0..62` (position 63, the last, is
/// never asked -- same convention every other category's array already
/// follows). Read from JM 19.1's `cabac.c::pos2ctx_map8x8` (BSD/Tier A),
/// the same source and confidence level as this category's init tables
/// above -- this crate's primary spec text predates the category entirely.
#[rustfmt::skip]
const POS2CTX_MAP8X8: [u8; 63] = [
     0,  1,  2,  3,  4,  5,  5,  4,  4,  3,  3,  4,  4,  4,  5,  5,
     4,  4,  4,  4,  3,  3,  6,  7,  7,  7,  8,  9, 10,  9,  8,  7,
     7,  6, 11, 12, 13, 11,  6,  7,  8,  9, 14, 10,  9,  8,  6, 11,
    12, 13, 11,  6,  9, 14, 10,  9, 11, 12, 13, 11, 14, 10, 12,
];

/// Table 9-43's own `ctxIdxInc` mapping for `last_significant_coeff_flag`
/// when `ctxBlockCat == 5`, frame-coded -- same source/scope note as
/// [`POS2CTX_MAP8X8`]. Read from JM 19.1's `cabac.c::pos2ctx_last8x8`.
#[rustfmt::skip]
const POS2CTX_LAST8X8: [u8; 63] = [
    0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    3, 3, 3, 3, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4, 4, 4,
    5, 5, 5, 5, 6, 6, 6, 6, 7, 7, 7, 7, 8, 8, 8,
];

/// One context category's `(m, n)` init pair (Tables 9-19/9-20/9-21) for
/// each of the four `cabac_init_idc`-selected columns, per context index.
type CabacInitRow = [(i16, i16); 4];

impl ContextSet {
    /// Build and initialise for one [`ContextCategory`] from `slice_qp` and
    /// `init`, clause 9.3.1.1. `(m, n)` literals below: transcribed from
    /// `provenance/vaco-codec-h264.toml`'s primary source, Tables 9-19
    /// (`significant_coeff_flag`), 9-20 (`last_significant_coeff_flag`), and
    /// 9-21 (`coeff_abs_level_minus1`) — see this struct's own doc for why a
    /// per-category, per-`cabac_init_idc` table replaced a single shared one.
    #[must_use]
    pub fn new(category: ContextCategory, slice_qp: i8, init: CabacInit) -> Self {
        let col = match init {
            CabacInit::IorSi => 0,
            CabacInit::PSpB(idc) => 1 + usize::from(idc.min(2)),
        };


        let (sig, last, bin0, binn): (
            &[CabacInitRow],
            &[CabacInitRow],
            &[CabacInitRow],
            &[CabacInitRow],
        ) = match category {
                ContextCategory::LumaDc => (&SIG_LUMA_DC, &LAST_LUMA_DC, &ABS_BIN0_LUMA_DC, &ABS_BINN_LUMA_DC),
                ContextCategory::LumaAc => (&SIG_LUMA_AC, &LAST_LUMA_AC, &ABS_BIN0_LUMA_AC, &ABS_BINN_LUMA_AC),
                ContextCategory::Luma4x4 => (&SIG_LUMA4X4, &LAST_LUMA4X4, &ABS_BIN0_LUMA4X4, &ABS_BINN_LUMA4X4),
                ContextCategory::ChromaDc => (&SIG_CHROMA_DC, &LAST_CHROMA_DC, &ABS_BIN0_CHROMA_DC, &ABS_BINN_CHROMA_DC),
                ContextCategory::ChromaAc => (&SIG_CHROMA_AC, &LAST_CHROMA_AC, &ABS_BIN0_CHROMA_AC, &ABS_BINN_CHROMA_AC),
                ContextCategory::Luma8x8 => (&SIG_LUMA8X8, &LAST_LUMA8X8, &ABS_BIN0_LUMA8X8, &ABS_BINN_LUMA8X8),
            };

        let build = |rows: &[[(i16, i16); 4]], dst: &mut [ContextModel]| {
            let inits: Vec<ContextInit> = rows.iter().map(|row| {
                let (m, n) = row.get(col).copied().unwrap_or((0, 0));
                ContextInit::new(m, n)
            }).collect();
            init_contexts(dst, &inits, slice_qp);
        };
        let mut s = Self {
            category,
            significant_coeff_flag: [ContextModel::UNINITIALISED; 15],
            last_significant_coeff_flag: [ContextModel::UNINITIALISED; 15],
            coeff_abs_level_minus1_bin0: [ContextModel::UNINITIALISED; 5],
            coeff_abs_level_minus1_binn: [ContextModel::UNINITIALISED; 5],
            scratch: ContextModel::UNINITIALISED,
        };
        // Each category's own array is smaller than 15/5 for LumaAc/ChromaAc
        // (14 significance contexts, `maxNumCoeff - 1`) — `init_contexts`
        // writes only as many entries as it is given rows for, per its own
        // contract, leaving this struct's fixed-size arrays' unused tail at
        // `UNINITIALISED`, which `residual_block_cabac` never asks for
        // (`last_scan_idx` bounds every index by the category's own
        // `max_num_coeff`).
        if let Some(dst) = s.significant_coeff_flag.get_mut(..sig.len()) {
            build(sig, dst);
        }
        if let Some(dst) = s.last_significant_coeff_flag.get_mut(..last.len()) {
            build(last, dst);
        }
        build(bin0, &mut s.coeff_abs_level_minus1_bin0);
        build(binn, &mut s.coeff_abs_level_minus1_binn);
        s
    }
}

impl ContextSet {
    /// `ctxIdxInc` for `significant_coeff_flag` at scan position `i` --
    /// `i` itself for every category except `Luma8x8`, which remaps
    /// through [`POS2CTX_MAP8X8`] first (see [`ContextCategory::Luma8x8`]'s
    /// own doc for why that category alone needs this).
    fn sig_mut(&mut self, i: u8) -> &mut ContextModel {
        let idx = if self.category == ContextCategory::Luma8x8 {
            POS2CTX_MAP8X8.get(usize::from(i)).copied().unwrap_or(0)
        } else {
            i
        };
        let scratch = &mut self.scratch;
        self.significant_coeff_flag.get_mut(usize::from(idx)).unwrap_or(scratch)
    }

    /// [`Self::sig_mut`]'s own counterpart for `last_significant_coeff_flag`,
    /// via [`POS2CTX_LAST8X8`].
    fn last_sig_mut(&mut self, i: u8) -> &mut ContextModel {
        let idx = if self.category == ContextCategory::Luma8x8 {
            POS2CTX_LAST8X8.get(usize::from(i)).copied().unwrap_or(0)
        } else {
            i
        };
        let scratch = &mut self.scratch;
        self.last_significant_coeff_flag.get_mut(usize::from(idx)).unwrap_or(scratch)
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
    max_num_coeff: u8,
    budget: &mut Budget,
) -> Result<CabacResidual> {
    let mut positions: Vec<u8> = budget.alloc(usize::from(max_num_coeff))?;
    positions.clear();

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
        // Routed through `ContextSet`'s own private `sig_mut`/`last_sig_mut`
        // (not raw field indexing) specifically so this fixture stays
        // correct for `Luma8x8`: that category's `ctxIdxInc` is not the
        // scan position itself (see `ContextCategory::Luma8x8`'s own doc),
        // and encoding into the wrong physical context would desync the
        // arithmetic coder's adaptation state, not merely mislabel it.
        let last_scan_idx = max_num_coeff - 1;
        for i in 0..last_scan_idx {
            let sig = u32::from(positions.contains(&i));
            enc.encode_decision(ctx.sig_mut(i), sig);
            if sig == 1 {
                let is_last = positions.last() == Some(&i);
                enc.encode_decision(ctx.last_sig_mut(i), u32::from(is_last));
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
                    ctx.abs_level_bin0_mut(idx)
                } else {
                    let idx = num_gt1.min(4);
                    ctx.abs_level_binn_mut(idx)
                };
                enc.encode_decision(c, 1);
            }
            if prefix < U_COFF {
                let c = if prefix == 0 {
                    let idx = if num_gt1 != 0 { 0 } else { (1 + num_eq1).min(4) };
                    ctx.abs_level_bin0_mut(idx)
                } else {
                    let idx = num_gt1.min(4);
                    ctx.abs_level_binn_mut(idx)
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
        let mut ctx = ContextSet::new(ContextCategory::Luma4x4, 26, CabacInit::IorSi);
        encode_fixture(&mut enc, &mut ctx, 16, &positions, &levels);
        enc.encode_terminate(1);
        let bytes = enc.finish();

        let mut dec = CabacDecoder::new(&bytes);
        let mut ctx2 = ContextSet::new(ContextCategory::Luma4x4, 26, CabacInit::IorSi);
        let mut b = budget();
        let out = residual_block_cabac(&mut dec, &mut ctx2, 16, &mut b)
            .unwrap();
        assert_eq!(out.positions, positions);
        assert_eq!(out.levels, levels);
    }

    #[test]
    fn residual_block_cabac_single_coefficient() {
        let positions = [0u8];
        let levels = [1i32];
        let mut enc = CabacEncoder::new();
        let mut ctx = ContextSet::new(ContextCategory::LumaDc, 26, CabacInit::IorSi);
        encode_fixture(&mut enc, &mut ctx, 16, &positions, &levels);
        enc.encode_terminate(1);
        let bytes = enc.finish();

        let mut dec = CabacDecoder::new(&bytes);
        let mut ctx2 = ContextSet::new(ContextCategory::LumaDc, 26, CabacInit::IorSi);
        let mut b = budget();
        let out = residual_block_cabac(&mut dec, &mut ctx2, 16, &mut b)
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
        let mut ctx = ContextSet::new(ContextCategory::LumaAc, 26, CabacInit::IorSi);
        encode_fixture(&mut enc, &mut ctx, 15, &positions, &levels);
        enc.encode_terminate(1);
        let bytes = enc.finish();

        let mut dec = CabacDecoder::new(&bytes);
        let mut ctx2 = ContextSet::new(ContextCategory::LumaAc, 26, CabacInit::IorSi);
        let mut b = budget();
        let out = residual_block_cabac(&mut dec, &mut ctx2, 15, &mut b)
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
        let mut ctx = ContextSet::new(ContextCategory::Luma4x4, 26, CabacInit::IorSi);
        encode_fixture(&mut enc, &mut ctx, 16, &positions, &levels);
        enc.encode_terminate(1);
        let bytes = enc.finish();

        let mut dec = CabacDecoder::new(&bytes);
        let mut ctx2 = ContextSet::new(ContextCategory::Luma4x4, 26, CabacInit::IorSi);
        let mut b = budget();
        let out = residual_block_cabac(&mut dec, &mut ctx2, 16, &mut b)
            .unwrap();
        assert_eq!(out.positions, positions);
        assert_eq!(out.levels, levels);
    }

    /// `ctxBlockCat` 5's own 64-coefficient block, round-tripped through
    /// [`residual_block_cabac`] like every other category above -- proves
    /// the position remap ([`POS2CTX_MAP8X8`]/[`POS2CTX_LAST8X8`]) and the
    /// 64-length scan both work end to end, not merely that the tables
    /// exist. `encode_fixture` itself uses direct field indexing
    /// (`ctx.significant_coeff_flag[...]`), not `sig_mut`, so this also
    /// exercises the remap asymmetrically: the encoder writes to the same
    /// *physical* context [`residual_block_cabac`]'s own [`ContextSet::sig_mut`]
    /// reads back via the remap, which only round-trips correctly if the
    /// remap is applied consistently -- a wrong or missing remap would
    /// desync the arithmetic coder's context state, not merely misattribute
    /// adaptation, and this test's own decode would then not reproduce the
    /// encoded positions/levels.
    #[test]
    fn residual_block_cabac_luma8x8_round_trips() {
        let positions = [0u8, 5, 30, 62];
        let levels = [4i32, -2, 9, 1];
        let mut enc = CabacEncoder::new();
        let mut ctx = ContextSet::new(ContextCategory::Luma8x8, 26, CabacInit::IorSi);
        encode_fixture(&mut enc, &mut ctx, 64, &positions, &levels);
        enc.encode_terminate(1);
        let bytes = enc.finish();

        let mut dec = CabacDecoder::new(&bytes);
        let mut ctx2 = ContextSet::new(ContextCategory::Luma8x8, 26, CabacInit::IorSi);
        let mut b = budget();
        let out = residual_block_cabac(&mut dec, &mut ctx2, 64, &mut b).unwrap();
        assert_eq!(out.positions, positions);
        assert_eq!(out.levels, levels);
    }

    #[test]
    fn context_set_new_produces_valid_states_across_the_full_qp_range() {
        const CATEGORIES: [ContextCategory; 6] = [
            ContextCategory::LumaDc,
            ContextCategory::LumaAc,
            ContextCategory::Luma4x4,
            ContextCategory::ChromaDc,
            ContextCategory::ChromaAc,
            ContextCategory::Luma8x8,
        ];
        const INITS: [CabacInit; 4] =
            [CabacInit::IorSi, CabacInit::PSpB(0), CabacInit::PSpB(1), CabacInit::PSpB(2)];
        for qp in 0..=51i8 {
            for &category in &CATEGORIES {
                for &init in &INITS {
                    let ctx = ContextSet::new(category, qp, init);
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
        let mut ctx = ContextSet::new(ContextCategory::LumaDc, 3, CabacInit::IorSi);
        let mut b = budget();
        // Must not panic; the decoded content of adversarial input carries
        // no correctness guarantee (`CabacDecoder` itself never fails a
        // read — see `residual_block_cabac`'s own doc on `malformed()`).
        let _ = residual_block_cabac(&mut dec, &mut ctx, 4, &mut b);
    }
}

#[cfg(test)]
mod table_distinctness {
    //! Mirrors `cabac_mb_tables.rs`'s own `table_distinctness` module,
    //! added there after `CBF_CHROMA_AC` turned out to be an exact
    //! duplicate of `CBF_CHROMA_DC` -- a failure mode per-table
    //! verification structurally cannot catch, since it never compares one
    //! table to its neighbours. Every table here is transcribed from a
    //! distinct `ctxBlockCat`/`(significant|last|abs_bin0|abs_binn)`
    //! combination, each its own row range of Tables 9-19/9-20/9-21, so no
    //! legitimate reason exists for any two to hold the same values.
    use super::*;

    fn named_tables() -> Vec<(&'static str, Vec<(i16, i16)>)> {
        vec![
            ("SIG_LUMA_DC", SIG_LUMA_DC.iter().flatten().copied().collect()),
            ("SIG_LUMA_AC", SIG_LUMA_AC.iter().flatten().copied().collect()),
            ("SIG_LUMA4X4", SIG_LUMA4X4.iter().flatten().copied().collect()),
            ("SIG_CHROMA_AC", SIG_CHROMA_AC.iter().flatten().copied().collect()),
            ("SIG_CHROMA_DC", SIG_CHROMA_DC.iter().flatten().copied().collect()),
            ("LAST_LUMA_DC", LAST_LUMA_DC.iter().flatten().copied().collect()),
            ("LAST_LUMA_AC", LAST_LUMA_AC.iter().flatten().copied().collect()),
            ("LAST_LUMA4X4", LAST_LUMA4X4.iter().flatten().copied().collect()),
            ("LAST_CHROMA_AC", LAST_CHROMA_AC.iter().flatten().copied().collect()),
            ("LAST_CHROMA_DC", LAST_CHROMA_DC.iter().flatten().copied().collect()),
            ("ABS_BIN0_LUMA_DC", ABS_BIN0_LUMA_DC.iter().flatten().copied().collect()),
            ("ABS_BINN_LUMA_DC", ABS_BINN_LUMA_DC.iter().flatten().copied().collect()),
            ("ABS_BIN0_LUMA_AC", ABS_BIN0_LUMA_AC.iter().flatten().copied().collect()),
            ("ABS_BINN_LUMA_AC", ABS_BINN_LUMA_AC.iter().flatten().copied().collect()),
            ("ABS_BIN0_LUMA4X4", ABS_BIN0_LUMA4X4.iter().flatten().copied().collect()),
            ("ABS_BINN_LUMA4X4", ABS_BINN_LUMA4X4.iter().flatten().copied().collect()),
            ("ABS_BIN0_CHROMA_AC", ABS_BIN0_CHROMA_AC.iter().flatten().copied().collect()),
            ("ABS_BINN_CHROMA_AC", ABS_BINN_CHROMA_AC.iter().flatten().copied().collect()),
            ("ABS_BIN0_CHROMA_DC", ABS_BIN0_CHROMA_DC.iter().flatten().copied().collect()),
            ("ABS_BINN_CHROMA_DC", ABS_BINN_CHROMA_DC.iter().flatten().copied().collect()),
            ("SIG_LUMA8X8", SIG_LUMA8X8.iter().flatten().copied().collect()),
            ("LAST_LUMA8X8", LAST_LUMA8X8.iter().flatten().copied().collect()),
            ("ABS_BIN0_LUMA8X8", ABS_BIN0_LUMA8X8.iter().flatten().copied().collect()),
            ("ABS_BINN_LUMA8X8", ABS_BINN_LUMA8X8.iter().flatten().copied().collect()),
        ]
    }

    /// Pairs allowed to be byte-identical, with the reason why -- empty
    /// today. `ABS_BINN_CHROMA_DC` is 4 rows against every other table's 5
    /// (see the comment above `SIG_CHROMA_DC`), so it can only ever collide
    /// with another 4-row table, and there are none -- shape alone already
    /// rules that one out, no allowlist entry needed.
    const ALLOWED_DUPLICATES: &[(&str, &str)] = &[];

    #[test]
    fn no_two_tables_are_byte_identical() {
        let tables = named_tables();
        let mut hits = Vec::new();
        for (i, (name_a, vals_a)) in tables.iter().enumerate() {
            for (name_b, vals_b) in tables.iter().skip(i + 1) {
                if vals_a == vals_b {
                    let allowed = ALLOWED_DUPLICATES.iter().any(|&(a, b)| {
                        (a == *name_a && b == *name_b) || (a == *name_b && b == *name_a)
                    });
                    if !allowed {
                        hits.push(format!("{name_a} == {name_b} ({} entries)", vals_a.len()));
                    }
                }
            }
        }
        assert!(
            hits.is_empty(),
            "found residual context-init tables that are byte-for-byte identical to \
             each other, which Tables 9-19/9-20/9-21's per-ctxBlockCat row assignment \
             gives no legitimate reason for: {hits:?}"
        );
    }

    #[test]
    fn named_tables_is_not_accidentally_empty() {
        let tables = named_tables();
        assert_eq!(tables.len(), 24, "expected exactly 24 named tables in this file");
        for (name, vals) in &tables {
            assert!(!vals.is_empty(), "table {name} flattened to zero entries");
        }
    }
}
