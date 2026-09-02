//! `uncompressed_header()`'s intra path in full, AV1 spec §5.9.1–§5.9.24.
//!
//! # Scope: `FrameIsIntra` only
//!
//! `vaco-parse-av1::frame_header` already parses the common prefix through
//! `frame_size()`/`render_size()`/`allow_intrabc` for stream identification
//! (D14.1: this crate depends on it for OBU/av1C/sequence-header parsing).
//! It stops there because a *parser* has no use for tile geometry or
//! quantization parameters. A *decoder* needs all of it, so this module
//! re-derives the same common prefix (duplicating roughly a hundred lines
//! of bit-for-bit-identical logic) and continues through every syntax
//! structure a real decode of an intra or intra-only frame touches:
//! `tile_info()`, `quantization_params()`, `segmentation_params()`,
//! `delta_q_params()`/`delta_lf_params()`, `loop_filter_params()`/
//! `cdef_params()`/`lr_params()` (parsed for correct bit alignment; not
//! *applied* — deblocking/CDEF/restoration are issue #35, another agent),
//! `read_tx_mode()`, `frame_reference_mode()`/`skip_mode_params()`/
//! `global_motion_params()` (all three are no-op reads for an intra frame,
//! kept only so the syntax order matches the specification exactly),
//! `reduced_tx_set` and `film_grain_params()` (parsed in full for bit
//! alignment; grain synthesis is issue #343).
//!
//! An inter frame's header (`FrameIsIntra == 0`) is rejected with
//! [`Error::Unsupported`] before any inter-only syntax is touched, rather
//! than guessed at — inter prediction is issue #34, another agent's scope.
//!
//! `Vaco-Spec-Ref: aom-av1-spec §5.9 (frame header OBU syntax)`.

use vaco_bitstream::BitReader;
use vaco_core::{Error, Result};
use vaco_parse_av1::leb::{ns, su};
use vaco_parse_av1::seq::{NUM_REF_FRAMES, SELECT_VALUE};
pub use vaco_parse_av1::{FrameSize, FrameType, SequenceHeader};

/// `PRIMARY_REF_NONE`, §3.
pub const PRIMARY_REF_NONE: u32 = 7;
/// `TOTAL_REFS_PER_FRAME`, §3.
pub const TOTAL_REFS_PER_FRAME: usize = 8;
/// `MAX_SEGMENTS`, §3.
pub const MAX_SEGMENTS: usize = 8;
/// `SEG_LVL_MAX`, §3.
pub const SEG_LVL_MAX: usize = 8;
/// `SEG_LVL_ALT_Q`, §3: the segment feature index for a per-segment
/// quantizer delta, the only one [`crate`]'s dequantization reads.
pub const SEG_LVL_ALT_Q: usize = 0;
/// `MAX_LOOP_FILTER`, §3.
pub const MAX_LOOP_FILTER: i32 = 63;
/// `MAX_TILE_WIDTH`/`MAX_TILE_AREA`/`MAX_TILE_COLS`/`MAX_TILE_ROWS`, §3.
const MAX_TILE_WIDTH: u32 = 4096;
const MAX_TILE_AREA: u32 = 4096 * 2304;
const MAX_TILE_COLS: u32 = 64;
const MAX_TILE_ROWS: u32 = 64;

/// `frame_type`, §6.8.2 — `vaco_parse_av1::FrameType`'s bit mapping,
/// re-derived here since it is not exported publicly from that crate (this
/// crate parses the frame header's bits itself rather than reusing its
/// partial parse; see the module doc).
const fn frame_type_from_bits(v: u32) -> FrameType {
    match v {
        0 => FrameType::Key,
        2 => FrameType::IntraOnly,
        3 => FrameType::Switch,
        _ => FrameType::Inter,
    }
}

/// `Segmentation_Feature_Bits`/`Signed`/`Max`, §5.9.14.
const SEG_FEATURE_BITS: [u32; SEG_LVL_MAX] = [8, 6, 6, 6, 6, 3, 0, 0];
const SEG_FEATURE_SIGNED: [bool; SEG_LVL_MAX] = [true, true, true, true, true, false, false, false];
fn seg_feature_max(j: usize) -> i32 {
    match j {
        0 => 255,
        1..=4 => MAX_LOOP_FILTER,
        5 => 7,
        _ => 0,
    }
}

/// `segmentation_params()`, §5.9.14.
#[derive(Debug, Clone)]
pub struct Segmentation {
    pub enabled: bool,
    /// `FeatureEnabled[segment][feature]`.
    pub feature_enabled: [[bool; SEG_LVL_MAX]; MAX_SEGMENTS],
    /// `FeatureData[segment][feature]`.
    pub feature_data: [[i32; SEG_LVL_MAX]; MAX_SEGMENTS],
}

