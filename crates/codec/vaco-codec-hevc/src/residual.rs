//! `residual_coding()`, ITU-T H.265 §7.3.8.11, and its context derivations
//! (§9.3.4.2.5 `sig_coeff_flag`, §9.3.4.2.6 `coeff_abs_level_greater1/2_flag`,
//! §9.3.3.11's Golomb-Rice/`k`-th order Exp-Golomb `coeff_abs_level_remaining`).
//!
//! Derived from the ITU-T H.265 specification and cross-checked line-by-line
//! against the HM reference decoder's `TDecSbac::parseCoeffNxN` /
//! `TComTrQuant::getSigCtxInc` / `calcPatternSigCtx` / `getSigCoeffGroupCtxInc`
//! (BSD-3-Clause, Tier A — see `cabac_ctx`'s module doc for the licence
//! reasoning). The control flow below follows that reference's loop shape
//! directly (descending scan order, a `pos` list built from the last
//! significant coefficient toward DC, `c1`/`ctxSet` state carried *between*
//! sub-blocks) because this is exactly the kind of syntax where a
//! re-derivation "from the clause text" independently is the trap
//! `AGENT-CONSTRAINTS.md` describes: two transcriptions of one sentence are
//! not an independent check.
//!
//! Transform-skip and transquant-bypass presence gates and reconstruction are
//! handled by [`crate::ctu`]; this module receives their resolved sign-hiding
//! choice. Range-extension features (persistent Rice adaptation, extended
//! precision, RDPCM, CABAC-bypass alignment, cross-component prediction) are
//! gated off at the SPS by [`crate::decoder`].

use vaco_codec_cabac::{CabacDecoder, ContextModel};

/// `usize` to `isize`, saturating. Every scan position this module handles
/// is bounded by `32*32 = 1024`, far inside `isize`'s range on any
/// supported target; this exists so the conversion is explicit rather than
/// a wrapping `as`.
fn isz(x: usize) -> isize {
    isize::try_from(x).unwrap_or(isize::MAX)
}

/// Decode a context-coded bin against `arr[idx]`, or against a throwaway
/// default context if `idx` is somehow out of range.
///
/// Every index computed in this module is derived from HEVC's own
/// spec-bounded ranges, so the fallback should never trigger on a conforming
/// bitstream; when malformed input reaches an inconsistent state, deciding
/// against a scratch context — whose subsequent adaptation nobody reads — is
/// how this crate stays panic-free (D6) rather than indexing out of bounds.
fn decide_at(cabac: &mut CabacDecoder<'_>, arr: &mut [ContextModel], idx: usize) -> u32 {
    if let Some(cm) = arr.get_mut(idx) {
        cabac.decode_decision(cm)
    } else {
        let mut scratch = ContextModel::default();
        cabac.decode_decision(&mut scratch)
    }
}

use crate::cabac_ctx::{CTX_IND_MAP_4X4, ContextBank, GROUP_IDX, MIN_IN_GROUP};
use crate::scan::{ScanOrder, generate, generate_grouped};

/// §9.3.4.2.3: `(ctxOffset, ctxShift)` for `last_sig_coeff_{x,y}_prefix`.
fn last_sig_ctx_params(log2_size: u32, is_chroma: bool) -> (u32, u32) {
    if is_chroma {
        (0, log2_size.saturating_sub(2))
    } else {
        let offset = 3 * log2_size.saturating_sub(2) + (log2_size.saturating_sub(1) >> 2);
        let shift = (log2_size + 1) >> 2;
        (offset, shift)
    }
}

/// §9.3.4.2.4 (`calcPatternSigCtx`): the 2-bit "which neighbouring
/// coefficient groups are already significant" pattern.
fn pattern_sig_ctx(
    cg_flags: &[bool],
    cg_x: usize,
    cg_y: usize,
    groups_w: usize,
    groups_h: usize,
) -> u32 {
    if groups_w <= 1 && groups_h <= 1 {
        return 0;
    }
    let right = cg_x + 1 < groups_w
        && cg_flags
            .get(cg_y * groups_w + cg_x + 1)
            .copied()
            .unwrap_or(false);
    let below = cg_y + 1 < groups_h
        && cg_flags
            .get((cg_y + 1) * groups_w + cg_x)
            .copied()
            .unwrap_or(false);
    u32::from(right) + (u32::from(below) << 1)
}

