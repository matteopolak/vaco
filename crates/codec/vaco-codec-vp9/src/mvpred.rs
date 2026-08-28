//! §6.5's motion vector prediction (`find_mv_refs`/`find_best_ref_mvs`/
//! `append_sub8x8_mvs` and everything they call), plus §6.4.19/6.4.20's
//! `read_mv`/`read_mv_component` (grouped here rather than in `decode.rs`
//! since both need the same `UseHp`/`BestMv` machinery).
//!
//! This is "part of the syntax" per the spec's own §6.5 preamble, not a
//! post-processing step: `read_mv`'s `UseHp` depends on `BestMv`, which
//! only exists once `find_mv_refs`/`find_best_ref_mvs` have run, so motion
//! vector prediction has to happen *before* the bitstream can even be
//! parsed for a `NEWMV` block, not after.

use vaco_codec_msac::Vp9BoolDecoder as Bd;

use crate::header::EntropyContext;
use crate::tables;

/// One 8x8 grid cell's motion-vector-prediction-relevant state: enough of
/// `RefFrames`/`Mvs`/`SubMvs`/`YModes` (current frame) or
/// `PrevRefFrames`/`PrevMvs` (previous frame) to serve every neighbour
/// candidate `find_mv_refs` looks at. The previous frame never needs
/// `sub_mvs`/`y_mode` (the temporal candidate only ever calls
/// `get_block_mv`, never `get_sub_block_mv`, and never contributes to
/// `mode_2_counter`), but keeping one shape for both avoids a second,
/// near-identical struct.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MvCell {
    pub y_mode: i32,
    pub ref_frame: [i32; 2],
    /// `Mv[refList]`, `[row, col]`.
    pub mv: [[i32; 2]; 2],
    /// `BlockMvs[refList][subblock]`, `[row, col]`.
    pub sub_mvs: [[[i32; 2]; 4]; 2],
}

pub(crate) struct MvRefContext<'a> {
    pub mi_row: usize,
    pub mi_col: usize,
    pub mi_rows: usize,
    pub mi_cols: usize,
    pub mi_col_start: usize,
    pub mi_col_end: usize,
    /// Reads one current-frame grid cell (already-decoded blocks only —
    /// spec-guaranteed since every neighbour `find_mv_refs` looks at is
    /// either above or to the left, or the block itself for `usePrev`).
    /// A closure rather than a borrowed slice: the grid is a `Vec<MiCell>`
    /// still being written block-by-block by the caller, and `MiCell`
    /// carries several fields (`skip`/`tx_size`/...) this module has no
    /// business knowing about, so a full-grid `Vec<MvCell>` copy — stale
    /// the moment the next block finishes — is both wasteful and wrong.
    pub cell_at: &'a dyn Fn(i32, i32) -> Option<MvCell>,
    pub use_prev_frame_mvs: bool,
    /// Reads one previous-frame grid cell at the *current* block's own
    /// `(MiRow, MiCol)` (§6.5.10's `usePrev` case always reads at the
    /// current position, never a neighbour's).
    pub prev_cell: &'a dyn Fn() -> MvCell,
    pub ref_frame_sign_bias: [bool; 4],
}

impl MvRefContext<'_> {
    /// §6.5.2's `is_inside`.
    fn is_inside(&self, r: i32, c: i32) -> bool {
        let Ok(r) = usize::try_from(r) else { return false };
        r < self.mi_rows && c >= i32::try_from(self.mi_col_start).unwrap_or(0) && c < i32::try_from(self.mi_col_end).unwrap_or(0)
    }

    fn sign_bias(&self, rf: i32) -> bool {
        self.ref_frame_sign_bias.get(usize::try_from(rf).unwrap_or(0)).copied().unwrap_or(false)
    }
}

fn add_mv_ref_list(list: &mut [[i32; 2]; 2], count: &mut usize, mv: [i32; 2]) {
    if *count >= 2 {
        return;
    }
    if *count > 0 && list.first().is_some_and(|&first| mv == first) {
        return;
    }
    if let Some(slot) = list.get_mut(*count) {
        *slot = mv;
        *count += 1;
    }
}

