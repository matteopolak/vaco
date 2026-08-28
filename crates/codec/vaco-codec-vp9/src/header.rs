//! VP9 §6.1/§6.2 (uncompressed header) and §6.3 (compressed header —
//! probability forward update) parsing.
//!
//! # Scope: key frames
//!
//! `parse_uncompressed_header` reads the *entire* uncompressed header
//! structurally for every frame type (so a non-key frame in the middle of a
//! stream does not desync byte counting for whatever follows), but
//! `vaco-codec-vp9`'s decode path (`crate::decode`) only reconstructs pixels
//! when the result is a key frame. See the crate-level doc for exactly
//! where support stops on an inter frame.
//!
//! Every key frame calls §6.2's `setup_past_independence()` (`FrameIsIntra`
//! is always 1 for a real key frame, and the uncompressed header's own
//! syntax makes that call unconditional in that case) — which resets the
//! probability model to the specification's defaults before this frame's
//! compressed header forward-updates it. `EntropyContext::default()` is
//! that reset. One consequence, checked against the syntax table rather
//! than assumed: a stream of consecutive key frames never carries adapted
//! probabilities from one key frame to the next, because every key frame
//! resets to defaults regardless of what a previous frame's backward
//! adaptation (§8.4) would otherwise have produced. Backward adaptation
//! therefore cannot affect — or be verified by — any bitstream this crate
//! can fully decode (key frames only), so it is not implemented here; see
//! `planning/TECH-DEBT.md`.

use vaco_bitstream::BitReader;
use vaco_codec_msac::Vp9BoolDecoder as Bd;

use crate::tables;

/// §6.2.2's `color_config()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorConfig {
    pub bit_depth: u8,
    pub color_space: u8,
    pub full_range: bool,
    pub subsampling_x: bool,
    pub subsampling_y: bool,
}

const CS_RGB: u32 = 7;

fn color_config(r: &mut BitReader<'_>, profile: u8) -> ColorConfig {
    let bit_depth = if profile >= 2 {
        if r.get(1) != 0 { 12 } else { 10 }
    } else {
        8
    };
    let color_space = u8::try_from(r.get(3)).unwrap_or(0);
    let (full_range, subsampling_x, subsampling_y) = if u32::from(color_space) == CS_RGB {
        if profile == 1 || profile == 3 {
            let _reserved_zero = r.get(1);
        }
        (true, false, false)
    } else {
        let full_range = r.get(1) != 0;
        if profile == 1 || profile == 3 {
            let sx = r.get(1) != 0;
            let sy = r.get(1) != 0;
            let _reserved_zero = r.get(1);
            (full_range, sx, sy)
        } else {
            (full_range, true, true)
        }
    };
    ColorConfig { bit_depth, color_space, full_range, subsampling_x, subsampling_y }
}

/// §6.2.8's loop filter parameters (parsed and stored; not applied by this
/// crate, whose scope stops before the loop filter — epic #32/C-32a).
#[derive(Debug, Clone, Copy, Default)]
pub struct LoopFilterParams {
    pub level: i32,
    pub sharpness: i32,
    pub delta_enabled: bool,
    pub ref_deltas: [i32; 4],
    pub mode_deltas: [i32; 2],
}

fn signed_literal(r: &mut BitReader<'_>, n: u32) -> i32 {
    let value = i32::try_from(r.get(n)).unwrap_or(0);
    if r.get(1) != 0 { -value } else { value }
}

fn loop_filter_params(r: &mut BitReader<'_>, prev: LoopFilterParams) -> LoopFilterParams {
    let mut lf = prev;
    lf.level = i32::try_from(r.get(6)).unwrap_or(0);
    lf.sharpness = i32::try_from(r.get(3)).unwrap_or(0);
    lf.delta_enabled = r.get(1) != 0;
    if lf.delta_enabled {
        let delta_update = r.get(1) != 0;
        if delta_update {
            for slot in &mut lf.ref_deltas {
                if r.get(1) != 0 {
                    *slot = signed_literal(r, 6);
                }
            }
            for slot in &mut lf.mode_deltas {
                if r.get(1) != 0 {
                    *slot = signed_literal(r, 6);
                }
            }
        }
    }
    lf
}