/// §9.3.4.2.5 (`getSigCtxInc`): `sig_coeff_flag`'s within-component `ctxInc`
/// (the caller adds the component/size-class base on top).
fn sig_ctx_inc(pattern: u32, x: u32, y: u32, log2_size: u32, is_chroma: bool) -> u32 {
    if x == 0 && y == 0 {
        return 0;
    }
    if log2_size == 2 {
        let idx = (4 * y + x) as usize;
        return u32::from(CTX_IND_MAP_4X4.get(idx).copied().unwrap_or(0));
    }
    let (px, py) = (x & 3, y & 3);
    let cnt = match pattern {
        0 => {
            let total = px + py;
            if total >= 3 {
                0
            } else if total >= 1 {
                1
            } else {
                2
            }
        }
        1 => {
            if py >= 2 {
                0
            } else if py >= 1 {
                1
            } else {
                2
            }
        }
        2 => {
            if px >= 2 {
                0
            } else if px >= 1 {
                1
            } else {
                2
            }
        }
        _ => 2,
    };
    let not_first_group = (x >> 2) + (y >> 2) > 0;
    // `notFirstGroupNeighbourhoodContextOffset`: 3 for luma, 0 for chroma.
    let offset = if not_first_group && !is_chroma { 3 } else { 0 };
    offset + cnt
}

/// The size-class base offset within a component's `sig_coeff_flag` array
/// (`significanceMapContextSetStart` in HM's `ContextTables.h`, values
/// `{0, 9, 21, 27}` luma / `{0, 9, 12, 15}` chroma indexed by
/// `{CONTEXT_TYPE_4x4, CONTEXT_TYPE_8x8, CONTEXT_TYPE_NxN,
/// CONTEXT_TYPE_SINGLE}`).
///
/// Two things this must get right that are easy to miss from the formula
/// alone (both confirmed against `ISLICE_LUMA_SIGNIFICANCE_CONTEXT`'s own
/// column comments in `ContextTables.h`, which lay the 28 luma contexts out
/// as `DC | 4x4(8) | 8x8-diag(6) | 8x8-nondiag(6) | NxN-first(3) |
/// NxN-other(3) | single(1)`):
///
/// * **16x16 and 32x32 luma share one base (21).** `CONTEXT_TYPE_NxN` is not
///   size-specific past 8x8 — the `27` slot is `CONTEXT_TYPE_SINGLE`, a
///   range-extension-only context this crate never reaches (no extended
///   precision, no persistent Rice, no cross-component prediction), so `27`
///   must never be produced here at all.
/// * **8x8 luma has two disjoint 6-context ranges (9 and 15), chosen by scan
///   order** — `9` for diagonal, `15` for horizontal/vertical, per the
///   `8x8 Diagonal Scan` / `8x8 Non-Diagonal Scan` column split. Chroma has
///   no such split (`8x8 Any group`): its 8x8 base is always `9` regardless
///   of scan order, because chroma's own mode-dependent-scan rule
///   ([`crate::intra_mode::scan_order_for_mode`]) never actually reaches 8x8
///   in this crate's 4:2:0-only scope (mode-dependent scan applies to chroma
///   only at `log2TrafoSize == 2`), but the base is scan-order-independent
///   for chroma either way.
fn sig_base(log2_size: u32, is_chroma: bool, order: ScanOrder) -> u32 {
    match (is_chroma, log2_size) {
        (_, 2) => 0,
        (true, 3) => 9,
        (false, 3) => {
            if order == ScanOrder::Diag {
                9
            } else {
                15
            }
        }
        (false, _) => 21,
        (true, _) => 12,
    }
}

/// §9.3.4.2.2 (`getSigCoeffGroupCtxInc`): `coded_sub_block_flag`'s `ctxInc`.
fn sig_group_ctx_inc(
    cg_flags: &[bool],
    cg_x: usize,
    cg_y: usize,
    groups_w: usize,
    groups_h: usize,
) -> u32 {
    let right = cg_x + 1 < groups_w
        && cg_flags
            .get(cg_y * groups_w + cg_x + 1)
            .copied()
            .unwrap_or(false);
    let below = cg_y + 1 < groups_h
        && cg_flags
            .get((cg_y + 1) * groups_w + cg_x)
            .copied()
            .unwrap_or(false);
    u32::from(right || below)
}

