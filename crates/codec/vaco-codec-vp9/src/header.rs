//! VP9 §6.1/§6.2 (uncompressed header, now including inter frames' own
//! fields — `ref_frame_idx`/`ref_frame_sign_bias`/`allow_high_precision_mv`/
//! `interpolation_filter`/`frame_size_with_refs`) and §6.3 (compressed
//! header — the forward-updated probability model, key-frame and inter
//! tables alike) parsing.
//!
//! Every key frame (and every error-resilient frame) calls
//! §6.2's `setup_past_independence()` — which resets the probability model
//! to the specification's defaults before that frame's own compressed
//! header forward-updates it (`crate::decode::Vp9Decoder::decode_one_frame`
//! implements the `save_probs`/`load_probs`/`frame_context_idx` machinery
//! around this). Backward probability adaptation (§8.3/8.4) is *not*
//! implemented, so later frames in a multi-frame GOP do not update that model.

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
    ColorConfig {
        bit_depth,
        color_space,
        full_range,
        subsampling_x,
        subsampling_y,
    }
}

/// §6.2.8's loop filter parameters, parsed and stored but not applied by this
/// crate because its scope stops before the loop filter.
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
    if r.get(1) != 0 {
        signed_literal(r, 4)
    } else {
        0
    }
}

fn quantization_params(r: &mut BitReader<'_>) -> QuantParams {
    let base_q_idx = i32::try_from(r.get(8)).unwrap_or(0);
    let delta_q_y_dc = read_delta_q(r);
    let delta_q_uv_dc = read_delta_q(r);
    let delta_q_uv_ac = read_delta_q(r);
    let lossless = base_q_idx == 0 && delta_q_y_dc == 0 && delta_q_uv_dc == 0 && delta_q_uv_ac == 0;
    QuantParams {
        base_q_idx,
        delta_q_y_dc,
        delta_q_uv_dc,
        delta_q_uv_ac,
        lossless,
    }
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
    if r.get(1) != 0 {
        u8::try_from(r.get(8)).unwrap_or(255)
    } else {
        255
    }
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
            *slot = if seg.temporal_update {
                read_prob(r)
            } else {
                255
            };
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
                    let bits = tables::SEGMENTATION_FEATURE_BITS
                        .get(j)
                        .copied()
                        .unwrap_or(0);
                    value = i32::try_from(r.get(bits)).unwrap_or(0);
                    if tables::SEGMENTATION_FEATURE_SIGNED
                        .get(j)
                        .copied()
                        .unwrap_or(false)
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
    TileInfo {
        cols_log2,
        rows_log2,
    }
}

/// One `RefFrameWidth`/`RefFrameHeight`/`RefFrameSignBias`-relevant entry
/// from the reference-frame store, as `frame_size_with_refs`/motion vector
/// scaling need it. `None` means the slot has never been written (a
/// conforming bitstream never actually references such a slot, but this
/// crate does not trust that).
#[derive(Debug, Clone, Copy, Default)]
pub struct RefFrameDims {
    pub width: u32,
    pub height: u32,
}

