//! §8.8's in-loop deblocking filter (epic #32/C-32a). Runs once per frame,
//! after every superblock's tile decode has finished reconstructing
//! `CurrFrame` (§8.8's own "already-constructed portions of the current
//! frame referenced via intra prediction are not yet filtered" note is
//! exactly why this cannot interleave with `decode_partition`/`decode_block`
//! the way intra prediction does) and before the picture is either emitted
//! or written into the reference-frame store — a stream's later frames'
//! motion compensation reads the *filtered* picture, per §8.8's own first
//! NOTE ("the results of loop filtering are used in the prediction of
//! subsequent frames").
//!
//! This module is a free function operating on plain data
//! ([`Picture`]/[`MiInfo`]/[`LoopFilterParams`]/[`Segmentation`]) rather than
//! `crate::decode::FrameCtx`, matching `mvpred`/`interpredict`'s existing
//! shape: `crate::decode` builds the small per-mi-cell `MiInfo` grid this
//! module needs from its own richer `MiCell` grid and calls
//! [`filter_frame`] once, right before returning the finished picture.

use crate::decode::get_uv_tx_size;
use crate::framebuf::{Picture, Plane};
use crate::header::{LoopFilterParams, Segmentation};
use crate::tables;

/// Everything §8.8.2-8.8.4 read out of `MiSizes`/`TxSizes`/`Skips`/
/// `RefFrames[...][0]`/`YModes` at one grid position — deliberately not
/// `crate::decode::MiCell` itself (which also carries motion vectors and an
/// interpolation filter choice this process has no use for).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct MiInfo {
    pub mi_size: i32,
    pub tx_size: i32,
    pub skip: bool,
    pub ref_frame0: i32,
    pub y_mode: i32,
}

/// `LvlLookup[segment_id][ref][mode]`, §8.8.1's output.
type LvlLookup = [[[i32; 2]; tables::MAX_REF_FRAMES]; tables::MAX_SEGMENTS];

/// §8.8.1's loop filter frame init process.
///
/// The spec's own ordered steps 3-4 read:
/// > 3. If `loop_filter_delta_update` is equal to 0, then `LvlLookup[...]`
/// >    is set equal to `lvlSeg` for every `ref`/`mode`.
/// > 4. If `loop_filter_delta_enabled` is equal to 1, then \[the
/// >    per-ref/mode delta formula runs\].
///
/// Both conditions can be true at once (deltas enabled but not refreshed
/// this frame, so the previously-decoded `loop_filter_ref_deltas`/
/// `loop_filter_mode_deltas` persist) — in that case step 4 runs *after*
/// step 3 and unconditionally overwrites every entry step 3 just wrote, so
/// the two steps are only ever *observably* different when
/// `loop_filter_delta_enabled` is 0, and `loop_filter_delta_update` is only
/// ever read from the bitstream (hence only ever nonzero) when
/// `loop_filter_delta_enabled` is 1. Collapsing this into a plain
/// `if delta_enabled { step 4 } else { step 3 }` is therefore provably
/// equivalent to transcribing both ordered steps literally — checked by
/// hand against the spec text above, not against any reference decoder.
fn frame_init(lf: &LoopFilterParams, seg: &Segmentation) -> LvlLookup {
    let n_shift = lf.level >> 5;
    let mut lvl_lookup: LvlLookup = [[[0i32; 2]; tables::MAX_REF_FRAMES]; tables::MAX_SEGMENTS];
    let intra = usize::try_from(tables::INTRA_FRAME).unwrap_or(0);
    let last = usize::try_from(tables::LAST_FRAME).unwrap_or(1);
    for (segment_id, seg_lookup) in lvl_lookup.iter_mut().enumerate() {
        let mut lvl_seg = lf.level;
        let alt_l_active = seg.enabled
            && seg
                .feature_enabled
                .get(segment_id)
                .and_then(|r| r.get(tables::SEG_LVL_ALT_L))
                .copied()
                .unwrap_or(false);
        if alt_l_active {
            let alt_l = seg
                .feature_data
                .get(segment_id)
                .and_then(|r| r.get(tables::SEG_LVL_ALT_L))
                .copied()
                .unwrap_or(0);
            lvl_seg = if seg.abs_or_delta_update {
                alt_l
            } else {
                alt_l + lf.level
            };
            lvl_seg = lvl_seg.clamp(0, tables::MAX_LOOP_FILTER);
        }
        if !lf.delta_enabled {
            for r in seg_lookup.iter_mut() {
                for m in r.iter_mut() {
                    *m = lvl_seg;
                }
            }
            continue;
        }
        let intra_delta = lf.ref_deltas.get(intra).copied().unwrap_or(0);
        let intra_lvl = lvl_seg + (intra_delta << n_shift);
        if let Some(r) = seg_lookup.get_mut(intra)
            && let Some(m) = r.get_mut(0)
        {
            *m = intra_lvl.clamp(0, tables::MAX_LOOP_FILTER);
        }
        for ref_frame in last..tables::MAX_REF_FRAMES {
            let ref_delta = lf.ref_deltas.get(ref_frame).copied().unwrap_or(0);
            for mode in 0..tables::MAX_MODE_LF_DELTAS {
                let mode_delta = lf.mode_deltas.get(mode).copied().unwrap_or(0);
                let inter_lvl = lvl_seg + (ref_delta << n_shift) + (mode_delta << n_shift);
                if let Some(r) = seg_lookup.get_mut(ref_frame)
                    && let Some(m) = r.get_mut(mode)
                {
                    *m = inter_lvl.clamp(0, tables::MAX_LOOP_FILTER);
                }
            }
        }
    }
    lvl_lookup
}

