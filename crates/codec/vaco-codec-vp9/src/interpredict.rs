//! §8.5.2's inter prediction process: motion vector selection (§8.5.2.1),
//! clamping (§8.5.2.2), scaling (§8.5.2.3) and the 8-tap block inter
//! prediction process itself (§8.5.2.4), tied together into the one
//! per-plane-per-block entry point `crate::decode::residual` calls.

use crate::framebuf::Plane;
use crate::refframe::RefSlot;
use crate::tables;

/// §8.5.2.1's `round_mv_comp_q2`.
#[allow(
    clippy::integer_division,
    reason = "spec-defined: (value +/- 1) / 2, a rounding division, not a bug"
)]
fn round_mv_comp_q2(v: i32) -> i32 {
    (if v < 0 { v - 1 } else { v + 1 }) / 2
}

/// §8.5.2.1's `round_mv_comp_q4`.
#[allow(
    clippy::integer_division,
    reason = "spec-defined: (value +/- 2) / 4, a rounding division, not a bug"
)]
fn round_mv_comp_q4(v: i32) -> i32 {
    (if v < 0 { v - 2 } else { v + 2 }) / 4
}

/// §8.5.2.1's motion vector selection process: `BlockMvs[refList]` (the
/// 4 per-4x4-subblock motion vectors for this coding block, all identical
/// when `MiSize >= BLOCK_8X8`) plus `blockIdx`/subsampling in, one
/// `[row, col]` motion vector for this plane's region out.
#[must_use]
pub(crate) fn select_mv(
    block_mvs: &[[i32; 2]; 4],
    block_idx: usize,
    plane: usize,
    mi_size_ge_8x8: bool,
    subsampling_x: bool,
    subsampling_y: bool,
) -> [i32; 2] {
    let get = |i: usize| block_mvs.get(i).copied().unwrap_or([0, 0]);
    if plane == 0 || mi_size_ge_8x8 {
        return get(block_idx);
    }
    match (subsampling_x, subsampling_y) {
        (false, false) => get(block_idx),
        (false, true) => {
            let a = get(block_idx);
            let b = get(block_idx + 2);
            [round_mv_comp_q2(a[0] + b[0]), round_mv_comp_q2(a[1] + b[1])]
        }
        (true, false) => {
            let a = get(block_idx);
            let b = get(block_idx + 1);
            [round_mv_comp_q2(a[0] + b[0]), round_mv_comp_q2(a[1] + b[1])]
        }
        (true, true) => {
            let sum: [i32; 2] =
                [0, 1].map(|c| (0..4).map(|i| get(i).get(c).copied().unwrap_or(0)).sum());
            [round_mv_comp_q4(sum[0]), round_mv_comp_q4(sum[1])]
        }
    }
}

/// §8.5.2.2's motion vector clamping process.
#[allow(
    clippy::too_many_arguments,
    reason = "mirrors the spec's own inputs, which Rust can't read off implicit globals"
)]
#[must_use]
pub(crate) fn clamp_mv(
    mv: [i32; 2],
    mi_row: usize,
    mi_col: usize,
    mi_rows: usize,
    mi_cols: usize,
    bw: usize,
    bh: usize,
    plane: usize,
    subsampling_x: bool,
    subsampling_y: bool,
) -> [i32; 2] {
    let (sx, sy) = if plane == 0 {
        (0u32, 0u32)
    } else {
        (u32::from(subsampling_x), u32::from(subsampling_y))
    };
    // `MiRows - bh - MiRow` / `MiCols - bw - MiCol` are signed per §8.5.2.2
    // — see `mvpred::clamp_mv_row`'s doc comment for why a `saturating_sub`
    // chain here is wrong for any edge block whose nominal size overhangs
    // the frame (routine for this crate's own 176x144 fixtures: mi_rows=18
    // is not a multiple of 8).
    let mb_to_top = -((i32::try_from(mi_row).unwrap_or(0) * tables::MI_SIZE) * 16) >> sy;
    let mb_to_bottom = ((i32::try_from(mi_rows).unwrap_or(0)
        - i32::try_from(bh).unwrap_or(0)
        - i32::try_from(mi_row).unwrap_or(0))
        * tables::MI_SIZE
        * 16)
        >> sy;
    let mb_to_left = -((i32::try_from(mi_col).unwrap_or(0) * tables::MI_SIZE) * 16) >> sx;
    let mb_to_right = ((i32::try_from(mi_cols).unwrap_or(0)
        - i32::try_from(bw).unwrap_or(0)
        - i32::try_from(mi_col).unwrap_or(0))
        * tables::MI_SIZE
        * 16)
        >> sx;
    let spel_left = (tables::INTERP_EXTEND
        + ((i32::try_from(bw).unwrap_or(0) * tables::MI_SIZE) >> sx))
        << tables::SUBPEL_BITS;
    let spel_right = spel_left - tables::SUBPEL_SHIFTS;
    let spel_top = (tables::INTERP_EXTEND
        + ((i32::try_from(bh).unwrap_or(0) * tables::MI_SIZE) >> sy))
        << tables::SUBPEL_BITS;
    let spel_bottom = spel_top - tables::SUBPEL_SHIFTS;
    let row = ((2 * mv[0]) >> sy).clamp(mb_to_top - spel_top, mb_to_bottom + spel_bottom);
    let col = ((2 * mv[1]) >> sx).clamp(mb_to_left - spel_left, mb_to_right + spel_right);
    [row, col]
}