/// What `compute_image_size` needs to remember about the last frame it was
/// invoked for (§7.2.6's `UsePrevFrameMvs` conditions (a)-(c); `None` means
/// "never invoked", satisfying condition (a) directly).
#[derive(Debug, Clone, Copy)]
pub struct PrevFrameInfo {
    pub width: u32,
    pub height: u32,
    pub show_frame: bool,
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
    pub reset_frame_context: u8,
    pub frame_context_idx: u8,
    /// §6.2's `ref_frame_idx[REFS_PER_FRAME]` — which of the `NUM_REF_FRAMES`
    /// (8) reference-frame-store slots this frame's `LAST_FRAME`/
    /// `GOLDEN_FRAME`/`ALTREF_FRAME` map to (index 0/1/2 respectively).
    pub ref_frame_idx: [u8; 3],
    /// §6.2's `ref_frame_sign_bias[MAX_REF_FRAMES]`, indexed by the
    /// `ref_frame` value itself (`INTRA_FRAME`'s slot is always `false`,
    /// unused).
    pub ref_frame_sign_bias: [bool; 4],
    pub allow_high_precision_mv: bool,
    /// §6.2.7's `interpolation_filter`: `EIGHTTAP`/`EIGHTTAP_SMOOTH`/
    /// `EIGHTTAP_SHARP`/`BILINEAR`/`SWITCHABLE`.
    pub interpolation_filter: i32,
    /// §7.2.6: whether this frame's motion-vector prediction may read the
    /// previous frame's per-block motion vectors/reference frames as a
    /// temporal candidate.
    pub use_prev_frame_mvs: bool,
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
    /// §6.3.12's `reference_mode`: `SINGLE_REFERENCE`/`COMPOUND_REFERENCE`/
    /// `REFERENCE_MODE_SELECT`. Filled in alongside `entropy`/`tx_mode`.
    pub reference_mode: i32,
    /// §6.3.18's `CompFixedRef`/`CompVarRef`, meaningful only when
    /// `reference_mode != SINGLE_REFERENCE`.
    pub comp_fixed_ref: i32,
    pub comp_var_ref: [i32; 2],
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
/// frame does not update them). `prev_color` is the sequence's color config
/// as last established by a key frame or profile>0 intra-only frame: §6.2
/// only calls `color_config()` on those two frame kinds, so a regular inter
/// frame (and a profile-0 intra-only frame, which is hardcoded regardless)
/// carries the previous value forward rather than re-reading it — before
/// this parameter existed, that carry-forward was a hardcoded 8-bit/4:2:0
/// literal, which happened to be correct for profile 0 (the only profile
/// with fixture coverage) and silently wrong for every inter frame of any
/// other profile. Returns `None` on a bad `frame_marker` or a buffer too
/// short for the fields this crate reads.
#[must_use]
#[allow(clippy::too_many_lines, reason = "one linear syntax table, §6.2")]
pub fn parse_uncompressed_header(
    data: &[u8],
    prev_loop_filter: LoopFilterParams,
    prev_seg: Segmentation,
    ref_dims: &[Option<RefFrameDims>; tables::NUM_REF_FRAMES],
    prev_frame: Option<PrevFrameInfo>,
    prev_color: ColorConfig,
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
                // Not a real syntax element for this frame kind — carried
                // through unread, same as `prev_loop_filter`/`prev_seg`
                // below, since `decode_one_frame` persists it back into
                // `State` and a show-existing-frame must not reset the
                // sequence's established color config.
                color: prev_color,
                width: 0,
                height: 0,
                mi_cols: 0,
                mi_rows: 0,
                sb64_cols: 0,
                sb64_rows: 0,
                refresh_frame_flags: 0,
                refresh_frame_context: false,
                frame_parallel_decoding_mode: true,
                reset_frame_context: 0,
                frame_context_idx: 0,
                ref_frame_idx: [0; 3],
                ref_frame_sign_bias: [false; 4],
                allow_high_precision_mv: false,
                interpolation_filter: tables::EIGHTTAP,
                use_prev_frame_mvs: false,
                loop_filter: prev_loop_filter,
                quant: QuantParams::default(),
                segmentation: prev_seg,
                tile: TileInfo::default(),
                header_size_in_bytes: 0,
                entropy: EntropyContext::default(),
                tx_mode: tables::ONLY_4X4,
                reference_mode: tables::SINGLE_REFERENCE,
                comp_fixed_ref: 0,
                comp_var_ref: [0; 2],
            },
            bits.div_ceil(8),
        ));
    }

    let is_key_frame = r.get(1) == 0;
    let show_frame = r.get(1) != 0;
    let error_resilient_mode = r.get(1) != 0;

    // §6.2's `color_config()` is only called on a key frame or a profile>0
    // intra-only frame (below); every other path here — a profile-0
    // intra-only frame (hardcoded, overwrites this unconditionally) and a
    // regular inter frame (never overwrites it) — needs the sequence's
    // established value, not a fresh default.
    let mut color = prev_color;
    let width;
    let height;
    let refresh_frame_flags;
    let mut intra_only = false;
    let frame_is_intra;
    let mut reset_frame_context = 0u8;
    let mut ref_frame_idx = [0u8; 3];
    let mut ref_frame_sign_bias = [false; 4];
    let mut allow_high_precision_mv = false;
    let mut interpolation_filter = tables::EIGHTTAP;

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
            reset_frame_context = u8::try_from(r.get(2)).unwrap_or(0);
        }
        if intra_only {
            if !frame_sync_code_ok(&mut r) {
                return None;
            }
            color = if profile > 0 {
                color_config(&mut r, profile)
            } else {
                ColorConfig {
                    bit_depth: 8,
                    color_space: 1,
                    full_range: false,
                    subsampling_x: true,
                    subsampling_y: true,
                }
            };
            refresh_frame_flags = u8::try_from(r.get(8)).unwrap_or(0);
            (width, height) = frame_size(&mut r);
            skip_render_size(&mut r);
        } else {
            refresh_frame_flags = u8::try_from(r.get(8)).unwrap_or(0);
            for (i, slot) in ref_frame_idx.iter_mut().enumerate() {
                *slot = u8::try_from(r.get(3)).unwrap_or(0);
                let bias = r.get(1) != 0;
                // §7.2's ref_frame_sign_bias is indexed by ref_frame value
                // (LAST_FRAME=1, GOLDEN_FRAME=2, ALTREF_FRAME=3); i is
                // LAST_FRAME + i - LAST_FRAME = i, so the slot is i+1.
                if let Some(dst) = ref_frame_sign_bias.get_mut(i + 1) {
                    *dst = bias;
                }
            }
            (width, height) = frame_size_with_refs(&mut r, ref_frame_idx, ref_dims)?;
            allow_high_precision_mv = r.get(1) != 0;
            interpolation_filter = read_interpolation_filter(&mut r);
        }
    }

    let (refresh_frame_context, frame_parallel_decoding_mode) = if error_resilient_mode {
        (false, true)
    } else {
        (r.get(1) != 0, r.get(1) != 0)
    };
    let frame_context_idx = u8::try_from(r.get(2)).unwrap_or(0);

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
    let seg_base = if frame_is_intra {
        Segmentation::default()
    } else {
        prev_seg
    };
    let segmentation = segmentation_params(&mut r, seg_base);

    let (mi_cols, mi_rows, sb64_cols, sb64_rows) = compute_image_size(width, height);
    // §7.2.6's `UsePrevFrameMvs`: conditions (a)-(c) (never invoked before /
    // same dimensions / previous invocation's `show_frame`) are checked
    // against `prev_frame`; (d)/(e) (this frame's own
    // `error_resilient_mode`/`FrameIsIntra`) against the current frame.
    let use_prev_frame_mvs = !frame_is_intra
        && !error_resilient_mode
        && prev_frame.is_some_and(|p| p.width == width && p.height == height && p.show_frame);
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
            reset_frame_context,
            frame_context_idx,
            ref_frame_idx,
            ref_frame_sign_bias,
            allow_high_precision_mv,
            interpolation_filter,
            use_prev_frame_mvs,
            loop_filter,
            quant,
            segmentation,
            tile,
            header_size_in_bytes,
            entropy: EntropyContext::default(),
            tx_mode: tables::ONLY_4X4,
            reference_mode: tables::SINGLE_REFERENCE,
            comp_fixed_ref: 0,
            comp_var_ref: [0; 2],
        },
        bits.div_ceil(8),
    ))
}