impl Segmentation {
    fn disabled() -> Self {
        Self {
            enabled: false,
            feature_enabled: [[false; SEG_LVL_MAX]; MAX_SEGMENTS],
            feature_data: [[0; SEG_LVL_MAX]; MAX_SEGMENTS],
        }
    }

    #[must_use]
    pub fn feature_active(&self, segment_id: usize, feature: usize) -> bool {
        self.enabled
            && self
                .feature_enabled
                .get(segment_id)
                .and_then(|f| f.get(feature))
                .copied()
                .unwrap_or(false)
    }

    #[must_use]
    pub fn feature_data(&self, segment_id: usize, feature: usize) -> i32 {
        self.feature_data
            .get(segment_id)
            .and_then(|f| f.get(feature))
            .copied()
            .unwrap_or(0)
    }
}

/// `tile_info()`, §5.9.15.
#[derive(Debug, Clone)]
pub struct Av1TileInfo {
    pub cols: usize,
    pub rows: usize,
    pub cols_log2: u32,
    pub rows_log2: u32,
    /// `MiColStarts[0..=cols]`.
    pub mi_col_starts: Vec<u32>,
    /// `MiRowStarts[0..=rows]`.
    pub mi_row_starts: Vec<u32>,
    pub context_update_tile_id: u32,
    pub tile_size_bytes: u32,
}

/// `quantization_params()`, §5.9.12.
#[derive(Debug, Clone, Copy)]
pub struct QuantizationParams {
    pub base_q_idx: u8,
    pub delta_q_y_dc: i32,
    pub delta_q_u_dc: i32,
    pub delta_q_u_ac: i32,
    pub delta_q_v_dc: i32,
    pub delta_q_v_ac: i32,
    pub using_qmatrix: bool,
}

/// `delta_q_params()`/`delta_lf_params()`, §5.9.17–§5.9.18.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeltaParams {
    pub delta_q_present: bool,
    pub delta_q_res: u32,
    pub delta_lf_present: bool,
    pub delta_lf_res: u32,
    pub delta_lf_multi: bool,
}

/// `TxMode`, §6.8.21.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxMode {
    Only4x4,
    Largest,
    Select,
}

/// What this crate parses of `frame_header_obu()` for the intra path.
#[derive(Debug, Clone)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is one independent field of uncompressed_header()'s own syntax table; \
              grouping them into enums would invent structure the specification does not have"
)]
pub struct FrameHeader {
    pub frame_type: FrameType,
    pub show_frame: bool,
    pub error_resilient_mode: bool,
    pub disable_cdf_update: bool,
    pub disable_frame_end_update_cdf: bool,
    pub allow_screen_content_tools: bool,
    pub allow_intrabc: bool,
    pub size: FrameSize,
    pub tile_info: Av1TileInfo,
    pub quant: QuantizationParams,
    pub segmentation: Segmentation,
    pub delta: DeltaParams,
    pub coded_lossless: bool,
    pub all_lossless: bool,
    /// `LosslessArray[segment]`.
    pub lossless_array: [bool; MAX_SEGMENTS],
    pub tx_mode: TxMode,
    pub reduced_tx_set: bool,
    /// `cdef_bits`, §5.9.19 — how many literal bits `read_cdef()`
    /// (§5.11.56) draws per 64x64 unit in the tile data itself. `0` when
    /// CDEF is off for this frame (`coded_lossless`, `allow_intrabc`, or
    /// the sequence's own `enable_cdef == 0`), in which case `read_cdef()`
    /// is a no-op — matching the specification's own early return, not a
    /// missing value.
    pub cdef_bits: u32,
}

impl FrameHeader {
    /// Parse `uncompressed_header()`'s payload for an intra path.
    ///
    /// `temporal_id`/`spatial_id` come from the OBU extension header (0/0 if
    /// it had none), needed only for the `buffer_removal_time` loop.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] for `show_existing_frame`, any non-intra
    /// `frame_type`, or `allow_intrabc` (intra block copy is not
    /// implemented). [`Error::InvalidData`] if the payload is truncated or a
    /// value is out of range.
    pub fn parse(
        payload: &[u8],
        seq: &SequenceHeader,
        temporal_id: u8,
        spatial_id: u8,
    ) -> Result<Self> {
        let mut r = BitReader::new(payload);
        let result = Self::parse_from_reader(&mut r, seq, temporal_id, spatial_id);
        r.check()
            .map_err(|_| Error::InvalidData("frame_header_obu ran past the end of its payload"))?;
        result
    }

