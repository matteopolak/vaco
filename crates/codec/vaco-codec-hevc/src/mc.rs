//! Motion compensation: the luma 8-tap and chroma 4-tap separable
//! interpolation filters (§8.5.3.3.3), applied through picture-edge
//! clamping rather than a padded reference buffer.
//!
//! # Why edge clamping instead of a padded picture
//!
//! HM (and most C++ decoders) pad every reconstructed picture with a border
//! of replicated edge samples (`TComPicYuv::extendPicBorder`) wide enough
//! that a filter tap's reference read never needs a bounds check. This
//! crate's [`crate::framebuf::Plane`] has no such border — every prediction
//! source read goes through [`clamped_sample`], which clamps the integer
//! sample position to `[0, width) x [0, height)` per tap. The two are
//! exactly equivalent (a padded border *is* the clamped value, stored
//! instead of computed), and clamping avoids growing every picture's
//! allocation by a filter-width border for a crate whose own `Budget`
//! already accounts for exact plane dimensions.
//!
//! # Scope: single-reference interpolation only
//!
//! [`predict_block`] produces a *final*, already-rounded-and-clipped
//! prediction block for a **uni-predictive** PU — HM's own `isLast` is
//! always `true` here, since a single-reference PU never needs to average
//! two interpolated sides (`bi == false` throughout HM's own
//! `xPredInterUni`/`xPredInterBlk`). [`predict_block_intermediate`] is the
//! one call this crate's bi-predictive path (a B slice with both
//! `predFlagL0` and `predFlagL1` set) also uses: it stops one clause
//! earlier, at §8.5.3.3.3's own *unshifted, unclipped* `predSampleLX`, which
//! both [`crate::weight::resolve_list`]'s explicit weighted sample
//! prediction (§8.5.3.3.4.3, applied via [`apply_weight`]/[`apply_weight_bi`])
//! and the unweighted bi-predictive combine ([`default_biprediction`],
//! §8.5.3.3.4.2) need to combine with a second side — folding the final
//! shift into the interpolation the way [`predict_block`] does is only valid
//! for a single, already-final side.
//!
//! # Specification
//!
//! ITU-T H.265 (08/2021) §8.5.3.3.3 (luma sample interpolation), §8.5.3.3.4.2
//! (the chroma equivalent, referenced from §8.5.3.3.4 as the same process at
//! eighth-sample precision). Cross-checked against HM 18.0's
//! `TComInterpolationFilter` (Tier A, BSD-3-Clause): the filter taps
//! (`m_lumaFilter`/`m_chromaFilter`), and `filter<N, isVertical, isFirst,
//! isLast>`'s exact shift/offset/clip arithmetic for the `isFirst == isLast
//! == true` (single-pass) and `isFirst=true,isLast=false` /
//! `isFirst=false,isLast=true` (two-pass, both fractions non-zero) cases —
//! the `isFirst=false,isLast=false` case (bi-prediction's own intermediate
//! stage) is never reached from this crate's uni-prediction-only scope and
//! is not implemented.

use crate::framebuf::Plane;

/// §8.5.3.3.3's `fL`: the 8-tap luma filter, one row per quarter-pel
/// fraction (`0` is the identity/copy row, present for uniformity even
/// though callers special-case `frac == 0` into a plain copy before ever
/// indexing this table).
const LUMA_FILTER: [[i32; 8]; 4] = [
    [0, 0, 0, 64, 0, 0, 0, 0],
    [-1, 4, -10, 58, 17, -5, 1, 0],
    [-1, 4, -11, 40, 40, -11, 4, -1],
    [0, 1, -5, 17, 58, -10, 4, -1],
];

/// §8.5.3.3.4.2's `fC`: the 4-tap chroma filter, one row per eighth-pel
/// fraction.
const CHROMA_FILTER: [[i32; 4]; 8] = [
    [0, 64, 0, 0],
    [-2, 58, 10, -2],
    [-4, 54, 16, -2],
    [-6, 46, 28, -4],
    [-4, 36, 36, -4],
    [-4, 28, 46, -6],
    [-2, 16, 54, -4],
    [-2, 10, 58, -2],
];

/// `IF_INTERNAL_PREC` (14) less one, i.e. the fixed internal-precision
/// offset HM's own two-pass filter centres its intermediate buffer on —
/// independent of bit depth (unlike `head_room` below).
const IF_INTERNAL_OFFS: i32 = 1 << 13;
/// `IF_FILTER_PREC`: log2 of the filter taps' own unity gain (they sum to
/// `64 = 1 << 6`).
const IF_FILTER_PREC: i32 = 6;