/// §6.2.5's `frame_size_with_refs`. Uses the first found reference frame's
/// dimensions if any `found_ref` bit is set, otherwise reads `frame_size()`
/// directly. A `ref_frame_idx` slot with no recorded dimensions (an
/// out-of-range index, or a slot never written by an earlier frame) is
/// treated as "not found" rather than trusted — untrusted bitstream data
/// must not desync the reader by skipping bits `found_ref == 1` would
/// otherwise commit to reading.
fn frame_size_with_refs(
    r: &mut BitReader<'_>,
    ref_frame_idx: [u8; 3],
    ref_dims: &[Option<RefFrameDims>; tables::NUM_REF_FRAMES],
) -> Option<(u32, u32)> {
    let mut found_at = None;
    for (i, &idx) in ref_frame_idx.iter().enumerate() {
        let found_ref = r.get(1) != 0;
        if found_ref {
            found_at = Some(idx);
            break;
        }
        let _ = i;
    }
    let (width, height) = match found_at {
        // `found_ref == 1`: the bitstream has already committed to *not*
        // sending frame_size() here, so a slot with no recorded dimensions
        // (out-of-range index, or a slot no earlier frame ever wrote) is
        // not recoverable by falling back to frame_size() — that would
        // read bits the bitstream never put there. Fail the whole header
        // parse instead of desyncing everything after this point.
        Some(idx) => {
            let dims = ref_dims.get(usize::from(idx)).copied().flatten()?;
            (dims.width, dims.height)
        }
        None => frame_size(r),
    };
    skip_render_size(r);
    Some((width, height))
}