    /// `frame_header_obu()` read from an already-positioned [`BitReader`],
    /// left positioned exactly where the syntax structure ends rather than
    /// requiring its own dedicated payload slice.
    ///
    /// This is the shape `frame_obu()` (§5.10) needs: `frame_header_obu()`
    /// immediately followed, in the same OBU payload, by `byte_alignment()`
    /// and `tile_group_obu()` — a combined `OBU_FRAME` cannot hand this
    /// function its own trimmed byte slice up front the way a standalone
    /// `OBU_FRAME_HEADER` can, since the frame header's own bit length is
    /// exactly what is being determined by parsing it.
    ///
    /// # Errors
    /// As [`FrameHeader::parse`].
    pub fn parse_from_reader(
        r: &mut BitReader<'_>,
        seq: &SequenceHeader,
        temporal_id: u8,
        spatial_id: u8,
    ) -> Result<Self> {
        parse_inner(r, seq, temporal_id, spatial_id)
    }

    /// `get_qindex(1, segmentId)`, §7.12.2 — the `ignoreDeltaQ = 1` form
    /// this crate always uses (`delta_q`'s per-superblock adjustment is
    /// folded in by the tile decode loop's own `CurrentQIndex`, not here).
    #[must_use]
    pub fn base_qindex_for_segment(&self, segment_id: usize) -> i32 {
        if self.segmentation.feature_active(segment_id, SEG_LVL_ALT_Q) {
            let data = self.segmentation.feature_data(segment_id, SEG_LVL_ALT_Q);
            (i32::from(self.quant.base_q_idx) + data).clamp(0, 255)
        } else {
            i32::from(self.quant.base_q_idx)
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "uncompressed_header() is one syntax structure in the specification"
)]
fn parse_inner(
    r: &mut BitReader<'_>,
    seq: &SequenceHeader,
    temporal_id: u8,
    spatial_id: u8,
) -> Result<FrameHeader> {
    let id_len = u32::from(seq.additional_frame_id_length) + u32::from(seq.delta_frame_id_length);

    let (frame_type, show_frame, error_resilient_mode);
    if seq.reduced_still_picture_header {
        frame_type = FrameType::Key;
        show_frame = true;
        error_resilient_mode = false;
    } else {
        let show_existing_frame = r.get_bit() != 0;
        if show_existing_frame {
            return Err(Error::Unsupported(
                "vaco-codec-av1: show_existing_frame is not decoded",
            ));
        }
        frame_type = frame_type_from_bits(r.get(2));
        show_frame = r.get_bit() != 0;
        if show_frame
            && seq.decoder_model_info_present_flag
            && !seq.timing_info.is_some_and(|t| t.equal_picture_interval)
        {
            return Err(Error::Unsupported(
                "vaco-codec-av1: temporal_point_info() is not decoded",
            ));
        }
        let _showable_frame = if show_frame {
            frame_type != FrameType::Key
        } else {
            r.get_bit() != 0
        };
        error_resilient_mode =
            if frame_type == FrameType::Switch || (frame_type == FrameType::Key && show_frame) {
                true
            } else {
                r.get_bit() != 0
            };
    }
    if !frame_type.is_intra() {
        return Err(Error::Unsupported(
            "vaco-codec-av1: inter frames are not decoded",
        ));
    }

    let disable_cdf_update = r.get_bit() != 0;
    let allow_screen_content_tools = if seq.seq_force_screen_content_tools == SELECT_VALUE {
        r.get_bit() != 0
    } else {
        seq.seq_force_screen_content_tools != 0
    };
    // force_integer_mv: read only to consume the right bits; FrameIsIntra
    // forces the value to 1 regardless, and this crate has no inter motion
    // vectors to apply it to.
    if allow_screen_content_tools && seq.seq_force_integer_mv == SELECT_VALUE {
        let _force_integer_mv = r.get_bit() != 0;
    }

    if seq.frame_id_numbers_present_flag {
        let _current_frame_id = r.get(id_len);
    }

    let frame_size_override_flag = if frame_type == FrameType::Switch {
        true
    } else if seq.reduced_still_picture_header {
        false
    } else {
        r.get_bit() != 0
    };

    let _order_hint = r.get(u32::from(seq.order_hint_bits));

    // FrameIsIntra -> primary_ref_frame is always PRIMARY_REF_NONE, so this
    // crate never loads a saved CDF/segmentation-map context from an
    // earlier frame — every frame it decodes starts from the specification
    // defaults, which is also why `disable_frame_end_update_cdf` below is
    // parsed but never acted on.
    let primary_ref_frame = PRIMARY_REF_NONE;

    if seq.decoder_model_info_present_flag {
        let buffer_removal_time_present_flag = r.get_bit() != 0;
        if buffer_removal_time_present_flag {
            for op in &seq.operating_points {
                if !op.decoder_model_present {
                    continue;
                }
                let in_temporal_layer = (op.idc >> temporal_id) & 1 != 0;
                let in_spatial_layer = (op.idc >> (u16::from(spatial_id) + 8)) & 1 != 0;
                if op.idc == 0 || (in_temporal_layer && in_spatial_layer) {
                    let _buffer_removal_time = r.get(u32::from(seq.buffer_removal_time_length));
                }
            }
        }
    }

    let all_frames: u32 = (1u32 << NUM_REF_FRAMES) - 1;
    let refresh_frame_flags =
        if frame_type == FrameType::Switch || (frame_type == FrameType::Key && show_frame) {
            all_frames
        } else {
            r.get(8)
        };
    if error_resilient_mode && seq.enable_order_hint && refresh_frame_flags != all_frames {
        for _ in 0..NUM_REF_FRAMES {
            let _ref_order_hint = r.get(u32::from(seq.order_hint_bits));
        }
    }

    let size = parse_frame_size(r, seq, frame_size_override_flag)?;
    let allow_intrabc = if allow_screen_content_tools && size.upscaled_width == size.coded_width {
        r.get_bit() != 0
    } else {
        false
    };
    if allow_intrabc {
        return Err(Error::Unsupported(
            "vaco-codec-av1: allow_intrabc (intra block copy) is not decoded",
        ));
    }

    let disable_frame_end_update_cdf =
        seq.reduced_still_picture_header || disable_cdf_update || r.get_bit() != 0;

    let num_planes = if seq.color_config.mono_chrome { 1 } else { 3 };
    let mi_cols = 2 * ((size.coded_width + 7) >> 3);
    let mi_rows = 2 * ((size.coded_height + 7) >> 3);

    let tile_info = parse_tile_info(r, seq, mi_cols, mi_rows);
    let quant = parse_quantization_params(r, seq, num_planes)?;
    let segmentation = parse_segmentation_params(r, primary_ref_frame);
    let delta = parse_delta_params(r, quant.base_q_idx, allow_intrabc);

    let mut coded_lossless = true;
    let mut lossless_array = [false; MAX_SEGMENTS];
    for (segment_id, slot) in lossless_array.iter_mut().enumerate() {
        let qindex = if segmentation.feature_active(segment_id, SEG_LVL_ALT_Q) {
            (i32::from(quant.base_q_idx) + segmentation.feature_data(segment_id, SEG_LVL_ALT_Q))
                .clamp(0, 255)
        } else {
            i32::from(quant.base_q_idx)
        };
        let lossless = qindex == 0
            && quant.delta_q_y_dc == 0
            && quant.delta_q_u_ac == 0
            && quant.delta_q_u_dc == 0
            && quant.delta_q_v_ac == 0
            && quant.delta_q_v_dc == 0;
        *slot = lossless;
        if !lossless {
            coded_lossless = false;
        }
    }
    let all_lossless = coded_lossless && size.coded_width == size.upscaled_width;

    parse_loop_filter_params(r, seq, coded_lossless, allow_intrabc, num_planes);
    let cdef_bits = parse_cdef_params(r, seq, coded_lossless, allow_intrabc, num_planes);
    parse_lr_params(r, seq, all_lossless, allow_intrabc, num_planes);

    let tx_mode = if coded_lossless {
        TxMode::Only4x4
    } else if r.get_bit() != 0 {
        TxMode::Select
    } else {
        TxMode::Largest
    };
    // frame_reference_mode()/skip_mode_params(): both are unconditional
    // no-op assignments when FrameIsIntra, with no bits read.
    // allow_warped_motion: FrameIsIntra forces 0, no bit read.
    let reduced_tx_set = r.get_bit() != 0;
    // global_motion_params(): FrameIsIntra returns immediately after
    // setting identity defaults, no bits read.
    parse_film_grain_params(r, seq, frame_type, show_frame, false);

    Ok(FrameHeader {
        frame_type,
        show_frame,
        error_resilient_mode,
        disable_cdf_update,
        disable_frame_end_update_cdf,
        allow_screen_content_tools,
        allow_intrabc,
        size,
        tile_info,
        quant,
        segmentation,
        delta,
        coded_lossless,
        all_lossless,
        lossless_array,
        tx_mode,
        reduced_tx_set,
        cdef_bits,
    })
}

const SUPERRES_NUM: u32 = 8;
const SUPERRES_DENOM_MIN: u32 = 9;

#[allow(
    clippy::integer_division,
    reason = "§5.9.7's own rounding-division pseudocode"
)]
fn parse_frame_size(
    r: &mut BitReader<'_>,
    seq: &SequenceHeader,
    frame_size_override_flag: bool,
) -> Result<FrameSize> {
    let (mut frame_width, frame_height) = if frame_size_override_flag {
        (
            r.get(u32::from(seq.frame_width_bits)) + 1,
            r.get(u32::from(seq.frame_height_bits)) + 1,
        )
    } else {
        (seq.max_frame_width, seq.max_frame_height)
    };
    let use_superres = seq.enable_superres && r.get_bit() != 0;
    let superres_denom = if use_superres {
        r.get(3) + SUPERRES_DENOM_MIN
    } else {
        SUPERRES_NUM
    };
    let upscaled_width = frame_width;
    if use_superres {
        frame_width = (upscaled_width * SUPERRES_NUM + superres_denom / 2) / superres_denom.max(1);
    }
    if frame_width == 0 || frame_height == 0 {
        return Err(Error::InvalidData("frame_size() produced a zero dimension"));
    }
    let render_and_frame_size_different = r.get_bit() != 0;
    let (render_width, render_height) = if render_and_frame_size_different {
        (r.get(16) + 1, r.get(16) + 1)
    } else {
        (upscaled_width, frame_height)
    };
    Ok(FrameSize {
        coded_width: frame_width,
        coded_height: frame_height,
        upscaled_width,
        render_width,
        render_height,
        use_superres,
    })
}