/// The whole grid the filter reads (§8.8.2's `MiSizes`/`TxSizes`/`Skips`/
/// `RefFrames`/`YModes`, plus `SegmentIds`), sized `mi_rows * mi_cols`.
pub(crate) struct Grid<'a> {
    pub mi: &'a [MiInfo],
    pub segment_ids: &'a [u8],
    pub mi_rows: usize,
    pub mi_cols: usize,
}

impl Grid<'_> {
    fn at(&self, row: usize, col: usize) -> MiInfo {
        self.mi
            .get(row * self.mi_cols + col)
            .copied()
            .unwrap_or_default()
    }

    fn segment_id(&self, row: usize, col: usize) -> u8 {
        self.segment_ids
            .get(row * self.mi_cols + col)
            .copied()
            .unwrap_or(0)
    }
}

/// §8.8's top-level loop filter process: `frame_init` once, then every
/// superblock's vertical pass (both luma and chroma) before any
/// superblock's horizontal pass — the raster order the spec's outer loop
/// (`for row ... for col ... for plane ... for pass`) spells out, which its
/// own NOTE says "needs to be respected by any implementation" since later
/// edges read samples earlier edges already modified.
#[allow(
    clippy::too_many_arguments,
    reason = "mirrors the spec's own frame-level inputs: three planes, subsampling, bit depth, and the filter/segmentation parameter blocks"
)]
pub(crate) fn filter_frame(
    pic: &mut Picture,
    grid: &Grid<'_>,
    lf: &LoopFilterParams,
    seg: &Segmentation,
    subsampling_x: bool,
    subsampling_y: bool,
    bit_depth: u32,
) {
    if lf.level == 0 {
        return;
    }
    let lvl_lookup = frame_init(lf, seg);
    let mut row = 0usize;
    while row < grid.mi_rows {
        let mut col = 0usize;
        while col < grid.mi_cols {
            for plane in 0..3usize {
                for pass in 0..2usize {
                    superblock_filter(
                        pic,
                        grid,
                        &lvl_lookup,
                        lf.sharpness,
                        plane,
                        pass,
                        row,
                        col,
                        subsampling_x,
                        subsampling_y,
                        bit_depth,
                    );
                }
            }
            col += 8;
        }
        row += 8;
    }
}