/// §6.2.7's `read_interpolation_filter`.
fn read_interpolation_filter(r: &mut BitReader<'_>) -> i32 {
    if r.get(1) != 0 {
        tables::SWITCHABLE
    } else {
        let raw = usize::try_from(r.get(2)).unwrap_or(0);
        tables::LITERAL_TO_TYPE
            .get(raw)
            .copied()
            .unwrap_or(tables::EIGHTTAP)
    }
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
    // -- Inter-only adaptive tables (C-31): forward-updated by
    // -- `parse_compressed_header` only when `!frame_is_intra` (§6.3's own
    // -- syntax table gates every one of these behind that condition).
    pub inter_mode_probs: [[u8; 3]; 7],
    pub interp_filter_probs: [[u8; 2]; 4],
    pub is_inter_prob: [u8; 4],
    pub comp_mode_prob: [u8; 5],
    pub single_ref_prob: [[u8; 2]; 5],
    pub comp_ref_prob: [u8; 5],
    pub y_mode_probs: [[u8; 9]; 4],
    pub uv_mode_probs: [[u8; 9]; 10],
    pub partition_probs: [[u8; 3]; 16],
    pub mv_joint_probs: [u8; 3],
    pub mv_sign_prob: [u8; 2],
    pub mv_class_probs: [[u8; 10]; 2],
    pub mv_class0_bit_prob: [u8; 2],
    pub mv_bits_prob: [[u8; tables::MV_OFFSET_BITS]; 2],
    pub mv_class0_fr_probs: [[[u8; 3]; tables::CLASS0_SIZE]; 2],
    pub mv_fr_probs: [[u8; 3]; 2],
    pub mv_class0_hp_prob: [u8; 2],
    pub mv_hp_prob: [u8; 2],
}