/// §6.2.9's quantization parameters.
#[derive(Debug, Clone, Copy, Default)]
pub struct QuantParams {
    pub base_q_idx: i32,
    pub delta_q_y_dc: i32,
    pub delta_q_uv_dc: i32,
    pub delta_q_uv_ac: i32,
    pub lossless: bool,
}

fn read_delta_q(r: &mut BitReader<'_>) -> i32 {
    if r.get(1) != 0 { signed_literal(r, 4) } else { 0 }
}

fn quantization_params(r: &mut BitReader<'_>) -> QuantParams {
    let base_q_idx = i32::try_from(r.get(8)).unwrap_or(0);
    let delta_q_y_dc = read_delta_q(r);
    let delta_q_uv_dc = read_delta_q(r);
    let delta_q_uv_ac = read_delta_q(r);
    let lossless = base_q_idx == 0 && delta_q_y_dc == 0 && delta_q_uv_dc == 0 && delta_q_uv_ac == 0;
    QuantParams { base_q_idx, delta_q_y_dc, delta_q_uv_dc, delta_q_uv_ac, lossless }
}

/// §6.2.11's segmentation parameters, persisted across frames (a frame that
/// does not update the map / data keeps the previous frame's).
#[derive(Debug, Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent §6.2.11 syntax element, not related flags that belong in one enum"
)]
pub struct Segmentation {
    pub enabled: bool,
    pub update_map: bool,
    pub tree_probs: [u8; 7],
    pub temporal_update: bool,
    pub pred_prob: [u8; 3],
    pub abs_or_delta_update: bool,
    pub feature_enabled: [[bool; tables::SEG_LVL_MAX]; tables::MAX_SEGMENTS],
    pub feature_data: [[i32; tables::SEG_LVL_MAX]; tables::MAX_SEGMENTS],
}

impl Default for Segmentation {
    fn default() -> Self {
        Self {
            enabled: false,
            update_map: false,
            tree_probs: [255; 7],
            temporal_update: false,
            pred_prob: [255; 3],
            abs_or_delta_update: false,
            feature_enabled: [[false; tables::SEG_LVL_MAX]; tables::MAX_SEGMENTS],
            feature_data: [[0; tables::SEG_LVL_MAX]; tables::MAX_SEGMENTS],
        }
    }
}

fn read_prob(r: &mut BitReader<'_>) -> u8 {
    if r.get(1) != 0 { u8::try_from(r.get(8)).unwrap_or(255) } else { 255 }
}

fn segmentation_params(r: &mut BitReader<'_>, prev: Segmentation) -> Segmentation {
    let mut seg = prev;
    seg.enabled = r.get(1) != 0;
    seg.update_map = false;
    if !seg.enabled {
        return seg;
    }
    seg.update_map = r.get(1) != 0;
    if seg.update_map {
        for slot in &mut seg.tree_probs {
            *slot = read_prob(r);
        }
        seg.temporal_update = r.get(1) != 0;
        for slot in &mut seg.pred_prob {
            *slot = if seg.temporal_update { read_prob(r) } else { 255 };
        }
    }
    let update_data = r.get(1) != 0;
    if update_data {
        seg.abs_or_delta_update = r.get(1) != 0;
        for i in 0..tables::MAX_SEGMENTS {
            for j in 0..tables::SEG_LVL_MAX {
                let enabled = r.get(1) != 0;
                if let Some(row) = seg.feature_enabled.get_mut(i)
                    && let Some(slot) = row.get_mut(j)
                {
                    *slot = enabled;
                }
                let mut value = 0i32;
                if enabled {
                    let bits = tables::SEGMENTATION_FEATURE_BITS.get(j).copied().unwrap_or(0);
                    value = i32::try_from(r.get(bits)).unwrap_or(0);
                    if tables::SEGMENTATION_FEATURE_SIGNED.get(j).copied().unwrap_or(false)
                        && r.get(1) != 0
                    {
                        value = -value;
                    }
                }
                if let Some(row) = seg.feature_data.get_mut(i)
                    && let Some(slot) = row.get_mut(j)
                {
                    *slot = value;
                }
            }
        }
    }
    seg
}