/// §8.8.2's superblock loop filter process.
#[allow(
    clippy::too_many_arguments,
    reason = "mirrors the spec's own inputs: plane, pass, superblock position, subsampling, bit depth"
)]
fn superblock_filter(
    pic: &mut Picture,
    grid: &Grid<'_>,
    lvl_lookup: &LvlLookup,
    sharpness: i32,
    plane: usize,
    pass: usize,
    row: usize,
    col: usize,
    subsampling_x: bool,
    subsampling_y: bool,
    bit_depth: u32,
) {
    let (sub_x, sub_y) = if plane == 0 {
        (false, false)
    } else {
        (subsampling_x, subsampling_y)
    };
    let (dx, dy, sub, edge_len) = if pass == 0 {
        (1i32, 0i32, sub_x, 64usize >> u32::from(sub_y))
    } else {
        (0i32, 1i32, sub_y, 64usize >> u32::from(sub_x))
    };
    let edge_count = 16usize >> u32::from(sub);
    for edge in 0..edge_count {
        for i in 0..edge_len {
            let (x, y) = if pass == 0 {
                (
                    col * 8 + edge * (4 << u32::from(sub_x)),
                    row * 8 + (i << u32::from(sub_y)),
                )
            } else {
                (
                    col * 8 + (i << u32::from(sub_x)),
                    row * 8 + edge * (4 << u32::from(sub_y)),
                )
            };
            let loop_col = ((x >> 3) >> u32::from(sub_x)) << u32::from(sub_x);
            let loop_row = ((y >> 3) >> u32::from(sub_y)) << u32::from(sub_y);
            if loop_row >= grid.mi_rows || loop_col >= grid.mi_cols {
                continue;
            }
            let info = grid.at(loop_row, loop_col);
            let tx_sz = if plane > 0 {
                get_uv_tx_size(info.mi_size, info.tx_size, subsampling_x, subsampling_y)
            } else {
                info.tx_size
            };
            let sb_size = if sub {
                info.mi_size.max(tables::BLOCK_16X16)
            } else {
                info.mi_size
            };
            let is_intra = info.ref_frame0 <= tables::INTRA_FRAME;

            let is_block_edge = if pass == 0 {
                let w = 8 * tables::NUM_8X8_BLOCKS_WIDE_LOOKUP
                    .get(usize::try_from(sb_size).unwrap_or(0))
                    .copied()
                    .unwrap_or(1);
                w > 0 && x % w == 0
            } else {
                let h = 8 * tables::NUM_8X8_BLOCKS_HIGH_LOOKUP
                    .get(usize::try_from(sb_size).unwrap_or(0))
                    .copied()
                    .unwrap_or(1);
                h > 0 && y % h == 0
            };
            // §8.8.2 step 11's chroma-right-edge special case: a horizontal
            // boundary that would land exactly on the right-hand image edge
            // for a subsampled plane is not a real transform edge.
            let odd_cols = grid.mi_cols % 2 == 1;
            let chroma_right_edge =
                pass == 1 && sub_x && odd_cols && edge % 2 == 1 && x + 8 >= grid.mi_cols * 8;
            let is_tx_edge = if chroma_right_edge {
                false
            } else {
                tx_sz >= 0 && edge % (1usize << tx_sz) == 0
            };
            let is_32_edge = edge % 8 == 0;

            let on_screen = !(x >= 8 * grid.mi_cols
                || y >= 8 * grid.mi_rows
                || (pass == 0 && x == 0)
                || (pass == 1 && y == 0));
            let apply_filter = on_screen
                && (is_block_edge || (is_tx_edge && is_intra) || (is_tx_edge && !info.skip));
            if !apply_filter {
                continue;
            }

            let filter_size = filter_size(
                tx_sz,
                is_32_edge,
                pass,
                x,
                y,
                sub_x,
                sub_y,
                grid.mi_cols,
                grid.mi_rows,
            );
            let (lvl, limit, blimit, thresh) = adaptive_strength(
                lvl_lookup,
                sharpness,
                grid.segment_id(loop_row, loop_col),
                info,
            );
            if lvl > 0 {
                let px = x >> u32::from(sub_x);
                let py = y >> u32::from(sub_y);
                filter_sample(
                    pic,
                    plane,
                    px,
                    py,
                    dx,
                    dy,
                    limit,
                    blimit,
                    thresh,
                    filter_size,
                    bit_depth,
                );
            }
        }
    }
}