/// A reference sample at `(x, y)`, clamped to the plane's own bounds — see
/// the module doc for why this stands in for a padded reference picture.
fn clamped_sample(plane: &Plane, x: i32, y: i32) -> i32 {
    let (w, h) = plane.dims();
    let cx = x.clamp(0, i32::try_from(w).unwrap_or(1) - 1);
    let cy = y.clamp(0, i32::try_from(h).unwrap_or(1) - 1);
    let (Ok(ux), Ok(uy)) = (usize::try_from(cx), usize::try_from(cy)) else {
        return 0;
    };
    i32::from(plane.get(ux, uy))
}

/// One filter tap sum, horizontal direction, taps centred so tap `N/2 - 1`
/// lands on the requested integer position (matching HM's own
/// `src -= (N/2 - 1) * cStride` before its dot product).
fn tap_sum_horizontal(plane: &Plane, x0: i32, y: i32, taps: &[i32]) -> i32 {
    let half = i32::try_from(taps.len() >> 1).unwrap_or(0) - 1;
    taps.iter()
        .enumerate()
        .map(|(i, &c)| c * clamped_sample(plane, x0 + i32::try_from(i).unwrap_or(0) - half, y))
        .sum()
}

fn tap_sum_vertical(plane: &Plane, x: i32, y0: i32, taps: &[i32]) -> i32 {
    let half = i32::try_from(taps.len() >> 1).unwrap_or(0) - 1;
    taps.iter()
        .enumerate()
        .map(|(i, &c)| c * clamped_sample(plane, x, y0 + i32::try_from(i).unwrap_or(0) - half))
        .sum()
}

/// Same as [`tap_sum_horizontal`] but reading from an already-produced `i32`
/// intermediate buffer (the two-pass case's first-stage output) instead of a
/// [`Plane`], with the same edge-by-replication semantics — the intermediate
/// buffer is itself already `height + taps - 1` rows tall specifically so no
/// clamping is needed vertically within it (see [`predict_block`]'s two-pass
/// branch, which pads its own vertical extent), only horizontally, since the
/// intermediate buffer is exactly `width` wide with no horizontal padding of
/// its own.
fn tap_sum_vertical_buf(buf: &[i32], stride: usize, x: usize, row0: usize, taps: &[i32]) -> i32 {
    taps.iter()
        .enumerate()
        .map(|(i, &c)| c * buf.get((row0 + i) * stride + x).copied().unwrap_or(0))
        .sum()
}