/// §6.2.13's tile info: how many tile columns/rows this frame's tile data
/// is split into.
#[derive(Debug, Clone, Copy, Default)]
pub struct TileInfo {
    pub cols_log2: u32,
    pub rows_log2: u32,
}

fn calc_min_log2_tile_cols(sb64_cols: usize) -> u32 {
    let mut min_log2 = 0u32;
    while (64usize << min_log2) < sb64_cols {
        min_log2 += 1;
    }
    min_log2
}

fn calc_max_log2_tile_cols(sb64_cols: usize) -> u32 {
    let mut max_log2 = 1u32;
    while (sb64_cols >> max_log2) >= 4 {
        max_log2 += 1;
    }
    max_log2.saturating_sub(1)
}

fn tile_info(r: &mut BitReader<'_>, sb64_cols: usize) -> TileInfo {
    let min_log2 = calc_min_log2_tile_cols(sb64_cols);
    let max_log2 = calc_max_log2_tile_cols(sb64_cols);
    let mut cols_log2 = min_log2;
    while cols_log2 < max_log2 {
        if r.get(1) != 0 {
            cols_log2 += 1;
        } else {
            break;
        }
    }
    let mut rows_log2 = 0u32;
    if r.get(1) != 0 {
        rows_log2 = 1;
        if r.get(1) != 0 {
            rows_log2 = 2;
        }
    }
    TileInfo { cols_log2, rows_log2 }
}

/// What `uncompressed_header()` states, restricted to the fields this crate
/// needs to reach the compressed header and tile data.
#[derive(Debug, Clone)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent §6.2 syntax element, mirroring the spec's own field list"
)]
pub struct FrameHeader {
    pub profile: u8,
    pub show_existing_frame: bool,
    pub frame_to_show_map_idx: u8,
    pub is_key_frame: bool,
    pub show_frame: bool,
    pub error_resilient_mode: bool,
    pub intra_only: bool,
    pub frame_is_intra: bool,
    pub color: ColorConfig,
    pub width: u32,
    pub height: u32,
    pub mi_cols: usize,
    pub mi_rows: usize,
    pub sb64_cols: usize,
    pub sb64_rows: usize,
    pub refresh_frame_flags: u8,
    pub refresh_frame_context: bool,
    pub frame_parallel_decoding_mode: bool,
    pub loop_filter: LoopFilterParams,
    pub quant: QuantParams,
    pub segmentation: Segmentation,
    pub tile: TileInfo,
    pub header_size_in_bytes: u16,
    /// Filled in by [`crate::decode`] after `parse_compressed_header` runs
    /// (not part of `uncompressed_header()` itself, but carried alongside
    /// it for convenience since every block-decode helper needs both).
    pub entropy: EntropyContext,
    pub tx_mode: i32,
}

fn compute_image_size(width: u32, height: u32) -> (usize, usize, usize, usize) {
    let mi_cols = (usize::try_from(width).unwrap_or(0) + 7) >> 3;
    let mi_rows = (usize::try_from(height).unwrap_or(0) + 7) >> 3;
    let sb64_cols = (mi_cols + 7) >> 3;
    let sb64_rows = (mi_rows + 7) >> 3;
    (mi_cols, mi_rows, sb64_cols, sb64_rows)
}

fn frame_size(r: &mut BitReader<'_>) -> (u32, u32) {
    let width = r.get(16) + 1;
    let height = r.get(16) + 1;
    (width, height)
}

fn skip_render_size(r: &mut BitReader<'_>) {
    if r.get(1) != 0 {
        let _ = r.get(16);
        let _ = r.get(16);
    }
}