/// §9.3.4.2.6 (`getContextSetIndex`): the `ctxSet` for
/// `coeff_abs_level_greater1_flag`/`_greater2_flag`, given the sub-block
/// index and whether the *previous* sub-block ended with `c1 == 0` (a `>1`
/// coefficient was found among its first 8).
fn context_set_index(is_chroma: bool, sub_block: usize, prev_c1_was_zero: bool) -> usize {
    let base = if is_chroma { 4 } else { 0 };
    let mut set = if sub_block == 0 || is_chroma { 0 } else { 2 };
    if prev_c1_was_zero {
        set += 1;
    }
    base + set
}

/// §9.3.3.11's Golomb-Rice/Exp-Golomb-`k` `coeff_abs_level_remaining`
/// (`xReadCoefRemainExGolomb` with `useLimitedPrefixLength == false`, this
/// crate's only supported case — extended precision is refused at the SPS).
///
/// The truncated-unary prefix has no bitstream-defined ceiling (a run of `1`
/// bins is a well-formed, if absurd, encoding of an enormous value), so it is
/// capped defensively per D6 — no conforming encoder at any bit depth this
/// crate accepts produces a prefix anywhere near this long.
const MAX_REMAINING_PREFIX: u32 = 48;
const REDUCTION: u32 = 3;

fn read_coeff_remain(cabac: &mut CabacDecoder<'_>, rice_param: u32) -> u32 {
    let mut prefix = 0u32;
    while cabac.decode_bypass() == 1 && prefix < MAX_REMAINING_PREFIX {
        prefix += 1;
    }
    if prefix < REDUCTION {
        (prefix << rice_param) + cabac.decode_bypass_bits(rice_param)
    } else {
        let suffix_len = (prefix - REDUCTION + rice_param).min(31);
        let suffix = cabac.decode_bypass_bits(suffix_len);
        let base = (1u32.checked_shl(prefix - REDUCTION).unwrap_or(u32::MAX))
            .wrapping_sub(1)
            .wrapping_add(REDUCTION);
        (base << rice_param) + suffix
    }
}

/// One transform block's decoded coefficients: `(x, y, value)` triples,
/// signed, still at the quantised level (dequantisation is
/// [`crate::transform`]'s concern).
#[derive(Debug, Default)]
pub(crate) struct Coeffs {
    pub values: Vec<(u8, u8, i32)>,
}

