//! Reference sample construction (availability, substitution, smoothing) and
//! prediction dispatch, ITU-T H.265 §8.4.4.2.
//!
//! # Why the angular case is hand-rolled rather than routed through
//! `vaco_codec_dsp_intrapred::angular_project`
//!
//! That crate's own doc is explicit that its indexing convention was
//! checked against the *properties* signed-angle projection must have
//! (zero-angle copies exactly, a linear ramp interpolates exactly), not
//! line-by-line against a primary specification edition — and its `ref_at`
//! helper returns `0` for any negative index, which is exactly the regime
//! HEVC's negative-angle modes need (`invAngle`-projected samples reached by
//! extending the main reference array *backwards*, HM's `refMain[k]` for
//! `k < 0`). Wiring that case through a zero-clamped helper would silently
//! zero real reference samples instead of the extended ones a conforming
//! decoder must produce — the "structured, not just imprecise" failure
//! `AGENT-CONSTRAINTS.md` warns is the one that matters. [`predict_angular`]
//! below builds the (possibly negatively indexed) `refMain`/`refSide`
//! construction directly, cross-checked against the HM reference decoder's
//! `TComPrediction::xPredIntraAng` (BSD-3-Clause, Tier A). [`predict_planar`]
//! and [`predict_dc`] have no such ambiguity and do route through the shared
//! crate.

use vaco_codec_dsp_intrapred::{dc_predict, planar_predict};

use crate::framebuf::ReconPlane;
use crate::intra_mode::{ANG_TABLE, DC_IDX, HOR_IDX, INV_ANG_TABLE, PLANAR_IDX, VER_IDX};

/// Per-size-class reference-sample-filtering threshold, Table 8-4 (`m_aucIntraFilter`
/// in HM — indexed by `log2(size) - 2`, so 4x4/8x8/16x16/32x32).
const FILTER_THRESHOLD: [i32; 4] = [10, 7, 1, 0];

/// `usize` to `i32`, saturating — every value this module casts is a
/// coordinate or size bounded by `MAX_CU_SIZE` (at most 32), so the
/// saturation never actually engages; it exists so the conversion is an
/// explicit, checked one rather than a wrapping `as`.
fn iz(x: usize) -> i32 {
    i32::try_from(x).unwrap_or(i32::MAX)
}

/// The concatenated, availability-substituted reference line for one
/// `size x size` block: `line[k]` for `k` in `0..=2*size` is `p[-1][2*size-1-k]`
/// (so `line[2*size]` is the above-left corner and `line[0]` is the
/// bottom-left-most sample), and `line[2*size+1+i]` for `i` in `0..2*size-1`
/// is `p[i][-1]` (the top row extending right past the block).
///
/// Built directly from [`ReconPlane::is_ready`]/[`ReconPlane::get`] rather than a
/// z-scan availability derivation — see `framebuf`'s module doc for why
/// those coincide exactly in this crate's one-slice, no-tile scope.
///
/// `is_intra_neighbor(x, y)` is §8.4.4.2.2's `constrained_intra_pred_flag`
/// gate: "when `constrained_intra_pred_flag` is equal to 1 ... the sample is
/// marked as not available for intra prediction" if the neighbouring
/// prediction block containing `(x, y)` is not coded in an intra prediction
/// mode, on top of the ordinary picture/slice/tile-boundary check
/// [`ReconPlane::is_ready`] already performs. Callers pass `|_, _| true` when
/// `constrained_intra_pred_flag` is 0 (the ordinary availability check is
/// the whole story), and a real neighbour-mode lookup otherwise.
pub(crate) fn build_reference_line(
    plane: &ReconPlane<'_>,
    x0: i32,
    y0: i32,
    size: usize,
    bit_depth: u32,
    is_intra_neighbor: impl Fn(i32, i32) -> bool,
) -> Vec<u16> {
    let n = iz(size);
    let len = 4 * size + 1;
    let mut avail = vec![false; len];
    let mut line = vec![0u16; len];

    // Left column and below-left, bottom to top: k=0 is (x0-1, y0+2n-1).
    for k in 0..2 * size {
        let y = y0 + (2 * n - 1) - iz(k);
        let (a, v) = sample(plane, x0 - 1, y, &is_intra_neighbor);
        set_at(&mut avail, k, a);
        set_at(&mut line, k, v);
    }
    // Corner.
    {
        let (a, v) = sample(plane, x0 - 1, y0 - 1, &is_intra_neighbor);
        set_at(&mut avail, 2 * size, a);
        set_at(&mut line, 2 * size, v);
    }
    // Top row and above-right, left to right.
    for i in 0..2 * size {
        let (a, v) = sample(plane, x0 + iz(i), y0 - 1, &is_intra_neighbor);
        set_at(&mut avail, 2 * size + 1 + i, a);
        set_at(&mut line, 2 * size + 1 + i, v);
    }

    substitute(&mut line, &avail, bit_depth);
    line
}