fn tile_log2(blk_size: u32, target: u32) -> u32 {
    let mut k = 0u32;
    while (blk_size << k) < target {
        k += 1;
        if k > 32 {
            break;
        }
    }
    k
}

fn parse_tile_info(
    r: &mut BitReader<'_>,
    seq: &SequenceHeader,
    mi_cols: u32,
    mi_rows: u32,
) -> Av1TileInfo {
    let (sb_cols, sb_rows, sb_shift) = if seq.use_128x128_superblock {
        ((mi_cols + 31) >> 5, (mi_rows + 31) >> 5, 5u32)
    } else {
        ((mi_cols + 15) >> 4, (mi_rows + 15) >> 4, 4u32)
    };
    let sb_size = sb_shift + 2;
    let max_tile_width_sb = MAX_TILE_WIDTH >> sb_size;
    let mut max_tile_area_sb = MAX_TILE_AREA >> (2 * sb_size);
    let min_log2_tile_cols = tile_log2(max_tile_width_sb.max(1), sb_cols);
    let max_log2_tile_cols = tile_log2(1, sb_cols.min(MAX_TILE_COLS));
    let max_log2_tile_rows = tile_log2(1, sb_rows.min(MAX_TILE_ROWS));
    let min_log2_tiles = min_log2_tile_cols.max(tile_log2(
        max_tile_area_sb.max(1),
        sb_rows.saturating_mul(sb_cols),
    ));

    let uniform_tile_spacing_flag = r.get_bit() != 0;
    let (mut mi_col_starts, mut mi_row_starts, cols, rows, cols_log2, rows_log2);
    if uniform_tile_spacing_flag {
        let mut tile_cols_log2 = min_log2_tile_cols;
        while tile_cols_log2 < max_log2_tile_cols {
            if r.get_bit() != 0 {
                tile_cols_log2 += 1;
            } else {
                break;
            }
        }
        let tile_width_sb = (sb_cols + (1 << tile_cols_log2) - 1) >> tile_cols_log2;
        mi_col_starts = Vec::new();
        let mut start_sb = 0u32;
        while start_sb < sb_cols {
            mi_col_starts.push(start_sb << sb_shift);
            start_sb += tile_width_sb.max(1);
        }
        mi_col_starts.push(mi_cols);
        cols = mi_col_starts.len().saturating_sub(1);
        cols_log2 = tile_cols_log2;

        let min_log2_tile_rows = min_log2_tiles.saturating_sub(tile_cols_log2);
        let mut tile_rows_log2 = min_log2_tile_rows;
        while tile_rows_log2 < max_log2_tile_rows {
            if r.get_bit() != 0 {
                tile_rows_log2 += 1;
            } else {
                break;
            }
        }
        let tile_height_sb = (sb_rows + (1 << tile_rows_log2) - 1) >> tile_rows_log2;
        mi_row_starts = Vec::new();
        let mut start_sb = 0u32;
        while start_sb < sb_rows {
            mi_row_starts.push(start_sb << sb_shift);
            start_sb += tile_height_sb.max(1);
        }
        mi_row_starts.push(mi_rows);
        rows = mi_row_starts.len().saturating_sub(1);
        rows_log2 = tile_rows_log2;
    } else {
        let mut widest_tile_sb = 0u32;
        let mut start_sb = 0u32;
        mi_col_starts = Vec::new();
        while start_sb < sb_cols {
            mi_col_starts.push(start_sb << sb_shift);
            let max_width = (sb_cols - start_sb).min(max_tile_width_sb.max(1));
            let size_sb = ns(r, max_width.max(1)) + 1;
            widest_tile_sb = size_sb.max(widest_tile_sb);
            start_sb += size_sb;
        }
        mi_col_starts.push(mi_cols);
        cols = mi_col_starts.len().saturating_sub(1);
        cols_log2 = tile_log2(1, u32::try_from(cols).unwrap_or(1));

        if min_log2_tiles > 0 {
            max_tile_area_sb = (sb_rows.saturating_mul(sb_cols)) >> (min_log2_tiles + 1);
        } else {
            max_tile_area_sb = sb_rows.saturating_mul(sb_cols);
        }
        #[allow(
            clippy::integer_division,
            reason = "\u{a7}5.9.15's own maxTileHeightSb = Max(maxTileAreaSb / widestTileSb, 1)"
        )]
        let max_tile_height_sb = (max_tile_area_sb / widest_tile_sb.max(1)).max(1);
        start_sb = 0;
        mi_row_starts = Vec::new();
        while start_sb < sb_rows {
            mi_row_starts.push(start_sb << sb_shift);
            let max_height = (sb_rows - start_sb).min(max_tile_height_sb.max(1));
            let size_sb = ns(r, max_height.max(1)) + 1;
            start_sb += size_sb;
        }
        mi_row_starts.push(mi_rows);
        rows = mi_row_starts.len().saturating_sub(1);
        rows_log2 = tile_log2(1, u32::try_from(rows).unwrap_or(1));
    }

    let (context_update_tile_id, tile_size_bytes) = if cols_log2 > 0 || rows_log2 > 0 {
        let id = r.get(rows_log2 + cols_log2);
        let bytes = r.get(2) + 1;
        (id, bytes)
    } else {
        (0, 1)
    };

    Av1TileInfo {
        cols,
        rows,
        cols_log2,
        rows_log2,
        mi_col_starts,
        mi_row_starts,
        context_update_tile_id,
        tile_size_bytes,
    }
}