/// §8.5.3.3.3/.4.2's motion-compensated prediction for one plane: `ref_plane`
/// is the reference picture's own plane (already the correct component —
/// luma or one chroma plane), `int_x0`/`int_y0` the integer part of the
/// motion vector already added to the block's own top-left (so `(int_x0,
/// int_y0)` is where a zero-fraction fetch would read from), `frac_x`/
/// `frac_y` the fractional part in the filter table's own units (`0..4` for
/// luma, `0..8` for chroma — the caller picks the right `taps` table and
/// fractional mask/shift for its own bit depth via `is_luma`).
///
/// Always produces a *final* prediction (`isLast == true` throughout, per
/// the module doc): output values are already rounded and clipped to
/// `[0, (1 << bit_depth) - 1]`.
pub(crate) fn predict_block(
    ref_plane: &Plane,
    int_x0: i32,
    int_y0: i32,
    frac_x: i32,
    frac_y: i32,
    width: usize,
    height: usize,
    bit_depth: u32,
    is_luma: bool,
    out: &mut [i32],
) {
    let max_val = (1i32 << bit_depth) - 1;

    if frac_x == 0 && frac_y == 0 {
        for y in 0..height {
            for x in 0..width {
                if let Some(slot) = out.get_mut(y * width + x) {
                    *slot = clamped_sample(
                        ref_plane,
                        int_x0 + i32::try_from(x).unwrap_or(0),
                        int_y0 + i32::try_from(y).unwrap_or(0),
                    );
                }
            }
        }
        return;
    }

    let filter_row = |frac: i32| -> &'static [i32] {
        if is_luma {
            LUMA_FILTER
                .get(usize::try_from(frac).unwrap_or(0) & 3)
                .map_or(&[][..], |r| &r[..])
        } else {
            CHROMA_FILTER
                .get(usize::try_from(frac).unwrap_or(0) & 7)
                .map_or(&[][..], |r| &r[..])
        }
    };
    let h_taps = filter_row(frac_x);
    let v_taps = filter_row(frac_y);

    if frac_y == 0 {
        // Horizontal-only, single pass, final: shift = IF_FILTER_PREC,
        // offset = 1 << (shift - 1), clipped — HM's `filter<N, false, true,
        // true>`.
        let offset = 1i32 << (IF_FILTER_PREC - 1);
        for y in 0..height {
            for x in 0..width {
                let sum = tap_sum_horizontal(
                    ref_plane,
                    int_x0 + i32::try_from(x).unwrap_or(0),
                    int_y0 + i32::try_from(y).unwrap_or(0),
                    h_taps,
                );
                if let Some(slot) = out.get_mut(y * width + x) {
                    *slot = ((sum + offset) >> IF_FILTER_PREC).clamp(0, max_val);
                }
            }
        }
        return;
    }

    if frac_x == 0 {
        // Vertical-only, single pass, final — HM's `filter<N, true, true,
        // true>`.
        let offset = 1i32 << (IF_FILTER_PREC - 1);
        for y in 0..height {
            for x in 0..width {
                let sum = tap_sum_vertical(
                    ref_plane,
                    int_x0 + i32::try_from(x).unwrap_or(0),
                    int_y0 + i32::try_from(y).unwrap_or(0),
                    v_taps,
                );
                if let Some(slot) = out.get_mut(y * width + x) {
                    *slot = ((sum + offset) >> IF_FILTER_PREC).clamp(0, max_val);
                }
            }
        }
        return;
    }

    // Both fractions non-zero: horizontal pass first (isFirst=true,
    // isLast=false), producing an intermediate buffer `height + taps - 1`
    // rows tall so the vertical pass's own tap footprint never needs to
    // re-clamp against the plane — it reads purely from this buffer.
    // `head_room` never exceeds `IF_FILTER_PREC` for any bit depth >= 8 (HM's
    // own comment on this exact formula: "shift will remain non-negative for
    // bit depths of 8->20"), and this crate's `bit_depth` is always exactly
    // 8 (`check_scope` refuses anything else) — so `h_shift` below is always
    // `>= 0` and a plain `>>` (never a negative shift) is correct, not an
    // unchecked assumption.
    let head_room = (14i32 - i32::try_from(bit_depth).unwrap_or(8)).max(2);
    let n = h_taps.len();
    let half = i32::try_from(n >> 1).unwrap_or(0) - 1;
    let extra_rows = n - 1;
    let buf_rows = height + extra_rows;
    let mut tmp = vec![0i32; width * buf_rows];
    let h_shift = (IF_FILTER_PREC - head_room).max(0);
    let h_offset = -(IF_INTERNAL_OFFS << h_shift);
    for row in 0..buf_rows {
        let src_y = int_y0 + i32::try_from(row).unwrap_or(0) - half;
        for x in 0..width {
            let sum = tap_sum_horizontal(
                ref_plane,
                int_x0 + i32::try_from(x).unwrap_or(0),
                src_y,
                h_taps,
            );
            if let Some(slot) = tmp.get_mut(row * width + x) {
                *slot = (sum + h_offset) >> h_shift;
            }
        }
    }

    // Vertical pass (isFirst=false, isLast=true) over the intermediate
    // buffer — row `half` of `tmp` corresponds to the block's own first
    // output row (`tmp` started `half` rows above `int_y0`).
    let v_shift = IF_FILTER_PREC + head_room;
    let v_offset = (1i32 << (v_shift - 1)) + (IF_INTERNAL_OFFS << IF_FILTER_PREC);
    for y in 0..height {
        for x in 0..width {
            let sum = tap_sum_vertical_buf(&tmp, width, x, y, v_taps);
            if let Some(slot) = out.get_mut(y * width + x) {
                *slot = ((sum + v_offset) >> v_shift).clamp(0, max_val);
            }
        }
    }
}