/// §6.5.11's `get_sub_block_mv`.
fn get_sub_block_mv(cell: MvCell, ref_list: usize, delta_col: i32, block: i32) -> [i32; 2] {
    let idx = if block >= 0 {
        let col_is_zero = usize::from(delta_col == 0);
        tables::IDX_N_COLUMN_TO_SUBBLOCK.get(usize::try_from(block).unwrap_or(0)).and_then(|r| r.get(col_is_zero)).copied().unwrap_or(3)
    } else {
        3
    };
    cell.sub_mvs.get(ref_list).and_then(|r| r.get(idx)).copied().unwrap_or([0, 0])
}

/// §6.5.10's `get_block_mv`.
fn get_block_mv(cell: MvCell, ref_list: usize) -> ([i32; 2], i32) {
    (cell.mv.get(ref_list).copied().unwrap_or([0, 0]), cell.ref_frame.get(ref_list).copied().unwrap_or(0))
}

/// §6.5.9's `scale_mv`.
fn scale_mv(ctx: &MvRefContext<'_>, mut mv: [i32; 2], cand_frame: i32, ref_frame: i32) -> [i32; 2] {
    if ctx.sign_bias(cand_frame) != ctx.sign_bias(ref_frame) {
        mv[0] = -mv[0];
        mv[1] = -mv[1];
    }
    mv
}

/// §6.5.7's `if_same_ref_frame_add_mv`.
fn if_same_ref_frame_add_mv(cell: MvCell, ref_frame: i32, list: &mut [[i32; 2]; 2], count: &mut usize) {
    for j in 0..2 {
        let (mv, cand_frame) = get_block_mv(cell, j);
        if cand_frame == ref_frame {
            add_mv_ref_list(list, count, mv);
            return;
        }
    }
}

/// §6.5.8's `if_diff_ref_frame_add_mv`.
fn if_diff_ref_frame_add_mv(ctx: &MvRefContext<'_>, cell: MvCell, ref_frame: i32, list: &mut [[i32; 2]; 2], count: &mut usize) {
    let (mv0, frame0) = get_block_mv(cell, 0);
    let (mv1, frame1) = get_block_mv(cell, 1);
    let mvs_same = mv0 == mv1;
    if frame0 > tables::INTRA_FRAME && frame0 != ref_frame {
        add_mv_ref_list(list, count, scale_mv(ctx, mv0, frame0, ref_frame));
    }
    if frame1 > tables::INTRA_FRAME && frame1 != ref_frame && !mvs_same {
        add_mv_ref_list(list, count, scale_mv(ctx, mv1, frame1, ref_frame));
    }
}

/// §6.5.3-6.5.5's `clamp_mv_ref`/`clamp_mv_row`/`clamp_mv_col`.
// Both functions below deliberately use signed subtraction for
// `MiRows - bh - MiRow` / `MiCols - bw - MiCol`, matching §6.5.4/6.5.5's own
// arithmetic exactly: for an edge block whose nominal size overhangs the
// frame (`bh`/`bw` bigger than the mi units actually remaining — the common
// case whenever a frame's height/width is not an exact multiple of the
// block-size grid, e.g. this crate's own 176x144 fixtures, mi_rows=18),
// this is genuinely *negative*. Clamping it to 0 first (`saturating_sub`,
// this function's original — and wrong — approach) silently narrows
// `mbToBottomEdge`/`mbToRightEdge`, which shrinks the legal clamp range and
// can force a boundary block's motion vector short of where the true
// encoder placed it.
fn clamp_mv_row(mv_row: i32, border: i32, mi_row: usize, mi_rows: usize, bh: usize) -> i32 {
    let mb_to_top = -(i32::try_from(mi_row).unwrap_or(0) * tables::MI_SIZE * 8);
    let mb_to_bottom = (i32::try_from(mi_rows).unwrap_or(0) - i32::try_from(bh).unwrap_or(0) - i32::try_from(mi_row).unwrap_or(0)) * tables::MI_SIZE * 8;
    mv_row.clamp(mb_to_top - border, mb_to_bottom + border)
}