fn set_at<T: Copy>(v: &mut [T], idx: usize, value: T) {
    if let Some(slot) = v.get_mut(idx) {
        *slot = value;
    }
}

fn sample(plane: &ReconPlane<'_>, x: i32, y: i32, is_intra_neighbor: &impl Fn(i32, i32) -> bool) -> (bool, u16) {
    if plane.is_ready(x, y) && is_intra_neighbor(x, y) {
        let (Ok(ux), Ok(uy)) = (usize::try_from(x), usize::try_from(y)) else {
            return (false, 0);
        };
        (true, plane.get(ux, uy))
    } else {
        (false, 0)
    }
}

/// §8.4.4.2.2's substitution process, at pixel granularity — see
/// [`build_reference_line`]'s doc for why that is equivalent to the
/// specification's unit-granularity search in this crate's scope.
fn substitute(line: &mut [u16], avail: &[bool], bit_depth: u32) {
    if !avail.iter().any(|&a| a) {
        let mid = 1u16 << bit_depth.saturating_sub(1).min(15);
        line.fill(mid);
        return;
    }
    if !avail.first().copied().unwrap_or(false) {
        let Some(j) = avail.iter().position(|&a| a) else { return };
        let v = line.get(j).copied().unwrap_or(0);
        if let Some(slice) = line.get_mut(0..j) {
            slice.fill(v);
        }
    }
    for k in 1..line.len() {
        if !avail.get(k).copied().unwrap_or(false) {
            let prev = line.get(k - 1).copied().unwrap_or(0);
            if let Some(slot) = line.get_mut(k) {
                *slot = prev;
            }
        }
    }
}

/// `top(i)` for `i` in `-1..2*size`, `-1` being the above-left corner.
fn top_at(line: &[u16], size: usize, i: i32) -> u16 {
    if i < 0 {
        return line.get(2 * size).copied().unwrap_or(0);
    }
    line.get(2 * size + 1 + i as usize).copied().unwrap_or(0)
}

/// `left(i)` for `i` in `-1..2*size`, `-1` being the above-left corner.
fn left_at(line: &[u16], size: usize, i: i32) -> u16 {
    if i < 0 {
        return line.get(2 * size).copied().unwrap_or(0);
    }
    let idx = 2 * iz(size) - 1 - i;
    usize::try_from(idx).ok().and_then(|k| line.get(k)).copied().unwrap_or(0)
}

/// Whether §8.4.4.2.3's reference-sample smoothing filter applies to this
/// mode/size — Table 8-4's threshold rule. `is_luma` gates the whole thing:
/// this crate's 4:2:0-only scope never filters chroma (`filterIntraReferenceSamples`
/// in the reference is `isLuma || chFmt == CHROMA_444`, and 4:4:4 is out of
/// scope here — see the crate doc).
#[must_use]
pub(crate) fn should_filter(mode: u8, size: usize, is_luma: bool) -> bool {
    if !is_luma || mode == DC_IDX || size < 4 {
        return false;
    }
    let diff = (i32::from(mode) - i32::from(HOR_IDX)).abs().min((i32::from(mode) - i32::from(VER_IDX)).abs());
    let size_index = size.trailing_zeros().saturating_sub(2) as usize;
    let threshold = FILTER_THRESHOLD.get(size_index).copied().unwrap_or(0);
    diff > threshold
}