fn read_delta_q(r: &mut BitReader<'_>) -> i32 {
    if r.get_bit() != 0 { su(r, 7) } else { 0 }
}

fn parse_quantization_params(
    r: &mut BitReader<'_>,
    seq: &SequenceHeader,
    num_planes: u32,
) -> Result<QuantizationParams> {
    let base_q_idx = u8::try_from(r.get(8)).unwrap_or(0);
    let delta_q_y_dc = read_delta_q(r);
    let (delta_q_u_dc, delta_q_u_ac, delta_q_v_dc, delta_q_v_ac) = if num_planes > 1 {
        let diff_uv_delta = seq.color_config.separate_uv_delta_q && r.get_bit() != 0;
        let u_dc = read_delta_q(r);
        let u_ac = read_delta_q(r);
        if diff_uv_delta {
            (u_dc, u_ac, read_delta_q(r), read_delta_q(r))
        } else {
            (u_dc, u_ac, u_dc, u_ac)
        }
    } else {
        (0, 0, 0, 0)
    };
    let using_qmatrix = r.get_bit() != 0;
    if using_qmatrix {
        let _qm_y = r.get(4);
        let _qm_u = r.get(4);
        if seq.color_config.separate_uv_delta_q {
            let _qm_v = r.get(4);
        }
        // else: qm_v = qm_u, no bits.
        return Err(Error::Unsupported(
            "vaco-codec-av1: using_qmatrix is not decoded",
        ));
    }
    Ok(QuantizationParams {
        base_q_idx,
        delta_q_y_dc,
        delta_q_u_dc,
        delta_q_u_ac,
        delta_q_v_dc,
        delta_q_v_ac,
        using_qmatrix,
    })
}