/// `residual_coding()`, §7.3.8.11.
///
/// `log2_size` is the transform block's log2 side length (2..=5, this
/// crate's whole scope since 4:2:0 chroma never exceeds 16x16 for the CU
/// sizes it supports); `order` is the scan this block uses, already resolved
/// by the caller from the intra prediction mode (§6.5.1).
pub(crate) fn residual_coding(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    log2_size: u32,
    order: ScanOrder,
    is_chroma: bool,
    sign_data_hiding: bool,
) -> Coeffs {
    let size = 1usize << log2_size;
    #[allow(
        clippy::integer_division,
        reason = "group size is always a whole 4x4 division of the transform block"
    )]
    let groups = (size / 4).max(1);
    let scan = generate_grouped(size, order);
    let group_scan = generate(groups, order);

    let (x_off, x_shift) = last_sig_ctx_params(log2_size, is_chroma);
    let (y_off, y_shift) = last_sig_ctx_params(log2_size, is_chroma);
    let (last_x, last_y) = decode_last_sig_xy(
        cabac,
        ctx,
        log2_size,
        order,
        is_chroma,
        (x_off, x_shift),
        (y_off, y_shift),
    );

    let last_scan_pos = scan
        .iter()
        .position(|&(x, y)| u32::from(x) == last_x && u32::from(y) == last_y)
        .unwrap_or(0);

    let comp_base = if is_chroma { 28usize } else { 0usize };
    let sig_class_base = usize::try_from(sig_base(log2_size, is_chroma, order)).unwrap_or(0);

    let mut out = Vec::new();
    let mut cg_flags = vec![false; groups.saturating_mul(groups).max(1)];
    let last_subset = last_scan_pos >> 4;
    let mut c1_was_zero = false; // carried `c1 == 0` from the previously processed (higher-index) subset

    let mut scan_pos_sig = isz(last_scan_pos);

    for subset in (0..=last_subset).rev() {
        let sub_pos0 = subset << 4;
        let (cg_x, cg_y) = group_scan
            .get(subset)
            .map_or((0, 0), |&(x, y)| (usize::from(x), usize::from(y)));

        let mut pos: Vec<usize> = Vec::new();
        let mut last_nz_scan: Option<isize> = None;
        let mut first_nz_scan: isize = isz(sub_pos0 + 15);

        if scan_pos_sig == isz(last_scan_pos) {
            last_nz_scan = Some(scan_pos_sig);
            first_nz_scan = scan_pos_sig;
            let scan_pos_sig_u = usize::try_from(scan_pos_sig).unwrap_or(0);
            if let Some(&(x, y)) = scan.get(scan_pos_sig_u) {
                pos.push(usize::from(y) * size + usize::from(x));
            }
            scan_pos_sig -= 1;
        }

        let group_sig = if subset == last_subset || subset == 0 {
            cg_flags_set(&mut cg_flags, cg_x, cg_y, groups, true);
            true
        } else {
            let inc = sig_group_ctx_inc(&cg_flags, cg_x, cg_y, groups, groups);
            let row = if is_chroma { 2usize } else { 0usize };
            let bit = decide_at(cabac, &mut ctx.sig_coeff_group, row + inc as usize) != 0;
            cg_flags_set(&mut cg_flags, cg_x, cg_y, groups, bit);
            bit
        };

        if group_sig {
            let pattern = pattern_sig_ctx(&cg_flags, cg_x, cg_y, groups, groups);
            while scan_pos_sig >= isz(sub_pos0) {
                let sp = scan_pos_sig as usize;
                let Some(&(x, y)) = scan.get(sp) else {
                    scan_pos_sig -= 1;
                    continue;
                };
                let explicit = scan_pos_sig > isz(sub_pos0) || subset == 0 || !pos.is_empty();
                let sig = if explicit {
                    // HM's `getSigCtxInc` returns a literal `0` for the DC
                    // position (`(posX + posY) == 0`) — a special case that
                    // *bypasses* `firstSignificanceMapContext` (this file's
                    // `sig_class_base`) entirely rather than feeding `0` into
                    // it. DC is one context shared by every transform size
                    // within a component, at the component's own base index
                    // (`comp_base`) — not `comp_base + sig_class_base`. That
                    // distinction is invisible at 4x4 (`sig_class_base == 0`
                    // there), which is exactly why 4x4 residual blocks decoded
                    // byte-exact while every 8x8+ block desynchronised here.
                    let full = if x == 0 && y == 0 {
                        comp_base
                    } else {
                        let inc =
                            sig_ctx_inc(pattern, u32::from(x), u32::from(y), log2_size, is_chroma);
                        comp_base + sig_class_base + inc as usize
                    };
                    decide_at(cabac, &mut ctx.sig_coeff_flag, full) != 0
                } else {
                    true
                };
                if sig {
                    pos.push(usize::from(y) * size + usize::from(x));
                    if last_nz_scan.is_none() {
                        last_nz_scan = Some(scan_pos_sig);
                    }
                    first_nz_scan = scan_pos_sig;
                }
                scan_pos_sig -= 1;
            }
        }
        scan_pos_sig = isz(sub_pos0) - 1;

        let num_nonzero = pos.len();
        if num_nonzero == 0 {
            continue;
        }

        let sign_hidden =
            sign_data_hiding && last_nz_scan.unwrap_or(0).saturating_sub(first_nz_scan) >= 4;

        let ctx_set = context_set_index(is_chroma, subset, c1_was_zero);
        let mut c1 = 1u32;
        let num_c1 = num_nonzero.min(8);
        let mut first_c2_idx: Option<usize> = None;
        let mut escape_present = num_nonzero > 8;
        let mut abs_level = vec![1i32; num_nonzero];
        let greater1_base = ctx_set * 4;

        for (idx, slot) in abs_level.iter_mut().enumerate().take(num_c1) {
            let bin = decide_at(cabac, &mut ctx.greater1, greater1_base + c1 as usize) != 0;
            if bin {
                c1 = 0;
                if first_c2_idx.is_none() {
                    first_c2_idx = Some(idx);
                } else {
                    escape_present = true;
                }
            } else if c1 > 0 && c1 < 3 {
                c1 += 1;
            }
            *slot = i32::from(bin) + 1;
        }
        c1_was_zero = c1 == 0;

        if c1 == 0
            && let Some(i0) = first_c2_idx
        {
            let bin = decide_at(cabac, &mut ctx.greater2, ctx_set) != 0;
            if let Some(slot) = abs_level.get_mut(i0) {
                *slot = i32::from(bin) + 2;
            }
            if bin {
                escape_present = true;
            }
        }

        let sign_count = if sign_hidden {
            num_nonzero - 1
        } else {
            num_nonzero
        };
        let signs = cabac.decode_bypass_bits(u32::try_from(sign_count).unwrap_or(0));

        if escape_present {
            let mut first_coeff2 = true;
            let mut rice_param = 0u32;
            for (idx, level) in abs_level.iter_mut().enumerate() {
                let base_level: i32 = if idx < 8 {
                    if first_coeff2 { 3 } else { 2 }
                } else {
                    1
                };
                if *level == base_level {
                    let remaining = read_coeff_remain(cabac, rice_param);
                    *level = i32::try_from(remaining)
                        .unwrap_or(i32::MAX)
                        .saturating_add(base_level);
                    if *level > (3 << rice_param) {
                        rice_param = (rice_param + 1).min(4);
                    }
                }
                if *level >= 2 {
                    first_coeff2 = false;
                }
            }
        }

        let mut abs_sum: i64 = 0;
        for (idx, &blk_pos) in pos.iter().enumerate() {
            let level = i64::from(abs_level.get(idx).copied().unwrap_or(1));
            abs_sum += level;
            let negative = if sign_hidden && idx == num_nonzero - 1 {
                (abs_sum & 1) != 0
            } else {
                sign_at(signs, sign_count, idx)
            };
            let value = if negative { -level } else { level };
            #[allow(
                clippy::integer_division,
                reason = "raster-position decomposition: bx/by from a flat index, the block's own coordinate system"
            )]
            let (bx, by) = (blk_pos % size, blk_pos / size);
            out.push((
                u8::try_from(bx).unwrap_or(0),
                u8::try_from(by).unwrap_or(0),
                i32::try_from(value).unwrap_or(0),
            ));
        }
    }

    Coeffs { values: out }
}