/// §8.4.4.2.3's 3-tap `[1,2,1]/4` smoothing filter, plus the strong
/// intra-smoothing bilinear replacement for 32x32 luma
/// (`useStrongIntraSmoothing`), applied to a fresh copy of `line`.
#[must_use]
pub(crate) fn filter_reference_line(line: &[u16], size: usize, bit_depth: u32, strong_smoothing_enabled: bool) -> Vec<u16> {
    let mut out = line.to_vec();
    let n = size;
    let bottom_left = line.first().copied().unwrap_or(0);
    let top_left = line.get(2 * n).copied().unwrap_or(0);
    let top_right = line.get(4 * n).copied().unwrap_or(0);

    let mut strong = strong_smoothing_enabled && n == 32;
    if strong {
        let threshold = 1i32 << bit_depth.saturating_sub(5);
        let mid_left = i32::from(left_at(line, n, iz(n) - 1));
        let mid_top = i32::from(top_at(line, n, iz(n) - 1));
        let bilinear_left = (i32::from(bottom_left) + i32::from(top_left) - 2 * mid_left).abs() < threshold;
        let bilinear_above = (i32::from(top_left) + i32::from(top_right) - 2 * mid_top).abs() < threshold;
        strong = bilinear_left && bilinear_above;
    }

    if strong {
        let shift = i32::try_from((2 * n).trailing_zeros()).unwrap_or(6); // log2(2n)
        let n64 = i64::try_from(n).unwrap_or(0);
        for i in 1..2 * n {
            let i64_ = i64::try_from(i).unwrap_or(0);
            let v = (((2 * n64 - i64_) * i64::from(bottom_left)) + (i64_ * i64::from(top_left)) + n64) >> shift;
            if let Some(slot) = out.get_mut(i) {
                *slot = u16::try_from(v).unwrap_or(0);
            }
        }
        for i in 1..2 * n {
            let i64_ = i64::try_from(i).unwrap_or(0);
            let v = (((2 * n64 - i64_) * i64::from(top_left)) + (i64_ * i64::from(top_right)) + n64) >> shift;
            if let Some(slot) = out.get_mut(2 * n + i) {
                *slot = u16::try_from(v).unwrap_or(0);
            }
        }
        return out;
    }

    // Plain [1,2,1]/4, left column bottom-to-top then top row left-to-right;
    // the two ends (`line[0]` and `line[4n]`) are never filtered.
    for k in 1..2 * n {
        let a = i64::from(line.get(k - 1).copied().unwrap_or(0));
        let b = i64::from(line.get(k).copied().unwrap_or(0));
        let c = i64::from(line.get(k + 1).copied().unwrap_or(0));
        if let Some(slot) = out.get_mut(k) {
            *slot = u16::try_from((a + 2 * b + c + 2) >> 2).unwrap_or(0);
        }
    }
    {
        let a = i64::from(line.get(2 * n - 1).copied().unwrap_or(0));
        let b = i64::from(line.get(2 * n).copied().unwrap_or(0));
        let c = i64::from(line.get(2 * n + 1).copied().unwrap_or(0));
        if let Some(slot) = out.get_mut(2 * n) {
            *slot = u16::try_from((a + 2 * b + c + 2) >> 2).unwrap_or(0);
        }
    }
    for k in 2 * n + 1..4 * n {
        let a = i64::from(line.get(k - 1).copied().unwrap_or(0));
        let b = i64::from(line.get(k).copied().unwrap_or(0));
        let c = i64::from(line.get(k + 1).copied().unwrap_or(0));
        if let Some(slot) = out.get_mut(k) {
            *slot = u16::try_from((a + 2 * b + c + 2) >> 2).unwrap_or(0);
        }
    }
    out
}

/// §8.4.4.2.5 (`INTRA_PLANAR`).
pub(crate) fn predict_planar(line: &[u16], size: usize, dst: &mut [u16]) {
    let n = size;
    let n_i = iz(n);
    let top: Vec<u16> = (0..n_i).map(|i| top_at(line, n, i)).collect();
    let left: Vec<u16> = (0..n_i).map(|i| left_at(line, n, i)).collect();
    let top_right = top_at(line, n, n_i);
    let bottom_left = left_at(line, n, n_i);
    let log2 = n.trailing_zeros();
    planar_predict(dst, &top, &left, top_right, bottom_left, n, log2);
}

/// `INTRA_DC` plus, for luma, §8.4.4.2.5's edge-smoothing post-filter.
pub(crate) fn predict_dc(line: &[u16], size: usize, bit_depth: u32, is_luma: bool, dst: &mut [u16]) {
    let n = size;
    let n_i = iz(n);
    let top: Vec<u16> = (0..n_i).map(|i| top_at(line, n, i)).collect();
    let left: Vec<u16> = (0..n_i).map(|i| left_at(line, n, i)).collect();
    let dc = dc_predict(&top, &left, n, bit_depth);
    dst.iter_mut().take(n * n).for_each(|v| *v = dc);

    if is_luma && n <= 16 {
        let max = (1i32 << bit_depth) - 1;
        if let Some(v) = dst.get_mut(0) {
            let t = i32::from(top_at(line, n, 0));
            let l = i32::from(left_at(line, n, 0));
            *v = u16::try_from((t + l + 2 * i32::from(dc) + 2) >> 2).unwrap_or(0).min(max as u16);
        }
        for x in 1..n {
            if let Some(v) = dst.get_mut(x) {
                let t = i32::from(top_at(line, n, iz(x)));
                *v = u16::try_from((t + 3 * i32::from(dc) + 2) >> 2).unwrap_or(0).min(max as u16);
            }
        }
        for y in 1..n {
            if let Some(v) = dst.get_mut(y * n) {
                let l = i32::from(left_at(line, n, iz(y)));
                *v = u16::try_from((l + 3 * i32::from(dc) + 2) >> 2).unwrap_or(0).min(max as u16);
            }
        }
    }
}