fn parse_segmentation_params(r: &mut BitReader<'_>, primary_ref_frame: u32) -> Segmentation {
    let segmentation_enabled = r.get_bit() != 0;
    if !segmentation_enabled {
        return Segmentation::disabled();
    }
    let segmentation_update_data = if primary_ref_frame == PRIMARY_REF_NONE {
        true
    } else {
        let segmentation_update_map = r.get_bit() != 0;
        if segmentation_update_map {
            let _segmentation_temporal_update = r.get_bit() != 0;
        }
        r.get_bit() != 0
    };

    let mut feature_enabled = [[false; SEG_LVL_MAX]; MAX_SEGMENTS];
    let mut feature_data = [[0i32; SEG_LVL_MAX]; MAX_SEGMENTS];
    if segmentation_update_data {
        for i in 0..MAX_SEGMENTS {
            for j in 0..SEG_LVL_MAX {
                let enabled = r.get_bit() != 0;
                let mut clipped = 0;
                if enabled {
                    let bits = SEG_FEATURE_BITS.get(j).copied().unwrap_or(0);
                    let limit = seg_feature_max(j);
                    if bits > 0 {
                        clipped = if SEG_FEATURE_SIGNED.get(j).copied().unwrap_or(false) {
                            su(r, bits + 1).clamp(-limit, limit)
                        } else {
                            i32::try_from(r.get(bits))
                                .unwrap_or(i32::MAX)
                                .clamp(0, limit)
                        };
                    }
                }
                if let Some(row) = feature_enabled.get_mut(i)
                    && let Some(slot) = row.get_mut(j)
                {
                    *slot = enabled;
                }
                if let Some(row) = feature_data.get_mut(i)
                    && let Some(slot) = row.get_mut(j)
                {
                    *slot = clipped;
                }
            }
        }
    }
    Segmentation {
        enabled: true,
        feature_enabled,
        feature_data,
    }
}