/// §8.8.3's filter size process.
#[allow(clippy::too_many_arguments, reason = "mirrors the spec's own inputs")]
fn filter_size(
    tx_sz: i32,
    is_32_edge: bool,
    pass: usize,
    x: usize,
    y: usize,
    sub_x: bool,
    sub_y: bool,
    mi_cols: usize,
    mi_rows: usize,
) -> i32 {
    let base_size = if tx_sz == tables::TX_4X4 && is_32_edge {
        tables::TX_8X8
    } else {
        tx_sz.min(tables::TX_16X16)
    };
    let at_right_edge = pass == 0 && sub_x && (x >> 3) == mi_cols.saturating_sub(1);
    let at_bottom_edge = pass == 1 && sub_y && (y >> 3) == mi_rows.saturating_sub(1);
    if base_size == tables::TX_16X16 && (at_right_edge || at_bottom_edge) {
        tables::TX_8X8
    } else {
        base_size
    }
}

/// §8.8.4's adaptive filter strength process. Returns `(lvl, limit, blimit,
/// thresh)`.
fn adaptive_strength(
    lvl_lookup: &LvlLookup,
    sharpness: i32,
    segment_id: u8,
    info: MiInfo,
) -> (i32, i32, i32, i32) {
    let mode_type = i32::from(matches!(
        info.y_mode,
        tables::NEARESTMV | tables::NEARMV | tables::NEWMV
    ));
    let ref_frame = usize::try_from(info.ref_frame0).unwrap_or(0);
    let lvl = lvl_lookup
        .get(usize::from(segment_id))
        .and_then(|r| r.get(ref_frame))
        .and_then(|r| r.get(usize::try_from(mode_type).unwrap_or(0)))
        .copied()
        .unwrap_or(0);
    let shift = if sharpness > 4 {
        2
    } else {
        i32::from(sharpness > 0)
    };
    let limit = if sharpness > 0 {
        (lvl >> shift).clamp(1, 9 - sharpness)
    } else {
        (lvl >> shift).max(1)
    };
    let blimit = 2 * (lvl + 2) + limit;
    let thresh = lvl >> 4;
    (lvl, limit, blimit, thresh)
}

/// §8.8.5's sample filtering process: the filter mask, then whichever of
/// the narrow/wide filters the masks and `filterSize` select.
#[allow(clippy::too_many_arguments, reason = "mirrors the spec's own inputs")]
#[allow(
    clippy::integer_division,
    reason = "spec-defined: §8.8.5.1's filterMask formula is `Abs(p0-q0)*2 + Abs(p1-q1)/2 > blimitBd`, an exact integer division"
)]
fn filter_sample(
    pic: &mut Picture,
    plane: usize,
    x: usize,
    y: usize,
    dx: i32,
    dy: i32,
    limit: i32,
    blimit: i32,
    thresh: i32,
    filter_size: i32,
    bit_depth: u32,
) {
    let p = plane_ref(pic, plane);
    let sample = |i: i32| {
        i32::from(p.get_clamped(
            i32::try_from(x).unwrap_or(0) + i * dx,
            i32::try_from(y).unwrap_or(0) + i * dy,
        ))
    };
    let (q0, q1, q2, q3) = (sample(0), sample(1), sample(2), sample(3));
    let (p0, p1, p2, p3) = (sample(-1), sample(-2), sample(-3), sample(-4));

    let shift_bd = bit_depth.saturating_sub(8);
    let thresh_bd = thresh << shift_bd;
    let hev_mask = (p1 - p0).abs() > thresh_bd || (q1 - q0).abs() > thresh_bd;

    let limit_bd = limit << shift_bd;
    let blimit_bd = blimit << shift_bd;
    let mut mask = (p3 - p2).abs() > limit_bd;
    mask |= (p2 - p1).abs() > limit_bd;
    mask |= (p1 - p0).abs() > limit_bd;
    mask |= (q1 - q0).abs() > limit_bd;
    mask |= (q2 - q1).abs() > limit_bd;
    mask |= (q3 - q2).abs() > limit_bd;
    mask |= (p0 - q0).abs() * 2 + (p1 - q1).abs() / 2 > blimit_bd;
    let filter_mask = !mask;
    if !filter_mask {
        return;
    }

    let threshold_bd = 1i32 << shift_bd;
    let flat_mask = filter_size >= tables::TX_8X8 && {
        let mut m = (p1 - p0).abs() > threshold_bd;
        m |= (q1 - q0).abs() > threshold_bd;
        m |= (p2 - p0).abs() > threshold_bd;
        m |= (q2 - q0).abs() > threshold_bd;
        m |= (p3 - p0).abs() > threshold_bd;
        m |= (q3 - q0).abs() > threshold_bd;
        !m
    };

    if filter_size == tables::TX_4X4 || !flat_mask {
        narrow_filter(pic, plane, x, y, dx, dy, hev_mask, bit_depth);
        return;
    }

    let flat_mask2 = filter_size >= tables::TX_16X16 && {
        let p = plane_ref(pic, plane);
        let sample = |i: i32| {
            i32::from(p.get_clamped(
                i32::try_from(x).unwrap_or(0) + i * dx,
                i32::try_from(y).unwrap_or(0) + i * dy,
            ))
        };
        let (q4, q5, q6, q7) = (sample(4), sample(5), sample(6), sample(7));
        let (p4, p5, p6, p7) = (sample(-5), sample(-6), sample(-7), sample(-8));
        let mut m = (p7 - p0).abs() > threshold_bd;
        m |= (q7 - q0).abs() > threshold_bd;
        m |= (p6 - p0).abs() > threshold_bd;
        m |= (q6 - q0).abs() > threshold_bd;
        m |= (p5 - p0).abs() > threshold_bd;
        m |= (q5 - q0).abs() > threshold_bd;
        m |= (p4 - p0).abs() > threshold_bd;
        m |= (q4 - q0).abs() > threshold_bd;
        !m
    };

    if filter_size == tables::TX_8X8 || !flat_mask2 {
        wide_filter(pic, plane, x, y, dx, dy, 3, bit_depth);
    } else {
        wide_filter(pic, plane, x, y, dx, dy, 4, bit_depth);
    }
}