/// Parses `uncompressed_header()` from the start of one VP9 frame (not a
/// superframe-wrapping sample — split with [`crate::superframe::split`]
/// first). `prev_loop_filter`/`prev_seg` are the previous frame's persisted
/// state (segmentation and loop-filter deltas both carry forward when a
/// frame does not update them). Returns `None` on a bad `frame_marker` or a
/// buffer too short for the fields this crate reads.
#[must_use]
#[allow(clippy::too_many_lines, reason = "one linear syntax table, §6.2")]
pub fn parse_uncompressed_header(
    data: &[u8],
    prev_loop_filter: LoopFilterParams,
    prev_seg: Segmentation,
) -> Option<(FrameHeader, usize)> {
    let mut r = BitReader::new(data);
    if r.get(2) != 2 {
        return None;
    }
    let profile_low = r.get(1);
    let profile_high = r.get(1);
    let profile = u8::try_from((profile_high << 1) | profile_low).unwrap_or(0);
    if profile == 3 {
        let _reserved_zero = r.get(1);
    }
    let show_existing_frame = r.get(1) != 0;
    if show_existing_frame {
        let frame_to_show_map_idx = u8::try_from(r.get(3)).unwrap_or(0);
        r.check().ok()?;
        let bits = usize::try_from(r.bit_pos()).unwrap_or(0);
        return Some((
            FrameHeader {
                profile,
                show_existing_frame: true,
                frame_to_show_map_idx,
                is_key_frame: false,
                show_frame: true,
                error_resilient_mode: false,
                intra_only: false,
                frame_is_intra: false,
                color: ColorConfig { bit_depth: 8, color_space: 0, full_range: false, subsampling_x: true, subsampling_y: true },
                width: 0,
                height: 0,
                mi_cols: 0,
                mi_rows: 0,
                sb64_cols: 0,
                sb64_rows: 0,
                refresh_frame_flags: 0,
                refresh_frame_context: false,
                frame_parallel_decoding_mode: true,
                loop_filter: prev_loop_filter,
                quant: QuantParams::default(),
                segmentation: prev_seg,
                tile: TileInfo::default(),
                header_size_in_bytes: 0,
                entropy: EntropyContext::default(),
                tx_mode: tables::ONLY_4X4,
            },
            bits.div_ceil(8),
        ));
    }

    let is_key_frame = r.get(1) == 0;
    let show_frame = r.get(1) != 0;
    let error_resilient_mode = r.get(1) != 0;

    let mut color = ColorConfig { bit_depth: 8, color_space: 1, full_range: false, subsampling_x: true, subsampling_y: true };
    let mut width = 0u32;
    let mut height = 0u32;
    let refresh_frame_flags;
    let mut intra_only = false;
    let frame_is_intra;

    if is_key_frame {
        if !frame_sync_code_ok(&mut r) {
            return None;
        }
        color = color_config(&mut r, profile);
        (width, height) = frame_size(&mut r);
        skip_render_size(&mut r);
        refresh_frame_flags = 0xFF;
        frame_is_intra = true;
    } else {
        intra_only = if show_frame { false } else { r.get(1) != 0 };
        frame_is_intra = intra_only;
        if !error_resilient_mode {
            let _reset_frame_context = r.get(2);
        }
        if intra_only {
            if !frame_sync_code_ok(&mut r) {
                return None;
            }
            color = if profile > 0 {
                color_config(&mut r, profile)
            } else {
                ColorConfig { bit_depth: 8, color_space: 1, full_range: false, subsampling_x: true, subsampling_y: true }
            };
            refresh_frame_flags = u8::try_from(r.get(8)).unwrap_or(0);
            (width, height) = frame_size(&mut r);
            skip_render_size(&mut r);
        } else {
            refresh_frame_flags = u8::try_from(r.get(8)).unwrap_or(0);
            for _ in 0..3 {
                let _ref_frame_idx = r.get(3);
                let _ref_frame_sign_bias = r.get(1);
            }
            // frame_size_with_refs(): this crate does not track reference
            // frame dimensions (out of scope — inter decode is C-31), so an
            // inter frame's width/height cannot be resolved here. The
            // caller must not attempt pixel reconstruction in this case.
            for _ in 0..3 {
                if r.get(1) != 0 {
                    break;
                }
            }
            let _allow_high_precision_mv = r.get(1);
            if r.get(1) == 0 {
                let _raw_interpolation_filter = r.get(2);
            }
        }
    }

    let (refresh_frame_context, frame_parallel_decoding_mode) = if error_resilient_mode {
        (false, true)
    } else {
        (r.get(1) != 0, r.get(1) != 0)
    };
    let _frame_context_idx = r.get(2);

    let loop_filter = loop_filter_params(&mut r, prev_loop_filter);
    let quant = quantization_params(&mut r);
    // §7.2's `setup_past_independence`: a key frame (`FrameIsIntra` is
    // always 1 here) resets FeatureData/FeatureEnabled and
    // segmentation_abs_or_delta_update to their spec defaults *before*
    // this frame's own `segmentation_params()` update is applied — loop
    // filter deltas are reset too, but that reset is inert here since this
    // crate never applies the loop filter (out of scope; see the crate
    // doc), only quantization-affecting segmentation state matters for
    // pixel correctness.
    let seg_base = if frame_is_intra { Segmentation::default() } else { prev_seg };
    let segmentation = segmentation_params(&mut r, seg_base);

    let (mi_cols, mi_rows, sb64_cols, sb64_rows) = compute_image_size(width, height);
    let tile = tile_info(&mut r, sb64_cols);

    let header_size_in_bytes = u16::try_from(r.get(16)).unwrap_or(0);

    r.check().ok()?;
    let bits = usize::try_from(r.bit_pos()).unwrap_or(0);
    Some((
        FrameHeader {
            profile,
            show_existing_frame: false,
            frame_to_show_map_idx: 0,
            is_key_frame,
            show_frame,
            error_resilient_mode,
            intra_only,
            frame_is_intra,
            color,
            width,
            height,
            mi_cols,
            mi_rows,
            sb64_cols,
            sb64_rows,
            refresh_frame_flags,
            refresh_frame_context,
            frame_parallel_decoding_mode,
            loop_filter,
            quant,
            segmentation,
            tile,
            header_size_in_bytes,
            entropy: EntropyContext::default(),
            tx_mode: tables::ONLY_4X4,
        },
        bits.div_ceil(8),
    ))
}