fn clamp_mv_col(mv_col: i32, border: i32, mi_col: usize, mi_cols: usize, bw: usize) -> i32 {
    let mb_to_left = -(i32::try_from(mi_col).unwrap_or(0) * tables::MI_SIZE * 8);
    let mb_to_right = (i32::try_from(mi_cols).unwrap_or(0) - i32::try_from(bw).unwrap_or(0) - i32::try_from(mi_col).unwrap_or(0)) * tables::MI_SIZE * 8;
    mv_col.clamp(mb_to_left - border, mb_to_right + border)
}

/// §6.5.1's `find_mv_refs`. Returns `(RefListMv, ModeContext)` — `RefListMv`
/// always has exactly 2 entries (spec-guaranteed: unfilled slots stay
/// `ZeroMv`), `ModeContext` only meaningful when `block < 0` (a sub-8x8
/// call's `ModeContext` is never read — `inter_mode` for the whole block is
/// decoded once, before any `append_sub8x8_mvs` call).
#[allow(clippy::too_many_arguments, reason = "mirrors the spec's own find_mv_refs(refFrame, block) plus the block-size/border inputs Rust can't read off implicit globals")]
pub(crate) fn find_mv_refs(ctx: &MvRefContext<'_>, mi_size: i32, bw: usize, bh: usize, ref_frame: i32, block: i32) -> ([[i32; 2]; 2], i32) {
    let mut ref_list_mv = [[0i32; 2]; 2];
    let mut ref_mv_count = 0usize;
    let mut different_ref_found = false;
    let mut context_counter = 0i32;

    let search = tables::MV_REF_BLOCKS.get(usize::try_from(mi_size).unwrap_or(0)).copied().unwrap_or([[0; 2]; tables::MVREF_NEIGHBOURS]);

    for i in 0..2 {
        let Some(&[dr, dc]) = search.get(i) else { continue };
        let (cr, cc) = (i32::try_from(ctx.mi_row).unwrap_or(0) + dr, i32::try_from(ctx.mi_col).unwrap_or(0) + dc);
        if ctx.is_inside(cr, cc)
            && let Some(cell) = (ctx.cell_at)(cr, cc)
        {
            different_ref_found = true;
            context_counter += tables::MODE_2_COUNTER.get(usize::try_from(cell.y_mode).unwrap_or(0)).copied().unwrap_or(0);
            for j in 0..2 {
                if cell.ref_frame.get(j).copied().unwrap_or(0) == ref_frame {
                    let mv = get_sub_block_mv(cell, j, dc, block);
                    add_mv_ref_list(&mut ref_list_mv, &mut ref_mv_count, mv);
                    break;
                }
            }
        }
    }
    for i in 2..tables::MVREF_NEIGHBOURS {
        let Some(&[dr, dc]) = search.get(i) else { continue };
        let (cr, cc) = (i32::try_from(ctx.mi_row).unwrap_or(0) + dr, i32::try_from(ctx.mi_col).unwrap_or(0) + dc);
        if ctx.is_inside(cr, cc)
            && let Some(cell) = (ctx.cell_at)(cr, cc)
        {
            different_ref_found = true;
            if_same_ref_frame_add_mv(cell, ref_frame, &mut ref_list_mv, &mut ref_mv_count);
        }
    }
    if ctx.use_prev_frame_mvs {
        let cell = (ctx.prev_cell)();
        if_same_ref_frame_add_mv(cell, ref_frame, &mut ref_list_mv, &mut ref_mv_count);
    }
    if different_ref_found {
        for i in 0..tables::MVREF_NEIGHBOURS {
            let Some(&[dr, dc]) = search.get(i) else { continue };
            let (cr, cc) = (i32::try_from(ctx.mi_row).unwrap_or(0) + dr, i32::try_from(ctx.mi_col).unwrap_or(0) + dc);
            if ctx.is_inside(cr, cc)
                && let Some(cell) = (ctx.cell_at)(cr, cc)
            {
                if_diff_ref_frame_add_mv(ctx, cell, ref_frame, &mut ref_list_mv, &mut ref_mv_count);
            }
        }
    }
    if ctx.use_prev_frame_mvs {
        let cell = (ctx.prev_cell)();
        if_diff_ref_frame_add_mv(ctx, cell, ref_frame, &mut ref_list_mv, &mut ref_mv_count);
    }
    let mode_context = tables::COUNTER_TO_CONTEXT.get(usize::try_from(context_counter).unwrap_or(0)).copied().unwrap_or(tables::INVALID_CASE);
    for slot in &mut ref_list_mv {
        slot[0] = clamp_mv_row(slot[0], tables::MV_BORDER, ctx.mi_row, ctx.mi_rows, bh);
        slot[1] = clamp_mv_col(slot[1], tables::MV_BORDER, ctx.mi_col, ctx.mi_cols, bw);
    }
    (ref_list_mv, mode_context)
}