/// §8.5.3.3.3.1's own `predSampleLXL` (luma) / §8.5.3.3.4.2's chroma
/// equivalent — the interpolation filter's *intermediate* output, before
/// either clause 8.5.3.3.4.2 (default) or 8.5.3.3.4.3 (explicit weighted)
/// applies its own final shift/offset/clip. See the module doc for why
/// [`predict_block`] cannot be reused for this: its single-pass branches
/// fold that final step into the interpolation itself.
///
/// This crate's 8-bit-only scope (`decoder::check_scope` refuses any other
/// `bit_depth`) fixes §8.5.3.3.3.1's own clause-local `shift1`/`shift2`/
/// `shift3` at `Min(4, BitDepth-8) == 0`, `6`, and `Max(2, 14-BitDepth) == 6`
/// respectively, so — unlike [`predict_block`] — this takes no `bit_depth`
/// parameter: there is only one value it could be.
pub(crate) fn predict_block_intermediate(
    ref_plane: &Plane,
    int_x0: i32,
    int_y0: i32,
    frac_x: i32,
    frac_y: i32,
    width: usize,
    height: usize,
    is_luma: bool,
    out: &mut [i32],
) {
    if frac_x == 0 && frac_y == 0 {
        // Case 1: predSampleLXL = refSample << shift3 (shift3 == 6 at 8-bit).
        for y in 0..height {
            for x in 0..width {
                let v = clamped_sample(
                    ref_plane,
                    int_x0 + i32::try_from(x).unwrap_or(0),
                    int_y0 + i32::try_from(y).unwrap_or(0),
                ) << IF_FILTER_PREC;
                if let Some(slot) = out.get_mut(y * width + x) {
                    *slot = v;
                }
            }
        }
        return;
    }

    let filter_row = |frac: i32| -> &'static [i32] {
        if is_luma {
            LUMA_FILTER
                .get(usize::try_from(frac).unwrap_or(0) & 3)
                .map_or(&[][..], |r| &r[..])
        } else {
            CHROMA_FILTER
                .get(usize::try_from(frac).unwrap_or(0) & 7)
                .map_or(&[][..], |r| &r[..])
        }
    };
    let h_taps = filter_row(frac_x);
    let v_taps = filter_row(frac_y);

    if frac_y == 0 {
        // Case 2 (horizontal only): predSampleLXL = sum, unshifted (shift1 == 0).
        for y in 0..height {
            for x in 0..width {
                let sum = tap_sum_horizontal(
                    ref_plane,
                    int_x0 + i32::try_from(x).unwrap_or(0),
                    int_y0 + i32::try_from(y).unwrap_or(0),
                    h_taps,
                );
                if let Some(slot) = out.get_mut(y * width + x) {
                    *slot = sum;
                }
            }
        }
        return;
    }

    if frac_x == 0 {
        // Case 3 (vertical only): same, unshifted.
        for y in 0..height {
            for x in 0..width {
                let sum = tap_sum_vertical(
                    ref_plane,
                    int_x0 + i32::try_from(x).unwrap_or(0),
                    int_y0 + i32::try_from(y).unwrap_or(0),
                    v_taps,
                );
                if let Some(slot) = out.get_mut(y * width + x) {
                    *slot = sum;
                }
            }
        }
        return;
    }

    // Case 4 (both fractions non-zero): a horizontal pass (unshifted, same
    // as case 2/3) into an intermediate buffer `height + taps - 1` rows
    // tall, then a vertical pass over it shifted right by `shift2 == 6` —
    // computed directly from the specification's own two-step formula
    // rather than through `predict_block`'s HM-style biased two-pass
    // arithmetic (that bias exists only to keep `predict_block`'s own
    // *already-clipped final* output in range; recovering the unbiased
    // intermediate value from it would be more roundabout than computing
    // §8.5.3.3.3.1's formula directly here, and there is no clipping to get
    // subtly wrong this way since nothing here clips).
    let n = h_taps.len();
    let half = i32::try_from(n >> 1).unwrap_or(0) - 1;
    let extra_rows = n - 1;
    let buf_rows = height + extra_rows;
    let mut tmp = vec![0i32; width * buf_rows];
    for row in 0..buf_rows {
        let src_y = int_y0 + i32::try_from(row).unwrap_or(0) - half;
        for x in 0..width {
            let sum = tap_sum_horizontal(
                ref_plane,
                int_x0 + i32::try_from(x).unwrap_or(0),
                src_y,
                h_taps,
            );
            if let Some(slot) = tmp.get_mut(row * width + x) {
                *slot = sum;
            }
        }
    }
    for y in 0..height {
        for x in 0..width {
            let sum = tap_sum_vertical_buf(&tmp, width, x, y, v_taps);
            if let Some(slot) = out.get_mut(y * width + x) {
                *slot = sum >> IF_FILTER_PREC;
            }
        }
    }
}