fn parse_delta_params(r: &mut BitReader<'_>, base_q_idx: u8, allow_intrabc: bool) -> DeltaParams {
    let delta_q_present = base_q_idx > 0 && r.get_bit() != 0;
    let delta_q_res = if delta_q_present { r.get(2) } else { 0 };
    let mut delta = DeltaParams {
        delta_q_present,
        delta_q_res,
        ..DeltaParams::default()
    };
    if delta_q_present {
        delta.delta_lf_present = !allow_intrabc && r.get_bit() != 0;
        if delta.delta_lf_present {
            delta.delta_lf_res = r.get(2);
            delta.delta_lf_multi = r.get_bit() != 0;
        }
    }
    delta
}

fn parse_loop_filter_params(
    r: &mut BitReader<'_>,
    seq: &SequenceHeader,
    coded_lossless: bool,
    allow_intrabc: bool,
    num_planes: u32,
) {
    if coded_lossless || allow_intrabc {
        return;
    }
    let level0 = r.get(6);
    let level1 = r.get(6);
    if num_planes > 1 && (level0 != 0 || level1 != 0) {
        let _level2 = r.get(6);
        let _level3 = r.get(6);
    }
    let _sharpness = r.get(3);
    let delta_enabled = r.get_bit() != 0;
    if delta_enabled {
        let delta_update = r.get_bit() != 0;
        if delta_update {
            for _ in 0..TOTAL_REFS_PER_FRAME {
                if r.get_bit() != 0 {
                    let _delta = su(r, 7);
                }
            }
            for _ in 0..2 {
                if r.get_bit() != 0 {
                    let _delta = su(r, 7);
                }
            }
        }
    }
    let _ = seq;
}

fn parse_cdef_params(
    r: &mut BitReader<'_>,
    seq: &SequenceHeader,
    coded_lossless: bool,
    allow_intrabc: bool,
    num_planes: u32,
) -> u32 {
    if coded_lossless || allow_intrabc || !seq.enable_cdef {
        return 0;
    }
    let _damping = r.get(2);
    let cdef_bits = r.get(2);
    for _ in 0..(1u32 << cdef_bits) {
        let _y_pri = r.get(4);
        let sec = r.get(2);
        let _y_sec = if sec == 3 { sec + 1 } else { sec };
        if num_planes > 1 {
            let _uv_pri = r.get(4);
            let uv_sec = r.get(2);
            let _uv_sec = if uv_sec == 3 { uv_sec + 1 } else { uv_sec };
        }
    }
    cdef_bits
}