/// §6.5.13's `use_mv_hp`.
fn use_mv_hp(mv: [i32; 2]) -> bool {
    (mv[0].abs() >> 3) < tables::COMPANDED_MVREF_THRESH && (mv[1].abs() >> 3) < tables::COMPANDED_MVREF_THRESH
}

/// §6.5.12's `find_best_ref_mvs`. Returns `(NearestMv, NearMv, BestMv)`.
pub(crate) fn find_best_ref_mvs(mut ref_list_mv: [[i32; 2]; 2], allow_high_precision_mv: bool, mi_row: usize, mi_col: usize, mi_rows: usize, mi_cols: usize, bw: usize, bh: usize) -> ([i32; 2], [i32; 2], [i32; 2]) {
    for slot in &mut ref_list_mv {
        let mut delta_row = slot[0];
        let mut delta_col = slot[1];
        if !allow_high_precision_mv || !use_mv_hp(*slot) {
            if delta_row & 1 != 0 {
                delta_row += if delta_row > 0 { -1 } else { 1 };
            }
            if delta_col & 1 != 0 {
                delta_col += if delta_col > 0 { -1 } else { 1 };
            }
        }
        let border = (tables::BORDERINPIXELS - tables::INTERP_EXTEND) << 3;
        slot[0] = clamp_mv_row(delta_row, border, mi_row, mi_rows, bh);
        slot[1] = clamp_mv_col(delta_col, border, mi_col, mi_cols, bw);
    }
    (ref_list_mv[0], ref_list_mv[1], ref_list_mv[0])
}

/// §6.5.14's `append_sub8x8_mvs`. `block_mvs` is the in-progress block's own
/// `BlockMvs[refList]` (sub-blocks before `block` in raster order are
/// already filled in by the caller; the rest are stale/irrelevant).
pub(crate) fn append_sub8x8_mvs(
    ref_list_mv: [[i32; 2]; 2],
    block: usize,
    block_mvs: &[[i32; 2]; 4],
) -> ([i32; 2], [i32; 2]) {
    let mut sub8x8_mvs = [[0i32; 2]; 2];
    let mut dst = 0usize;
    let push = |mvs: &mut [[i32; 2]; 2], d: &mut usize, mv: [i32; 2]| {
        if let Some(slot) = mvs.get_mut(*d) {
            *slot = mv;
            *d += 1;
        }
    };
    let first = |mvs: &[[i32; 2]; 2]| mvs.first().copied().unwrap_or([0, 0]);
    if block == 0 {
        for cand in ref_list_mv {
            if dst < 2 {
                push(&mut sub8x8_mvs, &mut dst, cand);
            }
        }
    } else if block <= 2 {
        push(&mut sub8x8_mvs, &mut dst, block_mvs.first().copied().unwrap_or([0, 0]));
    } else {
        push(&mut sub8x8_mvs, &mut dst, block_mvs.get(2).copied().unwrap_or([0, 0]));
        let mut idx = 1i32;
        while idx >= 0 && dst < 2 {
            let cand = block_mvs.get(usize::try_from(idx).unwrap_or(0)).copied().unwrap_or([0, 0]);
            if cand != first(&sub8x8_mvs) {
                push(&mut sub8x8_mvs, &mut dst, cand);
            }
            idx -= 1;
        }
    }
    for cand in ref_list_mv {
        if dst >= 2 {
            break;
        }
        if cand != first(&sub8x8_mvs) {
            push(&mut sub8x8_mvs, &mut dst, cand);
        }
    }
    if dst < 2 {
        push(&mut sub8x8_mvs, &mut dst, [0, 0]);
    }
    (sub8x8_mvs.first().copied().unwrap_or([0, 0]), sub8x8_mvs.get(1).copied().unwrap_or([0, 0]))
}