fn cg_flags_set(flags: &mut [bool], x: usize, y: usize, w: usize, v: bool) {
    if let Some(slot) = flags.get_mut(y * w + x) {
        *slot = v;
    }
}

/// Extract the `i`-th (0-based, in decode order) sign bit from a
/// `decode_bypass_bits(count)` result — bit 0 decoded is the
/// most-significant of the `count` bits returned.
fn sign_at(bits: u32, count: usize, i: usize) -> bool {
    if i >= count {
        return false;
    }
    let shift = count - 1 - i;
    (bits >> shift) & 1 != 0
}

fn decode_last_sig_xy(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut ContextBank,
    log2_size: u32,
    order: ScanOrder,
    is_chroma: bool,
    x_params: (u32, u32),
    y_params: (u32, u32),
) -> (u32, u32) {
    // §7.3.8.11: for `SCAN_VER`, width/height (and so the group index
    // ceiling) are swapped before decoding and the result swapped back.
    let swapped = matches!(order, ScanOrder::Vert);
    let size = 1usize << log2_size;
    let max_group = GROUP_IDX.get(size.saturating_sub(1)).copied().unwrap_or(0);
    let base = if is_chroma { 15usize } else { 0usize };

    let (offset_x, shift_x) = x_params;
    let (offset_y, shift_y) = y_params;

    let mut group_x = 0u32;
    for g in 0..max_group {
        let idx = base + offset_x as usize + (usize::from(g) >> shift_x);
        if decide_at(cabac, &mut ctx.last_sig_x, idx) == 0 {
            break;
        }
        group_x = u32::from(g) + 1;
    }
    let mut group_y = 0u32;
    for g in 0..max_group {
        let idx = base + offset_y as usize + (usize::from(g) >> shift_y);
        if decide_at(cabac, &mut ctx.last_sig_y, idx) == 0 {
            break;
        }
        group_y = u32::from(g) + 1;
    }

    let mut pos_x = resolve_group(cabac, group_x);
    let mut pos_y = resolve_group(cabac, group_y);
    if swapped {
        std::mem::swap(&mut pos_x, &mut pos_y);
    }
    (pos_x, pos_y)
}

fn resolve_group(cabac: &mut CabacDecoder<'_>, group: u32) -> u32 {
    if group <= 3 {
        return group;
    }
    let count = (group - 2) >> 1;
    let suffix = cabac.decode_bypass_bits(count);
    MIN_IN_GROUP.get(group as usize).copied().unwrap_or(0) + suffix
}