/// One reference's resolved weight/offset for one component, §8.5.3.3.4.3 —
/// `log2_wd` already has `shift1 = Max(2, 14 - BitDepth)` folded in, the same
/// way the specification's own `LumaLog2WeightDenom`/`ChromaLog2WeightDenom`
/// combine with it at the point of use. Built by
/// [`crate::weight::resolve_list`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct Weight {
    pub log2_wd: i32,
    pub w: i32,
    pub o: i32,
}

/// §8.5.3.3.4.3's uni-predictive explicit weighted sample prediction:
///
/// ```text
/// log2Wd >= 1:  Clip3(0, max, ((predSampleLX * w + 2^(log2Wd-1)) >> log2Wd) + o)
/// log2Wd <  1:  Clip3(0, max, predSampleLX * w + o)
/// ```
///
/// `pred` is [`predict_block_intermediate`]'s own `predSampleLX` output.
/// `log2Wd < 1` is unreachable from this crate's own parsed ranges
/// (`luma_log2_weight_denom` is `0..=7`, `shift1 == 6` at 8-bit, so `log2Wd`
/// is always `>= 6`) but the specification states both cases
/// unconditionally, so both are implemented rather than asserted away.
pub(crate) fn apply_weight(pred: i32, weight: Weight, bit_depth: u32) -> i32 {
    let max_val = (1i32 << bit_depth) - 1;
    let val = if weight.log2_wd >= 1 {
        let rounding = 1i32 << (weight.log2_wd - 1);
        (pred.saturating_mul(weight.w).saturating_add(rounding) >> weight.log2_wd)
            .saturating_add(weight.o)
    } else {
        pred.saturating_mul(weight.w).saturating_add(weight.o)
    };
    val.clamp(0, max_val)
}

/// §8.5.3.3.4.2's bi-predictive default weighted sample prediction (the
/// `predFlagL0 == 1 && predFlagL1 == 1` case, eq. 8-264):
///
/// ```text
/// Clip3(0, max, (predSamplesL0 + predSamplesL1 + offset2) >> shift2)
/// ```
///
/// with `shift2 = Max(3, 15 - BitDepth)`, `offset2 = 1 << (shift2 - 1)`.
/// `pred_l0`/`pred_l1` are each [`predict_block_intermediate`]'s own
/// `predSampleLX` output for the respective list.
#[must_use]
pub(crate) fn default_biprediction(pred_l0: i32, pred_l1: i32, bit_depth: u32) -> i32 {
    let max_val = (1i32 << bit_depth) - 1;
    let shift2 = (15 - i32::try_from(bit_depth).unwrap_or(8)).max(3);
    let offset2 = 1i32 << (shift2 - 1);
    ((pred_l0.saturating_add(pred_l1).saturating_add(offset2)) >> shift2).clamp(0, max_val)
}

/// §8.5.3.3.4.3's bi-predictive explicit weighted sample prediction (the
/// `predFlagL0 == 1 && predFlagL1 == 1` case, eq. 8-277):
///
/// ```text
/// Clip3(0, max, (predSamplesL0*w0 + predSamplesL1*w1 + ((o0+o1+1) << log2Wd)) >> (log2Wd + 1))
/// ```
///
/// `w0`/`w1` share the same `log2Wd`: §7.3.6.3's `luma_log2_weight_denom`/
/// `delta_chroma_log2_weight_denom` are slice-wide, not per-reference-list,
/// so [`crate::weight::resolve_list`] always produces the same `log2_wd` for
/// list 0 and list 1 of the same component — `w0.log2_wd` is used for both
/// sides.
#[must_use]
pub(crate) fn apply_weight_bi(
    pred_l0: i32,
    w0: Weight,
    pred_l1: i32,
    w1: Weight,
    bit_depth: u32,
) -> i32 {
    let max_val = (1i32 << bit_depth) - 1;
    let log2_wd = w0.log2_wd;
    let rounding = (w0.o.saturating_add(w1.o).saturating_add(1)) << log2_wd;
    let val = (pred_l0
        .saturating_mul(w0.w)
        .saturating_add(pred_l1.saturating_mul(w1.w))
        .saturating_add(rounding))
        >> (log2_wd + 1);
    val.clamp(0, max_val)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code over fixed scenarios"
)]
mod tests {
    use super::*;
    use vaco_limits::{Budget, Limits};