/// §6.3.6's `inv_recenter_nonneg`... no — §6.5's `read_mv`/`read_mv_component`.
/// §6.4.19's `read_mv`. `best_mv` is `BestMv[ref]`. Returns the final `Mv`.
pub(crate) fn read_mv(bd: &mut Bd<'_>, entropy: &EntropyContext, best_mv: [i32; 2], allow_high_precision_mv: bool) -> [i32; 2] {
    let use_hp = allow_high_precision_mv && use_mv_hp(best_mv);
    let mut diff = [0i32, 0i32];
    let joint = bd.read_tree(&tables::MV_JOINT_TREE, &entropy.mv_joint_probs);
    if joint == tables::MV_JOINT_HZVNZ || joint == tables::MV_JOINT_HNZVNZ {
        diff[0] = read_mv_component(bd, entropy, 0, use_hp);
    }
    if joint == tables::MV_JOINT_HNZVZ || joint == tables::MV_JOINT_HNZVNZ {
        diff[1] = read_mv_component(bd, entropy, 1, use_hp);
    }
    [best_mv[0] + diff[0], best_mv[1] + diff[1]]
}

/// §6.4.20's `read_mv_component`.
fn read_mv_component(bd: &mut Bd<'_>, entropy: &EntropyContext, comp: usize, use_hp: bool) -> i32 {
    let sign_prob = entropy.mv_sign_prob.get(comp).copied().unwrap_or(128);
    let sign = bd.read_bool(sign_prob);
    let class_probs = entropy.mv_class_probs.get(comp).copied().unwrap_or([128; 10]);
    let mv_class = bd.read_tree(&tables::MV_CLASS_TREE, &class_probs);
    let mag = if mv_class == tables::MV_CLASS_0 {
        let bit_prob = entropy.mv_class0_bit_prob.get(comp).copied().unwrap_or(128);
        let class0_bit = i32::from(bd.read_bool(bit_prob));
        let fr_probs = entropy.mv_class0_fr_probs.get(comp).and_then(|r| r.get(usize::try_from(class0_bit).unwrap_or(0))).copied().unwrap_or([128; 3]);
        let class0_fr = bd.read_tree(&tables::MV_FR_TREE, &fr_probs);
        let class0_hp = if use_hp {
            let hp_prob = entropy.mv_class0_hp_prob.get(comp).copied().unwrap_or(128);
            i32::from(bd.read_bool(hp_prob))
        } else {
            1
        };
        ((class0_bit << 3) | (class0_fr << 1) | class0_hp) + 1
    } else {
        let mut d = 0i32;
        let n = usize::try_from(mv_class).unwrap_or(0);
        for i in 0..n {
            let bit_prob = entropy.mv_bits_prob.get(comp).and_then(|r| r.get(i)).copied().unwrap_or(128);
            let mv_bit = i32::from(bd.read_bool(bit_prob));
            d |= mv_bit << i;
        }
        let mut mag = i32::try_from(tables::CLASS0_SIZE).unwrap_or(2) << (mv_class + 2);
        let fr_probs = entropy.mv_fr_probs.get(comp).copied().unwrap_or([128; 3]);
        let mv_fr = bd.read_tree(&tables::MV_FR_TREE, &fr_probs);
        let mv_hp = if use_hp {
            let hp_prob = entropy.mv_hp_prob.get(comp).copied().unwrap_or(128);
            i32::from(bd.read_bool(hp_prob))
        } else {
            1
        };
        mag += ((d << 3) | (mv_fr << 1) | mv_hp) + 1;
        mag
    };
    if sign { -mag } else { mag }
}