fn plane_ref(pic: &Picture, plane: usize) -> &Plane {
    match plane {
        0 => &pic.y,
        1 => &pic.u,
        _ => &pic.v,
    }
}

fn plane_mut(pic: &mut Picture, plane: usize) -> &mut Plane {
    match plane {
        0 => &mut pic.y,
        1 => &mut pic.u,
        _ => &mut pic.v,
    }
}

fn filter4_clamp(value: i32, bit_depth: u32) -> i32 {
    value.clamp(-(1i32 << (bit_depth - 1)), (1i32 << (bit_depth - 1)) - 1)
}

/// §8.8.5.2's narrow filter process.
fn narrow_filter(
    pic: &mut Picture,
    plane: usize,
    x: usize,
    y: usize,
    dx: i32,
    dy: i32,
    hev_mask: bool,
    bit_depth: u32,
) {
    let ix = i32::try_from(x).unwrap_or(0);
    let iy = i32::try_from(y).unwrap_or(0);
    let p = plane_ref(pic, plane);
    let sample = |i: i32| i32::from(p.get_clamped(ix + i * dx, iy + i * dy));
    let (q0, q1) = (sample(0), sample(1));
    let (p0, p1) = (sample(-1), sample(-2));

    let bias = 0x80i32 << (bit_depth - 8);
    let (ps1, ps0, qs0, qs1) = (p1 - bias, p0 - bias, q0 - bias, q1 - bias);

    let mut filter = if hev_mask {
        filter4_clamp(ps1 - qs1, bit_depth)
    } else {
        0
    };
    filter = filter4_clamp(filter + 3 * (qs0 - ps0), bit_depth);
    let filter1 = filter4_clamp(filter + 4, bit_depth) >> 3;
    let filter2 = filter4_clamp(filter + 3, bit_depth) >> 3;
    let oq0 = filter4_clamp(qs0 - filter1, bit_depth) + bias;
    let op0 = filter4_clamp(ps0 + filter2, bit_depth) + bias;

    let max = (1i32 << bit_depth) - 1;
    let put = |pic: &mut Picture, x: i32, y: i32, v: i32| {
        let Ok(ux) = usize::try_from(x) else { return };
        let Ok(uy) = usize::try_from(y) else { return };
        plane_mut(pic, plane).set(ux, uy, u16::try_from(v.clamp(0, max)).unwrap_or(0));
    };
    put(pic, ix, iy, oq0);
    put(pic, ix - dx, iy - dy, op0);
    if !hev_mask {
        let round1 = |v: i32| (v + 1) >> 1;
        let filter = round1(filter1);
        let oq1 = filter4_clamp(qs1 - filter, bit_depth) + bias;
        let op1 = filter4_clamp(ps1 + filter, bit_depth) + bias;
        put(pic, ix + dx, iy + dy, oq1);
        put(pic, ix - 2 * dx, iy - 2 * dy, op1);
    }
}