    fn flat_plane(value: u16, w: usize, h: usize) -> Plane {
        let mut budget = Budget::new(Limits::strict());
        let mut p = Plane::new(&mut budget, w, h).unwrap();
        for y in 0..h {
            for x in 0..w {
                p.set(x, y, value);
            }
        }
        p
    }

    #[test]
    fn a_flat_plane_predicts_the_same_constant_at_every_fraction() {
        let plane = flat_plane(120, 32, 32);
        for (fx, fy) in [(0, 0), (2, 0), (0, 2), (2, 2), (1, 3)] {
            let mut out = vec![0i32; 16];
            predict_block(&plane, 8, 8, fx, fy, 4, 4, 8, true, &mut out);
            assert!(out.iter().all(|&v| v == 120), "fx={fx} fy={fy} out={out:?}");
        }
    }

    #[test]
    fn integer_motion_is_a_plain_copy() {
        let mut budget = Budget::new(Limits::strict());
        let mut plane = Plane::new(&mut budget, 8, 8).unwrap();
        for y in 0..8 {
            for x in 0..8 {
                plane.set(x, y, u16::try_from(x + y * 8).unwrap());
            }
        }
        let mut out = vec![0i32; 4];
        predict_block(&plane, 2, 3, 0, 0, 2, 2, 8, true, &mut out);
        assert_eq!(out, [2 + 3 * 8, 3 + 3 * 8, 2 + 4 * 8, 3 + 4 * 8]);
    }

    #[test]
    fn out_of_bounds_reads_clamp_to_the_edge_sample() {
        let plane = flat_plane(50, 4, 4);
        let mut out = vec![0i32; 1];
        // Way outside the plane in both directions and both fractions.
        predict_block(&plane, -50, 90, 2, 2, 1, 1, 8, true, &mut out);
        assert_eq!(out[0], 50);
    }

    #[test]
    fn chroma_filter_stays_within_the_valid_sample_range() {
        let plane = flat_plane(200, 16, 16);
        let mut out = vec![0i32; 4];
        predict_block(&plane, 4, 4, 5, 3, 2, 2, 8, false, &mut out);
        assert!(out.iter().all(|&v| (0..=255).contains(&v)));
        assert!(out.iter().all(|&v| v == 200));
    }

    #[test]
    fn default_biprediction_averages_two_equal_flat_intermediates() {
        // A flat 120-valued plane's own predSampleLX (integer motion, no
        // fraction) is `120 << 6` per `predict_block_intermediate`'s own
        // Case 1 — averaging it with itself must recover exactly 120.
        let pred = 120i32 << 6;
        assert_eq!(default_biprediction(pred, pred, 8), 120);
    }

    #[test]
    fn default_biprediction_matches_a_hand_derivation() {
        // shift2 = Max(3, 15-8) = 7, offset2 = 64.
        // (1000 + 2000 + 64) >> 7 = 3064 >> 7 = 23.
        assert_eq!(default_biprediction(1000, 2000, 8), 23);
    }

    #[test]
    fn apply_weight_bi_with_neutral_weights_matches_the_default_average() {
        // w0 == w1 == 1 << denom, o0 == o1 == 0 is the neutral case: the
        // weighted bi-pred formula must collapse to the same value
        // `default_biprediction` gives, the bi-predictive analogue of
        // `weight::tests::a_neutral_weight_collapses_to_the_default_shift_and_offset`.
        for denom in 0..=7i32 {
            let w = Weight {
                log2_wd: denom + 6,
                w: 1 << denom,
                o: 0,
            };
            for (p0, p1) in [(-500, 300), (0, 0), (4032, 100), (30000, -1000)] {
                let got = apply_weight_bi(p0, w, p1, w, 8);
                let want = default_biprediction(p0, p1, 8);
                assert_eq!(got, want, "denom={denom} p0={p0} p1={p1}");
            }
        }
    }

    #[test]
    fn apply_weight_bi_matches_a_hand_derivation() {
        // log2Wd = 10, w0=15,o0=-3, w1=17,o1=2.
        // (200*15 + 100*17 + ((-3+2+1)<<10)) >> 11 = (3000+1700+0) >> 11 = 4700>>11 = 2.
        let w0 = Weight {
            log2_wd: 10,
            w: 15,
            o: -3,
        };
        let w1 = Weight {
            log2_wd: 10,
            w: 17,
            o: 2,
        };
        assert_eq!(apply_weight_bi(200, w0, 100, w1, 8), 2);
    }
}