fn parse_lr_params(
    r: &mut BitReader<'_>,
    seq: &SequenceHeader,
    all_lossless: bool,
    allow_intrabc: bool,
    num_planes: u32,
) {
    if all_lossless || allow_intrabc || !seq.enable_restoration {
        return;
    }
    let mut uses_lr = false;
    let mut uses_chroma_lr = false;
    for plane in 0..num_planes {
        let lr_type = r.get(2);
        if lr_type != 0 {
            uses_lr = true;
            if plane > 0 {
                uses_chroma_lr = true;
            }
        }
    }
    if uses_lr {
        let lr_unit_shift = r.get_bit();
        let lr_unit_shift = if seq.use_128x128_superblock {
            lr_unit_shift + 1
        } else if lr_unit_shift != 0 {
            lr_unit_shift + r.get_bit()
        } else {
            lr_unit_shift
        };
        let _ = lr_unit_shift;
        if seq.color_config.subsampling_x && seq.color_config.subsampling_y && uses_chroma_lr {
            let _lr_uv_shift = r.get_bit();
        }
    }
}

fn parse_film_grain_params(
    r: &mut BitReader<'_>,
    seq: &SequenceHeader,
    frame_type: FrameType,
    show_frame: bool,
    showable_frame: bool,
) {
    if !seq.film_grain_params_present || (!show_frame && !showable_frame) {
        return;
    }
    let apply_grain = r.get_bit() != 0;
    if !apply_grain {
        return;
    }
    let _grain_seed = r.get(16);
    let update_grain = if frame_type == FrameType::Inter {
        r.get_bit() != 0
    } else {
        true
    };
    if !update_grain {
        let _film_grain_params_ref_idx = r.get(3);
        return;
    }
    let num_y_points = r.get(4);
    for _ in 0..num_y_points {
        let _value = r.get(8);
        let _scaling = r.get(8);
    }
    let chroma_scaling_from_luma = if seq.color_config.mono_chrome {
        false
    } else {
        r.get_bit() != 0
    };
    let (num_cb_points, num_cr_points);
    if seq.color_config.mono_chrome
        || chroma_scaling_from_luma
        || (seq.color_config.subsampling_x && seq.color_config.subsampling_y && num_y_points == 0)
    {
        num_cb_points = 0;
        num_cr_points = 0;
    } else {
        num_cb_points = r.get(4);
        for _ in 0..num_cb_points {
            let _v = r.get(8);
            let _s = r.get(8);
        }
        num_cr_points = r.get(4);
        for _ in 0..num_cr_points {
            let _v = r.get(8);
            let _s = r.get(8);
        }
    }
    let _grain_scaling_minus_8 = r.get(2);
    let ar_coeff_lag = r.get(2);
    let num_pos_luma = 2 * ar_coeff_lag * (ar_coeff_lag + 1);
    let num_pos_chroma = if num_y_points != 0 {
        for _ in 0..num_pos_luma {
            let _c = r.get(8);
        }
        num_pos_luma + 1
    } else {
        num_pos_luma
    };
    if chroma_scaling_from_luma || num_cb_points != 0 {
        for _ in 0..num_pos_chroma {
            let _c = r.get(8);
        }
    }
    if chroma_scaling_from_luma || num_cr_points != 0 {
        for _ in 0..num_pos_chroma {
            let _c = r.get(8);
        }
    }
    let _ar_coeff_shift_minus_6 = r.get(2);
    let _grain_scale_shift = r.get(2);
    if num_cb_points != 0 {
        let _cb_mult = r.get(8);
        let _cb_luma_mult = r.get(8);
        let _cb_offset = r.get(9);
    }
    if num_cr_points != 0 {
        let _cr_mult = r.get(8);
        let _cr_luma_mult = r.get(8);
        let _cr_offset = r.get(9);
    }
    let _overlap_flag = r.get_bit();
    let _clip_to_restricted_range = r.get_bit();
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;
    use vaco_limits::{Budget, Limits};

    fn seq_header() -> SequenceHeader {
        let payload = [
            0x00, 0x00, 0x00, 0x0c, 0xc5, 0x03, 0x65, 0x00, 0xbe, 0x00, 0x10,
        ];
        let mut b = Budget::new(Limits::strict());
        SequenceHeader::parse(&payload, &mut b).expect("a real sequence header parses")
    }

    #[test]
    fn truncation_never_panics() {
        let seq = seq_header();
        let data = [0u8; 24];
        for n in 0..=data.len() {
            let _ = FrameHeader::parse(&data[..n], &seq, 0, 0);
        }
    }

    #[test]
    fn random_bytes_never_panic() {
        let seq = seq_header();
        for seed in 0u8..40 {
            let data: Vec<u8> = (0..24)
                .map(|i: u8| i.wrapping_mul(seed).wrapping_add(7))
                .collect();
            let _ = FrameHeader::parse(&data, &seq, 0, 0);
        }
    }
}