/// The four outputs of §8.5.2.3's motion vector scaling process:
/// `(startX, startY, stepX, stepY)`, all in 1/16th-sample units.
pub(crate) struct ScaledMv {
    pub start_x: i64,
    pub start_y: i64,
    pub step_x: i64,
    pub step_y: i64,
}

/// §8.5.2.3's motion vector scaling process.
#[allow(clippy::too_many_arguments, reason = "mirrors the spec's own inputs")]
#[must_use]
#[allow(
    clippy::integer_division,
    reason = "spec-defined: (RefFrameWidth << REF_SCALE_SHIFT) / FrameWidth, an intentional fixed-point ratio, not a bug"
)]
pub(crate) fn scale_mv(
    clamped_mv: [i32; 2],
    x: usize,
    y: usize,
    plane: usize,
    subsampling_x: bool,
    subsampling_y: bool,
    ref_width: u32,
    ref_height: u32,
    frame_width: u32,
    frame_height: u32,
) -> ScaledMv {
    let x_scale = (i64::from(ref_width) << tables::REF_SCALE_SHIFT) / i64::from(frame_width.max(1));
    let y_scale =
        (i64::from(ref_height) << tables::REF_SCALE_SHIFT) / i64::from(frame_height.max(1));
    let base_x = (i64::try_from(x).unwrap_or(0) * x_scale) >> tables::REF_SCALE_SHIFT;
    let base_y = (i64::try_from(y).unwrap_or(0) * y_scale) >> tables::REF_SCALE_SHIFT;
    let luma_x = if plane > 0 {
        i64::try_from(x).unwrap_or(0) << u32::from(subsampling_x)
    } else {
        i64::try_from(x).unwrap_or(0)
    };
    let luma_y = if plane > 0 {
        i64::try_from(y).unwrap_or(0) << u32::from(subsampling_y)
    } else {
        i64::try_from(y).unwrap_or(0)
    };
    let frac_x =
        ((16 * luma_x * x_scale) >> tables::REF_SCALE_SHIFT) & i64::from(tables::SUBPEL_MASK);
    let frac_y =
        ((16 * luma_y * y_scale) >> tables::REF_SCALE_SHIFT) & i64::from(tables::SUBPEL_MASK);
    let d_x = ((i64::from(clamped_mv[1]) * x_scale) >> tables::REF_SCALE_SHIFT) + frac_x;
    let d_y = ((i64::from(clamped_mv[0]) * y_scale) >> tables::REF_SCALE_SHIFT) + frac_y;
    ScaledMv {
        start_x: (base_x << tables::SUBPEL_BITS) + d_x,
        start_y: (base_y << tables::SUBPEL_BITS) + d_y,
        step_x: (16 * x_scale) >> tables::REF_SCALE_SHIFT,
        step_y: (16 * y_scale) >> tables::REF_SCALE_SHIFT,
    }
}