fn frame_sync_code_ok(r: &mut BitReader<'_>) -> bool {
    r.get(8) == 0x49 && r.get(8) == 0x83 && r.get(8) == 0x42
}

/// §10.4/§10.5's default probability tables that a key frame's compressed
/// header forward-updates: coefficient probabilities, skip probability and
/// tx-size probability. (`kf_y_mode_probs`/`kf_uv_mode_probs`/
/// `kf_partition_probs` are fixed constants a key frame reads directly —
/// never forward-updated — so they live only in `tables.rs`, not here.)
#[derive(Debug, Clone)]
pub struct EntropyContext {
    pub coef_probs: [[[[[[u8; 3]; 6]; 6]; 2]; 2]; 4],
    pub skip_prob: [u8; 3],
    pub tx_probs: [[[u8; 3]; 2]; 4],
}

impl Default for EntropyContext {
    fn default() -> Self {
        Self {
            coef_probs: tables::DEFAULT_COEF_PROBS,
            skip_prob: tables::DEFAULT_SKIP_PROB,
            tx_probs: tables::DEFAULT_TX_PROBS,
        }
    }
}

/// §6.3.3's `diff_update_prob`.
fn diff_update_prob(bd: &mut Bd<'_>, prob: u8) -> u8 {
    if bd.read_bool(252) {
        let delta = decode_term_subexp(bd);
        inv_remap_prob(delta, prob)
    } else {
        prob
    }
}

/// §6.3.4's `decode_term_subexp`.
fn decode_term_subexp(bd: &mut Bd<'_>) -> i32 {
    if bd.read_literal(1) == 0 {
        return i32::try_from(bd.read_literal(4)).unwrap_or(0);
    }
    if bd.read_literal(1) == 0 {
        return i32::try_from(bd.read_literal(4)).unwrap_or(0) + 16;
    }
    if bd.read_literal(1) == 0 {
        return i32::try_from(bd.read_literal(5)).unwrap_or(0) + 32;
    }
    let v = i32::try_from(bd.read_literal(7)).unwrap_or(0);
    if v < 65 {
        return v + 64;
    }
    let bit = i32::try_from(bd.read_literal(1)).unwrap_or(0);
    (v << 1) - 1 + bit
}

/// §6.3.6's `inv_recenter_nonneg`.
fn inv_recenter_nonneg(v: i32, m: i32) -> i32 {
    if v > 2 * m {
        return v;
    }
    if v & 1 != 0 { m - ((v + 1) >> 1) } else { m + (v >> 1) }
}