/// The 33 angular modes, §8.4.4.2.6. `mode` is 2..=34 (`DC_IDX`/`PLANAR_IDX`
/// are handled by the other two functions).
pub(crate) fn predict_angular(line: &[u16], size: usize, mode: u8, bit_depth: u32, is_luma: bool, dst: &mut [u16]) {
    let n = size;
    let is_ver = mode >= 18;
    let angle_mode: i32 = if is_ver { i32::from(mode) - i32::from(VER_IDX) } else { -(i32::from(mode) - i32::from(HOR_IDX)) };
    let abs_mode = usize::try_from(angle_mode.unsigned_abs()).unwrap_or(0);
    let angle = angle_mode.signum() * ANG_TABLE.get(abs_mode).copied().unwrap_or(0);
    let inv_angle = INV_ANG_TABLE.get(abs_mode).copied().unwrap_or(0);

    // `main(k)`/`side(k)` for k in -(2n).. 2n: main is refAbove for vertical
    // modes, refLeft for horizontal; side is the other one. `main(0)` and
    // `side(0)` are both the above-left corner.
    let main = |k: i32| if is_ver { top_at(line, n, k - 1) } else { left_at(line, n, k - 1) };
    let side = |k: i32| if is_ver { left_at(line, n, k - 1) } else { top_at(line, n, k - 1) };

    // For angle < 0, extend `main` backward via `side`, HM's own
    // `invAngleSum` walk (rounding constant 128, an 8-bit fixed-point
    // half-step).
    let ext_needed = if angle < 0 {
        usize::try_from((-((iz(n) - 1) * angle)) >> 5).unwrap_or(0) + 1
    } else {
        0
    };
    let mut main_ext = vec![0i32; ext_needed];
    if angle < 0 {
        let mut sum = 128i32;
        for slot in main_ext.iter_mut().rev() {
            sum += inv_angle;
            *slot = i32::from(side(sum >> 8));
        }
    }
    let main_at = |k: i32| -> i32 {
        if k >= 0 {
            i32::from(main(k))
        } else {
            let idx = iz(ext_needed) + k;
            usize::try_from(idx).ok().and_then(|i| main_ext.get(i)).copied().unwrap_or(0)
        }
    };

    let mut tmp = vec![0u16; n * n];
    for y in 0..n {
        let delta_pos = (iz(y) + 1) * angle;
        let delta_int = delta_pos.div_euclid(32);
        let delta_frac = delta_pos.rem_euclid(32);
        for x in 0..n {
            let base = iz(x) + delta_int;
            let v = if delta_frac == 0 {
                main_at(base + 1)
            } else {
                let a = main_at(base + 1);
                let b = main_at(base + 2);
                ((32 - delta_frac) * a + delta_frac * b + 16) >> 5
            };
            if let Some(slot) = tmp.get_mut(y * n + x) {
                *slot = u16::try_from(v).unwrap_or(0);
            }
        }
    }

    if angle == 0 && is_luma && n <= 16 {
        let max = (1i32 << bit_depth) - 1;
        for y in 0..n {
            if let Some(v) = tmp.get_mut(y * n) {
                let adj = (i32::from(side(iz(y) + 1)) - i32::from(side(0))) >> 1;
                *v = u16::try_from((i32::from(*v) + adj).clamp(0, max)).unwrap_or(0);
            }
        }
    }

    for y in 0..n {
        for x in 0..n {
            let v = tmp.get(y * n + x).copied().unwrap_or(0);
            let (dr, dc) = if is_ver { (y, x) } else { (x, y) };
            if let Some(slot) = dst.get_mut(dr * n + dc) {
                *slot = v;
            }
        }
    }
}

/// Dispatch on `mode` (0..=34) and compose the whole prediction block.
pub(crate) fn predict(mode: u8, line: &[u16], size: usize, bit_depth: u32, is_luma: bool, dst: &mut [u16]) {
    match mode {
        PLANAR_IDX => predict_planar(line, size, dst),
        DC_IDX => predict_dc(line, size, bit_depth, is_luma, dst),
        _ => predict_angular(line, size, mode, bit_depth, is_luma, dst),
    }
}