/// §8.8.5.3's wide filter process. `log2Size` is 3 (modifies 3 samples each
/// side) or 4 (7 samples each side).
#[allow(
    clippy::too_many_arguments,
    reason = "mirrors the spec's own inputs plus the bit depth its output clamp needs"
)]
#[allow(
    clippy::many_single_char_names,
    reason = "x/y/n/i/j/p/t are pixel coordinates, tap-window bounds and loop/accumulator variables matching the spec's own single-letter notation for this box filter"
)]
fn wide_filter(
    pic: &mut Picture,
    plane: usize,
    x: usize,
    y: usize,
    dx: i32,
    dy: i32,
    log2_size: u32,
    bit_depth: u32,
) {
    let ix = i32::try_from(x).unwrap_or(0);
    let iy = i32::try_from(y).unwrap_or(0);
    let n = (1i32 << (log2_size - 1)) - 1;
    let mut out = [0i32; 15];
    {
        let p = plane_ref(pic, plane);
        let sample = |k: i32| i32::from(p.get_clamped(ix + k * dx, iy + k * dy));
        let mut i = -n;
        while i < n {
            let mut t = sample(i);
            let mut j = -n;
            while j <= n {
                let clamped = (i + j).clamp(-(n + 1), n);
                t += sample(clamped);
                j += 1;
            }
            let rounded = (t + (1 << (log2_size - 1))) >> log2_size;
            if let Some(slot) = out.get_mut(usize::try_from(i + n).unwrap_or(0)) {
                *slot = rounded;
            }
            i += 1;
        }
    }
    let max = (1i32 << bit_depth) - 1;
    let dst = plane_mut(pic, plane);
    let mut i = -n;
    while i < n {
        let v = out
            .get(usize::try_from(i + n).unwrap_or(0))
            .copied()
            .unwrap_or(0);
        if let (Ok(ux), Ok(uy)) = (usize::try_from(ix + i * dx), usize::try_from(iy + i * dy)) {
            dst.set(ux, uy, u16::try_from(v.clamp(0, max)).unwrap_or(0));
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::{LvlLookup, MiInfo, adaptive_strength, filter_size, frame_init};
    use crate::header::{LoopFilterParams, Segmentation};
    use crate::tables;

    /// Reads `lookup[segment][ref][mode]` via `.get()`, panicking on a
    /// hand-picked-wrong test index rather than risking `indexing_slicing`
    /// on the type under test.
    fn at(lookup: &LvlLookup, segment: usize, ref_frame: usize, mode: usize) -> i32 {
        // `i32::MIN` as the "index was out of bounds" sentinel: every real
        // `LvlLookup` entry is `Clip3(0, MAX_LOOP_FILTER, ...)`-bounded, so
        // this can never collide with a genuine value and a bad test index
        // still fails loudly via the surrounding `assert_eq!` mismatch.
        lookup
            .get(segment)
            .and_then(|s| s.get(ref_frame))
            .and_then(|r| r.get(mode))
            .copied()
            .unwrap_or(i32::MIN)
    }

    /// §8.8.1 with `loop_filter_delta_enabled == false`: every `(ref, mode)`
    /// entry for a segment collapses to that segment's own `lvlSeg` (here,
    /// plain `loop_filter_level` since no segment has `SEG_LVL_ALT_L`
    /// active) — hand-derived directly from the spec's step 3, not from
    /// running the code under test.
    #[test]
    fn frame_init_without_deltas_is_uniform_per_segment() {
        let lf = LoopFilterParams {
            level: 20,
            sharpness: 0,
            delta_enabled: false,
            ref_deltas: [9, 9, 9, 9],
            mode_deltas: [9, 9],
        };
        let seg = Segmentation::default();
        let lookup = frame_init(&lf, &seg);
        for segment in &lookup {
            for r in segment {
                for &m in r {
                    assert_eq!(m, 20);
                }
            }
        }
    }

    /// §8.8.1 with `loop_filter_delta_enabled == true`: hand-computed from
    /// the spec's step 4 formula directly —
    /// `intraLvl = lvlSeg + (ref_deltas[INTRA_FRAME] << nShift)`,
    /// `interLvl = lvlSeg + (ref_deltas[ref] << nShift) + (mode_deltas[mode] << nShift)`,
    /// both `Clip3(0, 63, ...)` — for `level = 40` (`nShift = 40 >> 5 = 1`).
    #[test]
    fn frame_init_with_deltas_matches_the_spec_formula() {
        let lf = LoopFilterParams {
            level: 40,
            sharpness: 0,
            delta_enabled: true,
            ref_deltas: [3, -2, 1, 0],
            mode_deltas: [-1, 4],
        };
        let seg = Segmentation::default();
        let lookup = frame_init(&lf, &seg);
        let n_shift = 1; // 40 >> 5
        // INTRA_FRAME only ever has mode 0 looked up (an intra y_mode never
        // gives modeType 1), so only [INTRA_FRAME][0] is spec-defined.
        assert_eq!(at(&lookup, 0, 0, 0), 40 + (3 << n_shift));
        // LAST_FRAME (ref=1), mode 0 and 1.
        assert_eq!(at(&lookup, 0, 1, 0), 40 + (-2 << n_shift) + (-1 << n_shift));
        assert_eq!(at(&lookup, 0, 1, 1), 40 + (-2 << n_shift) + (4 << n_shift));
        // GOLDEN_FRAME (ref=2), mode 0.
        assert_eq!(at(&lookup, 0, 2, 0), 40 + (1 << n_shift) + (-1 << n_shift));
        // ALTREF_FRAME (ref=3), mode 1.
        assert_eq!(at(&lookup, 0, 3, 1), 40 + (0 << n_shift) + (4 << n_shift));
    }

    /// §8.8.1 step 2's `SEG_LVL_ALT_L`, absolute-update case
    /// (`segmentation_abs_or_delta_update == 1`): `lvlSeg` becomes the
    /// segment's `FeatureData` value directly, not added to the frame
    /// level.
    #[test]
    fn seg_lvl_alt_l_absolute_replaces_the_frame_level() {
        let lf = LoopFilterParams {
            level: 50,
            sharpness: 0,
            delta_enabled: false,
            ref_deltas: [0; 4],
            mode_deltas: [0; 2],
        };
        let mut seg = Segmentation {
            enabled: true,
            abs_or_delta_update: true,
            ..Segmentation::default()
        };
        if let Some(row) = seg.feature_enabled.get_mut(2) {
            row[tables::SEG_LVL_ALT_L] = true;
        }
        if let Some(row) = seg.feature_data.get_mut(2) {
            row[tables::SEG_LVL_ALT_L] = 12;
        }
        let lookup = frame_init(&lf, &seg);
        assert_eq!(at(&lookup, 2, 0, 0), 12);
        // A segment without the feature enabled is untouched.
        assert_eq!(at(&lookup, 0, 0, 0), 50);
    }

    /// The same feature in delta mode (`segmentation_abs_or_delta_update ==
    /// 0`): `lvlSeg = FeatureData + loop_filter_level`, clamped to
    /// `MAX_LOOP_FILTER` — chosen here specifically to exercise the clamp.
    #[test]
    fn seg_lvl_alt_l_delta_adds_to_the_frame_level_and_clamps() {
        let lf = LoopFilterParams {
            level: 60,
            sharpness: 0,
            delta_enabled: false,
            ref_deltas: [0; 4],
            mode_deltas: [0; 2],
        };
        let mut seg = Segmentation {
            enabled: true,
            abs_or_delta_update: false,
            ..Segmentation::default()
        };
        if let Some(row) = seg.feature_enabled.get_mut(1) {
            row[tables::SEG_LVL_ALT_L] = true;
        }
        if let Some(row) = seg.feature_data.get_mut(1) {
            row[tables::SEG_LVL_ALT_L] = 20; // 60 + 20 = 80, clamps to 63.
        }
        let lookup = frame_init(&lf, &seg);
        assert_eq!(at(&lookup, 1, 0, 0), tables::MAX_LOOP_FILTER);
    }

    /// §8.8.4's `modeType`/`shift`/`limit`/`blimit`/`thresh`, hand-computed
    /// for `sharpness == 0` (the `Max(1, lvl >> shift)` branch) and a
    /// `NEARESTMV` block (`modeType == 1`).
    #[test]
    fn adaptive_strength_matches_the_spec_formula_at_zero_sharpness() {
        let mut lookup: LvlLookup = [[[0i32; 2]; tables::MAX_REF_FRAMES]; tables::MAX_SEGMENTS];
        if let Some(v) = lookup
            .get_mut(0)
            .and_then(|s| s.get_mut(1))
            .and_then(|r| r.get_mut(1))
        {
            *v = 16; // segment 0, LAST_FRAME, modeType 1.
        }
        let info = MiInfo {
            mi_size: tables::BLOCK_8X8,
            tx_size: tables::TX_8X8,
            skip: false,
            ref_frame0: tables::LAST_FRAME,
            y_mode: tables::NEARESTMV,
        };
        let (lvl, limit, blimit, thresh) = adaptive_strength(&lookup, 0, 0, info);
        assert_eq!(lvl, 16);
        assert_eq!(limit, 16); // Max(1, 16 >> 0)
        assert_eq!(blimit, 2 * (16 + 2) + 16); // 52
        assert_eq!(thresh, 1); // 16 >> 4
    }

    /// The `sharpness > 0` branch (`Clip3(1, 9 - sharpness, lvl >> shift)`)
    /// and an intra block (`modeType == 0` regardless of `y_mode`).
    #[test]
    fn adaptive_strength_clips_the_limit_when_sharpness_is_set() {
        let mut lookup: LvlLookup = [[[0i32; 2]; tables::MAX_REF_FRAMES]; tables::MAX_SEGMENTS];
        if let Some(v) = lookup
            .get_mut(0)
            .and_then(|s| s.get_mut(0))
            .and_then(|r| r.get_mut(0))
        {
            *v = 40; // segment 0, INTRA_FRAME, modeType 0.
        }
        let info = MiInfo {
            mi_size: tables::BLOCK_8X8,
            tx_size: tables::TX_8X8,
            skip: false,
            ref_frame0: tables::INTRA_FRAME,
            y_mode: tables::DC_PRED,
        };
        // sharpness = 6 (> 4): shift = 2, lvl >> shift = 10, but Clip3(1, 9-6=3, 10) = 3.
        let (lvl, limit, _, _) = adaptive_strength(&lookup, 6, 0, info);
        assert_eq!(lvl, 40);
        assert_eq!(limit, 3);
    }

    /// §8.8.3's filter size process: the `TX_4X4`-on-a-32-boundary
    /// promotion to `TX_8X8`, and the chroma-right/bottom-edge demotion
    /// back to `TX_8X8` for what would otherwise be `TX_16X16`.
    #[test]
    fn filter_size_promotes_and_demotes_per_the_spec() {
        // TX_4X4 on a 32-sample boundary promotes to TX_8X8.
        assert_eq!(
            filter_size(tables::TX_4X4, true, 0, 64, 0, false, false, 10, 10),
            tables::TX_8X8
        );
        // TX_4X4 off a 32-sample boundary stays TX_4X4.
        assert_eq!(
            filter_size(tables::TX_4X4, false, 0, 64, 0, false, false, 10, 10),
            tables::TX_4X4
        );
        // TX_16X16 vertical pass at the frame's last mi column, subsampled:
        // demoted to TX_8X8 so the chroma filter never reads past the edge.
        assert_eq!(
            filter_size(tables::TX_16X16, false, 0, 72, 0, true, false, 10, 10),
            tables::TX_8X8
        );
        // The same position without chroma subsampling is unaffected.
        assert_eq!(
            filter_size(tables::TX_16X16, false, 0, 72, 0, false, false, 10, 10),
            tables::TX_16X16
        );
    }
}