/// §6.3.5's `inv_remap_prob`. The spec decrements `m` (`m--`) immediately
/// after reading `prob`, *before* the `(m<<1) <= 255` test and both
/// `inv_recenter_nonneg` calls — easy to drop since `prob` and `m` look
/// interchangeable, but the two differ by exactly 1 and every updated
/// probability comes out wrong without it.
fn inv_remap_prob(delta_prob: i32, prob: u8) -> u8 {
    let m = i32::from(prob) - 1;
    let v = tables::INV_MAP_TABLE
        .get(usize::try_from(delta_prob).unwrap_or(0))
        .copied()
        .map_or(0, i32::from);
    let m = if (m << 1) <= 255 {
        1 + inv_recenter_nonneg(v, m)
    } else {
        255 - inv_recenter_nonneg(v, 255 - 1 - m)
    };
    u8::try_from(m.clamp(0, 255)).unwrap_or(255)
}

/// §6.3's `compressed_header()`, restricted to what a key frame reads
/// (`FrameIsIntra` gates every inter-only probability table behind a
/// condition that is always false for a real key frame — see §6.3's own
/// syntax table). Returns the decoded `tx_mode`.
pub fn parse_compressed_header(bd: &mut Bd<'_>, lossless: bool, entropy: &mut EntropyContext) -> i32 {
    let tx_mode = if lossless {
        tables::ONLY_4X4
    } else {
        let mut m = i32::try_from(bd.read_literal(2)).unwrap_or(0);
        if m == tables::ALLOW_32X32 && bd.read_literal(1) != 0 {
            m += 1;
        }
        m
    };
    if tx_mode == tables::TX_MODE_SELECT {
        // §6.3.2's three separate loops (`tx_probs_8x8`/`16x16`/`32x32`),
        // read in that order, onto the unified `[maxTxSize][ctx][slot]`
        // shape `tables::DEFAULT_TX_PROBS` documents (row 0, `TX_4X4`, is
        // never touched — there is nothing to select at that size).
        for (row_idx, take) in [(1usize, 1usize), (2, 2), (3, 3)] {
            if let Some(row) = entropy.tx_probs.get_mut(row_idx) {
                for ctx in row.iter_mut() {
                    for slot in ctx.iter_mut().take(take) {
                        *slot = diff_update_prob(bd, *slot);
                    }
                }
            }
        }
    }
    read_coef_probs(bd, tx_mode, entropy);
    for slot in &mut entropy.skip_prob {
        *slot = diff_update_prob(bd, *slot);
    }
    tx_mode
}

fn read_coef_probs(bd: &mut Bd<'_>, tx_mode: i32, entropy: &mut EntropyContext) {
    let max_tx_size = tables::TX_MODE_TO_BIGGEST_TX_SIZE.get(usize::try_from(tx_mode).unwrap_or(0)).copied().unwrap_or(0);
    for tx_sz in 0..=max_tx_size {
        let update = bd.read_literal(1) != 0;
        if !update {
            continue;
        }
        let Some(tx_table) = entropy.coef_probs.get_mut(usize::try_from(tx_sz).unwrap_or(0)) else { continue };
        for i_table in tx_table.iter_mut() {
            for j_table in i_table.iter_mut() {
                for (k, k_table) in j_table.iter_mut().enumerate() {
                    let max_l = if k == 0 { 3 } else { 6 };
                    for l_table in k_table.iter_mut().take(max_l) {
                        for m_slot in l_table.iter_mut() {
                            *m_slot = diff_update_prob(bd, *m_slot);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §6.3.5's `inv_remap_prob` decrements `m` (`m--`) before using it in
    /// the `(m<<1) <= 255` test and both `inv_recenter_nonneg` calls. A
    /// version that forgets the decrement takes the *other* branch for
    /// `prob == 128` (`256 <= 255` is false, `254 <= 255` is true) and
    /// returns a visibly different value (133 instead of 124) — this hand
    /// computation, done directly against the spec text rather than by
    /// running the buggy code, is what caught the missing decrement.
    #[test]
    fn inv_remap_prob_decrements_m_before_use() {
        assert_eq!(inv_remap_prob(0, 128), 124);
    }
}