fn ref_plane(slot: &RefSlot, plane: usize) -> &Plane {
    match plane {
        0 => &slot.pic.y,
        1 => &slot.pic.u,
        _ => &slot.pic.v,
    }
}

/// §8.5.2.4's block inter prediction process. Writes `w*h` samples
/// (row-major) into `pred`.
#[allow(clippy::too_many_arguments, reason = "mirrors the spec's own inputs")]
#[allow(
    clippy::many_single_char_names,
    reason = "w/h/p/s/v/r/c/t are pixel coordinates, dimensions, filter-tap indices and accumulator values, matching the spec's own single-letter notation for this convolution"
)]
pub(crate) fn block_inter_predict(
    pred: &mut [i32],
    slot: &RefSlot,
    plane: usize,
    scaled: &ScaledMv,
    w: usize,
    h: usize,
    interp_filter: usize,
    bit_depth: u32,
) {
    let ref_p = ref_plane(slot, plane);
    let (sub_x, sub_y) = if plane == 0 {
        (false, false)
    } else {
        (slot.subsampling_x, slot.subsampling_y)
    };
    let last_x = (i64::from(slot.width) + i64::from(u32::from(sub_x))) >> u32::from(sub_x);
    let last_x = last_x - 1;
    let last_y = (i64::from(slot.height) + i64::from(u32::from(sub_y))) >> u32::from(sub_y);
    let last_y = last_y - 1;

    let intermediate_height =
        usize::try_from((((i64::try_from(h).unwrap_or(0) - 1) * scaled.step_y + 15) >> 4) + 8)
            .unwrap_or(0);
    let clip_max = (1i32 << bit_depth) - 1;

    let filters = tables::SUBPEL_FILTERS
        .get(interp_filter)
        .copied()
        .unwrap_or(tables::SUBPEL_FILTERS[0]);

    let mut intermediate = vec![0i32; intermediate_height * w];
    for r in 0..intermediate_height {
        for c in 0..w {
            let p = scaled.start_x + scaled.step_x * i64::try_from(c).unwrap_or(0);
            let phase = usize::try_from(p & 15).unwrap_or(0);
            let taps = filters
                .get(phase)
                .copied()
                .unwrap_or([0, 0, 0, 128, 0, 0, 0, 0]);
            let row = (scaled.start_y >> 4) + i64::try_from(r).unwrap_or(0) - 3;
            let row = row.clamp(0, last_y);
            let mut s = 0i64;
            for (t, &tap) in taps.iter().enumerate() {
                let col = (p >> 4) + i64::try_from(t).unwrap_or(0) - 3;
                let col = col.clamp(0, last_x);
                let sample = ref_p.get_clamped(
                    i32::try_from(col).unwrap_or(0),
                    i32::try_from(row).unwrap_or(0),
                );
                s += i64::from(tap) * i64::from(sample);
            }
            let v = i32::try_from((s + 64) >> 7).unwrap_or(0).clamp(0, clip_max);
            if let Some(slot) = intermediate.get_mut(r * w + c) {
                *slot = v;
            }
        }
    }

    for r in 0..h {
        for c in 0..w {
            let p = (scaled.start_y & 15) + scaled.step_y * i64::try_from(r).unwrap_or(0);
            let phase = usize::try_from(p & 15).unwrap_or(0);
            let taps = filters
                .get(phase)
                .copied()
                .unwrap_or([0, 0, 0, 128, 0, 0, 0, 0]);
            let mut s = 0i64;
            for (t, &tap) in taps.iter().enumerate() {
                let row = usize::try_from((p >> 4) + i64::try_from(t).unwrap_or(0)).unwrap_or(0);
                let sample = intermediate.get(row * w + c).copied().unwrap_or(0);
                s += i64::from(tap) * i64::from(sample);
            }
            let v = i32::try_from((s + 64) >> 7).unwrap_or(0).clamp(0, clip_max);
            if let Some(slot) = pred.get_mut(r * w + c) {
                *slot = v;
            }
        }
    }
}