impl Default for EntropyContext {
    fn default() -> Self {
        Self {
            coef_probs: tables::DEFAULT_COEF_PROBS,
            skip_prob: tables::DEFAULT_SKIP_PROB,
            tx_probs: tables::DEFAULT_TX_PROBS,
            inter_mode_probs: tables::DEFAULT_INTER_MODE_PROBS,
            interp_filter_probs: tables::DEFAULT_INTERP_FILTER_PROBS,
            is_inter_prob: tables::DEFAULT_IS_INTER_PROB,
            comp_mode_prob: tables::DEFAULT_COMP_MODE_PROB,
            single_ref_prob: tables::DEFAULT_SINGLE_REF_PROB,
            comp_ref_prob: tables::DEFAULT_COMP_REF_PROB,
            y_mode_probs: tables::DEFAULT_Y_MODE_PROBS,
            uv_mode_probs: tables::DEFAULT_UV_MODE_PROBS,
            partition_probs: tables::DEFAULT_PARTITION_PROBS,
            mv_joint_probs: tables::DEFAULT_MV_JOINT_PROBS,
            mv_sign_prob: tables::DEFAULT_MV_SIGN_PROB,
            mv_class_probs: tables::DEFAULT_MV_CLASS_PROBS,
            mv_class0_bit_prob: tables::DEFAULT_MV_CLASS0_BIT_PROB,
            mv_bits_prob: tables::DEFAULT_MV_BITS_PROB,
            mv_class0_fr_probs: tables::DEFAULT_MV_CLASS0_FR_PROBS,
            mv_fr_probs: tables::DEFAULT_MV_FR_PROBS,
            mv_class0_hp_prob: tables::DEFAULT_MV_CLASS0_HP_PROB,
            mv_hp_prob: tables::DEFAULT_MV_HP_PROB,
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
    if v & 1 != 0 {
        m - ((v + 1) >> 1)
    } else {
        m + (v >> 1)
    }
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

/// `parse_compressed_header`'s inter-only results (§6.3.12's
/// `reference_mode`/§6.3.18's `CompFixedRef`/`CompVarRef`), alongside the
/// `tx_mode` a key frame's compressed header also decodes.
#[derive(Debug, Clone, Copy)]
pub struct CompressedHeaderInfo {
    pub tx_mode: i32,
    pub reference_mode: i32,
    pub comp_fixed_ref: i32,
    pub comp_var_ref: [i32; 2],
}

/// §6.3's `compressed_header()`. `frame_is_intra` gates every inter-only
/// probability table behind a condition that is always false for a real
/// key frame (see §6.3's own syntax table) — those reads, and
/// `reference_mode`/`CompFixedRef`/`CompVarRef`, only run when it is false.
#[allow(clippy::too_many_arguments, reason = "one linear syntax table, §6.3")]
pub fn parse_compressed_header(
    bd: &mut Bd<'_>,
    lossless: bool,
    frame_is_intra: bool,
    allow_high_precision_mv: bool,
    ref_frame_sign_bias: [bool; 4],
    interpolation_filter: i32,
    entropy: &mut EntropyContext,
) -> CompressedHeaderInfo {
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
    let mut info = CompressedHeaderInfo {
        tx_mode,
        reference_mode: tables::SINGLE_REFERENCE,
        comp_fixed_ref: 0,
        comp_var_ref: [0; 2],
    };
    if !frame_is_intra {
        read_inter_mode_probs(bd, entropy);
        // §6.3's `compressed_header()`: `read_interp_filter_probs()` is only
        // called `if (interpolation_filter == SWITCHABLE)` — reading it
        // unconditionally consumes bits the encoder never wrote whenever a
        // frame fixes one filter for its whole duration, desyncing every
        // read after this point in the SAME compressed header (is_inter,
        // reference_mode, y_mode_probs, partition_probs, mv_probs all read
        // garbage deltas from then on, even though each of their own
        // formulas is correct in isolation).
        if interpolation_filter == tables::SWITCHABLE {
            read_interp_filter_probs(bd, entropy);
        }
        read_is_inter_probs(bd, entropy);
        frame_reference_mode(bd, ref_frame_sign_bias, &mut info);
        frame_reference_mode_probs(bd, &info, entropy);
        read_y_mode_probs(bd, entropy);
        read_partition_probs(bd, entropy);
        mv_probs(bd, allow_high_precision_mv, entropy);
    }
    info
}

/// §6.3.14's `read_y_mode_probs`.
fn read_y_mode_probs(bd: &mut Bd<'_>, entropy: &mut EntropyContext) {
    for row in &mut entropy.y_mode_probs {
        for slot in row.iter_mut() {
            *slot = diff_update_prob(bd, *slot);
        }
    }
}

/// §6.3.15's `read_partition_probs`.
fn read_partition_probs(bd: &mut Bd<'_>, entropy: &mut EntropyContext) {
    for row in &mut entropy.partition_probs {
        for slot in row.iter_mut() {
            *slot = diff_update_prob(bd, *slot);
        }
    }
}

/// §6.3.9's `read_inter_mode_probs`.
fn read_inter_mode_probs(bd: &mut Bd<'_>, entropy: &mut EntropyContext) {
    for row in &mut entropy.inter_mode_probs {
        for slot in row.iter_mut() {
            *slot = diff_update_prob(bd, *slot);
        }
    }
}

/// §6.3.10's `read_interp_filter_probs`.
fn read_interp_filter_probs(bd: &mut Bd<'_>, entropy: &mut EntropyContext) {
    for row in &mut entropy.interp_filter_probs {
        for slot in row.iter_mut() {
            *slot = diff_update_prob(bd, *slot);
        }
    }
}

/// §6.3.11's `read_is_inter_probs`.
fn read_is_inter_probs(bd: &mut Bd<'_>, entropy: &mut EntropyContext) {
    for slot in &mut entropy.is_inter_prob {
        *slot = diff_update_prob(bd, *slot);
    }
}

/// §6.3.12's `frame_reference_mode`.
fn frame_reference_mode(bd: &mut Bd<'_>, sign_bias: [bool; 4], info: &mut CompressedHeaderInfo) {
    let last = sign_bias
        .get(usize::try_from(tables::LAST_FRAME).unwrap_or(0))
        .copied()
        .unwrap_or(false);
    let mut compound_reference_allowed = false;
    for i in 1..tables::REFS_PER_FRAME {
        let bias = sign_bias.get(i + 1).copied().unwrap_or(false);
        if bias != last {
            compound_reference_allowed = true;
        }
    }
    info.reference_mode = if compound_reference_allowed {
        if bd.read_bool(128) {
            let mode = if bd.read_bool(128) {
                tables::REFERENCE_MODE_SELECT
            } else {
                tables::COMPOUND_REFERENCE
            };
            setup_compound_reference_mode(sign_bias, info);
            mode
        } else {
            tables::SINGLE_REFERENCE
        }
    } else {
        tables::SINGLE_REFERENCE
    };
}

/// §6.3.18's `setup_compound_reference_mode`.
fn setup_compound_reference_mode(sign_bias: [bool; 4], info: &mut CompressedHeaderInfo) {
    let bias = |rf: i32| {
        sign_bias
            .get(usize::try_from(rf).unwrap_or(0))
            .copied()
            .unwrap_or(false)
    };
    if bias(tables::LAST_FRAME) == bias(tables::GOLDEN_FRAME) {
        info.comp_fixed_ref = tables::ALTREF_FRAME;
        info.comp_var_ref = [tables::LAST_FRAME, tables::GOLDEN_FRAME];
    } else if bias(tables::LAST_FRAME) == bias(tables::ALTREF_FRAME) {
        info.comp_fixed_ref = tables::GOLDEN_FRAME;
        info.comp_var_ref = [tables::LAST_FRAME, tables::ALTREF_FRAME];
    } else {
        info.comp_fixed_ref = tables::LAST_FRAME;
        info.comp_var_ref = [tables::GOLDEN_FRAME, tables::ALTREF_FRAME];
    }
}

/// §6.3.13's `frame_reference_mode_probs`.
fn frame_reference_mode_probs(
    bd: &mut Bd<'_>,
    info: &CompressedHeaderInfo,
    entropy: &mut EntropyContext,
) {
    if info.reference_mode == tables::REFERENCE_MODE_SELECT {
        for slot in &mut entropy.comp_mode_prob {
            *slot = diff_update_prob(bd, *slot);
        }
    }
    if info.reference_mode != tables::COMPOUND_REFERENCE {
        for row in &mut entropy.single_ref_prob {
            for slot in row.iter_mut() {
                *slot = diff_update_prob(bd, *slot);
            }
        }
    }
    if info.reference_mode != tables::SINGLE_REFERENCE {
        for slot in &mut entropy.comp_ref_prob {
            *slot = diff_update_prob(bd, *slot);
        }
    }
}

/// §6.3.17's `update_mv_prob`.
fn update_mv_prob(bd: &mut Bd<'_>, prob: u8) -> u8 {
    if bd.read_bool(252) {
        let mv_prob = u8::try_from(bd.read_literal(7)).unwrap_or(0);
        (mv_prob << 1) | 1
    } else {
        prob
    }
}

/// §6.3.16's `mv_probs`.
fn mv_probs(bd: &mut Bd<'_>, allow_high_precision_mv: bool, entropy: &mut EntropyContext) {
    for slot in &mut entropy.mv_joint_probs {
        *slot = update_mv_prob(bd, *slot);
    }
    for i in 0..2 {
        if let Some(slot) = entropy.mv_sign_prob.get_mut(i) {
            *slot = update_mv_prob(bd, *slot);
        }
        if let Some(row) = entropy.mv_class_probs.get_mut(i) {
            for slot in row.iter_mut() {
                *slot = update_mv_prob(bd, *slot);
            }
        }
        if let Some(slot) = entropy.mv_class0_bit_prob.get_mut(i) {
            *slot = update_mv_prob(bd, *slot);
        }
        if let Some(row) = entropy.mv_bits_prob.get_mut(i) {
            for slot in row.iter_mut() {
                *slot = update_mv_prob(bd, *slot);
            }
        }
    }
    for i in 0..2 {
        if let Some(rows) = entropy.mv_class0_fr_probs.get_mut(i) {
            for row in rows.iter_mut() {
                for slot in row.iter_mut() {
                    *slot = update_mv_prob(bd, *slot);
                }
            }
        }
        if let Some(row) = entropy.mv_fr_probs.get_mut(i) {
            for slot in row.iter_mut() {
                *slot = update_mv_prob(bd, *slot);
            }
        }
    }
    if allow_high_precision_mv {
        for i in 0..2 {
            if let Some(slot) = entropy.mv_class0_hp_prob.get_mut(i) {
                *slot = update_mv_prob(bd, *slot);
            }
            if let Some(slot) = entropy.mv_hp_prob.get_mut(i) {
                *slot = update_mv_prob(bd, *slot);
            }
        }
    }
}

fn read_coef_probs(bd: &mut Bd<'_>, tx_mode: i32, entropy: &mut EntropyContext) {
    let max_tx_size = tables::TX_MODE_TO_BIGGEST_TX_SIZE
        .get(usize::try_from(tx_mode).unwrap_or(0))
        .copied()
        .unwrap_or(0);
    for tx_sz in 0..=max_tx_size {
        let update = bd.read_literal(1) != 0;
        if !update {
            continue;
        }
        let Some(tx_table) = entropy
            .coef_probs
            .get_mut(usize::try_from(tx_sz).unwrap_or(0))
        else {
            continue;
        };
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
#[allow(
    clippy::expect_used,
    reason = "test code exercising a fixed real-encoder fixture, not the untrusted-input surface"
)]
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

    /// A real `libvpx-vp9` profile-1 4:2:2 inter frame (`ffmpeg -f lavfi -i
    /// testsrc2=size=160x96:rate=5:duration=1.6 -pix_fmt yuv422p -c:v
    /// libvpx-vp9 -profile:v 1 -frame-parallel 1 -error-resilient max
    /// -lag-in-frames 0 -b:v 500k -g 30`, second IVF frame payload, in full
    /// (1429 bytes)): a regular inter frame's
    /// own bits never re-signal `color_config()` (only a key frame or a
    /// profile>0 intra-only frame do), so before `prev_color` existed as a
    /// parameter here, this frame's parsed `color` was a hardcoded 4:2:0
    /// 8-bit literal regardless of what was passed in, silently breaking
    /// every profile-1/2/3 stream's chroma/bit-depth plumbing from the
    /// second frame onward (invisible on profile 0, where that literal
    /// happens to already be correct).
    fn real_inter_frame_profile1_yuv422() -> Vec<u8> {
        include_bytes!("../tests/fixtures/vp9_profile1_yuv422_inter_frame.bin").to_vec()
    }

    #[test]
    fn inter_frame_color_config_carries_forward_from_the_previous_frame() {
        let data = real_inter_frame_profile1_yuv422();
        let established = ColorConfig {
            bit_depth: 8,
            color_space: 1,
            full_range: false,
            subsampling_x: true,
            subsampling_y: false,
        };
        let ref_dims: [Option<RefFrameDims>; tables::NUM_REF_FRAMES] = [Some(RefFrameDims {
            width: 160,
            height: 96,
        });
            tables::NUM_REF_FRAMES];
        let prev_frame = Some(PrevFrameInfo {
            width: 160,
            height: 96,
            show_frame: true,
        });
        let (fh, _) = parse_uncompressed_header(
            &data,
            LoopFilterParams::default(),
            Segmentation::default(),
            &ref_dims,
            prev_frame,
            established,
        )
        .expect("a real encoder's inter-frame header parses");
        assert!(!fh.is_key_frame);
        assert!(!fh.intra_only);
        assert_eq!(
            fh.color, established,
            "a regular inter frame must carry the sequence's color config forward, not reset it"
        );
    }

    #[test]
    fn inter_frame_color_config_does_not_silently_default_to_4_2_0() {
        // Same frame, deliberately wrong `prev_color` (4:2:0) — proves the
        // previous test is reading `prev_color` through, not merely
        // matching it by coincidence with some other hardcoded value.
        let data = real_inter_frame_profile1_yuv422();
        let wrong_default = ColorConfig {
            bit_depth: 8,
            color_space: 1,
            full_range: false,
            subsampling_x: true,
            subsampling_y: true,
        };
        let ref_dims: [Option<RefFrameDims>; tables::NUM_REF_FRAMES] = [Some(RefFrameDims {
            width: 160,
            height: 96,
        });
            tables::NUM_REF_FRAMES];
        let prev_frame = Some(PrevFrameInfo {
            width: 160,
            height: 96,
            show_frame: true,
        });
        let (fh, _) = parse_uncompressed_header(
            &data,
            LoopFilterParams::default(),
            Segmentation::default(),
            &ref_dims,
            prev_frame,
            wrong_default,
        )
        .expect("a real encoder's inter-frame header parses");
        assert_eq!(
            fh.color, wrong_default,
            "an inter frame's color must equal whatever prev_color it was given"
        );
    }
}
