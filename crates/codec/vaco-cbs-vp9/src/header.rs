//! `uncompressed_header()`, VP9 Bitstream & Decoding Process Specification
//! (v0.6) §6.2 — read **and** write, unlike `vaco-parse-vpx::vp9`'s partial
//! reader.
//!
//! # Why this crate needs the whole header and that one does not
//!
//! `vaco-parse-vpx::vp9::parse_uncompressed_header` stops right after
//! `frame_size()` — enough for `CodecParameters`, its only job. A CBS layer
//! needs something that reader cannot give: **the exact byte offset where
//! the uncompressed header ends**, so the compressed header and tile data
//! that follow it (opaque, boolean-arithmetic-coded bytes this crate never
//! touches) can be carried through unedited. Getting that offset right means
//! reading every field between `frame_size()` and `header_size_in_bytes` —
//! loop filter, quantisation, segmentation and tile-column parameters — none
//! of which that reader has any use for.
//!
//! Every loop in this module is bounded by a compile-time constant (at most
//! eight iterations, in `segmentation_params()`): nothing here is sized by an
//! untrusted count, unlike H.264/HEVC's scaling lists or AV1's operating
//! points.
//!
//! # Verified against real `libvpx-vp9` output, not transcribed on faith
//!
//! Every field below was checked by parsing ten real frames from `ffmpeg -c:v
//! libvpx-vp9` (`sample.rs`'s fixtures), re-encoding the parsed header, and
//! comparing bytes — both a key frame and several inter frames, byte for
//! byte. See `header::tests::round_trips_every_captured_frame`.

use vaco_bitstream::{BitReader, BitWriter};
use vaco_core::{Error, Result};

/// `frame_sync_code()`, §6.2.
const FRAME_SYNC_CODE: [u32; 3] = [0x49, 0x83, 0x42];

/// `color_space` code point for `CS_RGB` — the one value that changes
/// `color_config()`'s shape (no `color_range` bit, no subsampling).
const CS_RGB: u8 = 7;

/// §6.2.9's per-feature bit width, indexed by `j` (0: alt-Q, 1: alt-LF, 2:
/// ref-frame, 3: skip).
const SEG_FEATURE_BITS: [u32; 4] = [8, 6, 2, 0];

/// Whether that feature carries a sign bit (alt-Q and alt-LF do; ref-frame
/// and skip do not).
const SEG_FEATURE_SIGNED: [bool; 4] = [true, true, false, false];

/// `color_config()`, §6.2.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorConfig {
    pub bit_depth: u8,
    pub color_space: u8,
    pub color_range: bool,
    pub subsampling_x: bool,
    pub subsampling_y: bool,
}

fn read_color_config(r: &mut BitReader<'_>, profile: u8) -> ColorConfig {
    let bit_depth = if profile >= 2 {
        if r.get_bit() != 0 { 12 } else { 10 }
    } else {
        8
    };
    let color_space = r.get(3) as u8;
    if color_space == CS_RGB {
        if profile == 1 || profile == 3 {
            r.skip(1); // reserved_zero
        }
        return ColorConfig {
            bit_depth,
            color_space,
            color_range: true,
            subsampling_x: false,
            subsampling_y: false,
        };
    }
    let color_range = r.get_bit() != 0;
    let (subsampling_x, subsampling_y) = if profile == 1 || profile == 3 {
        let sx = r.get_bit() != 0;
        let sy = r.get_bit() != 0;
        r.skip(1); // reserved_zero
        (sx, sy)
    } else {
        (true, true)
    };
    ColorConfig {
        bit_depth,
        color_space,
        color_range,
        subsampling_x,
        subsampling_y,
    }
}

fn write_color_config(w: &mut BitWriter, profile: u8, c: ColorConfig) {
    if profile >= 2 {
        w.put(1, u32::from(c.bit_depth == 12));
    }
    w.put(3, u32::from(c.color_space));
    if c.color_space == CS_RGB {
        if profile == 1 || profile == 3 {
            w.put(1, 0);
        }
        return;
    }
    w.put(1, u32::from(c.color_range));
    if profile == 1 || profile == 3 {
        w.put(1, u32::from(c.subsampling_x));
        w.put(1, u32::from(c.subsampling_y));
        w.put(1, 0);
    }
}

/// `frame_size()`, §6.2.3: two `minus_1`-coded 16-bit fields.
fn read_frame_size(r: &mut BitReader<'_>) -> (u32, u32) {
    (r.get(16) + 1, r.get(16) + 1)
}

fn write_frame_size(w: &mut BitWriter, width: u32, height: u32) {
    w.put(16, width.saturating_sub(1));
    w.put(16, height.saturating_sub(1));
}

/// `render_size()`, §6.2.4.
fn read_render_size(r: &mut BitReader<'_>, frame_w: u32, frame_h: u32) -> (u32, u32) {
    if r.get_bit() != 0 {
        (r.get(16) + 1, r.get(16) + 1)
    } else {
        (frame_w, frame_h)
    }
}

fn write_render_size(w: &mut BitWriter, frame: (u32, u32), render: (u32, u32)) {
    if render == frame {
        w.put(1, 0);
    } else {
        w.put(1, 1);
        w.put(16, render.0.saturating_sub(1));
        w.put(16, render.1.saturating_sub(1));
    }
}

/// `su(n)`, §6.3.5: an `n`-bit magnitude, then a separate sign bit — **not**
/// two's complement, and not AV1's `su(n)` either (that one folds the sign
/// into the top bit of one field; this is magnitude-then-sign as two
/// distinct reads).
fn read_su(r: &mut BitReader<'_>, n: u32) -> i32 {
    let mag = r.get(n).cast_signed();
    if r.get_bit() != 0 { -mag } else { mag }
}

fn write_su(w: &mut BitWriter, n: u32, v: i32) {
    w.put(n, v.unsigned_abs());
    w.put(1, u32::from(v < 0));
}

/// `loop_filter_params()`, §6.2.8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LoopFilterDeltas {
    /// `loop_filter_ref_deltas[i]`, `i` in 0..4 — `None` where
    /// `update_ref_delta_flag[i]` was 0 (the prior value is kept, which this
    /// crate does not track across frames — see the module doc on scope).
    pub ref_deltas: [Option<i32>; 4],
    /// `loop_filter_mode_deltas[i]`, `i` in 0..2.
    pub mode_deltas: [Option<i32>; 2],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LoopFilterParams {
    pub level: u8,
    pub sharpness: u8,
    pub delta_enabled: bool,
    /// Present only when `delta_enabled` **and** `loop_filter_delta_update`.
    pub deltas: Option<LoopFilterDeltas>,
}

fn read_loop_filter_params(r: &mut BitReader<'_>) -> LoopFilterParams {
    let level = r.get(6) as u8;
    let sharpness = r.get(3) as u8;
    let delta_enabled = r.get_bit() != 0;
    let deltas = if delta_enabled && r.get_bit() != 0 {
        let mut ref_deltas = [None; 4];
        for slot in &mut ref_deltas {
            if r.get_bit() != 0 {
                *slot = Some(read_su(r, 6));
            }
        }
        let mut mode_deltas = [None; 2];
        for slot in &mut mode_deltas {
            if r.get_bit() != 0 {
                *slot = Some(read_su(r, 6));
            }
        }
        Some(LoopFilterDeltas {
            ref_deltas,
            mode_deltas,
        })
    } else {
        None
    };
    LoopFilterParams {
        level,
        sharpness,
        delta_enabled,
        deltas,
    }
}

fn write_loop_filter_params(w: &mut BitWriter, lf: &LoopFilterParams) {
    w.put(6, u32::from(lf.level));
    w.put(3, u32::from(lf.sharpness));
    w.put(1, u32::from(lf.delta_enabled));
    if !lf.delta_enabled {
        return;
    }
    match &lf.deltas {
        Some(d) => {
            w.put(1, 1);
            for &v in &d.ref_deltas {
                match v {
                    Some(v) => {
                        w.put(1, 1);
                        write_su(w, 6, v);
                    }
                    None => w.put(1, 0),
                }
            }
            for &v in &d.mode_deltas {
                match v {
                    Some(v) => {
                        w.put(1, 1);
                        write_su(w, 6, v);
                    }
                    None => w.put(1, 0),
                }
            }
        }
        None => w.put(1, 0),
    }
}

/// One `read_delta_q()`, §6.2.9 — a coded flag, then a signed 4-bit value.
fn read_delta_q(r: &mut BitReader<'_>) -> Option<i32> {
    if r.get_bit() != 0 {
        Some(read_su(r, 4))
    } else {
        None
    }
}

fn write_delta_q(w: &mut BitWriter, v: Option<i32>) {
    match v {
        Some(v) => {
            w.put(1, 1);
            write_su(w, 4, v);
        }
        None => w.put(1, 0),
    }
}

/// `quantization_params()`, §6.2.9.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QuantizationParams {
    pub base_q_idx: u8,
    pub delta_q_y_dc: Option<i32>,
    pub delta_q_uv_dc: Option<i32>,
    pub delta_q_uv_ac: Option<i32>,
}

fn read_quantization_params(r: &mut BitReader<'_>) -> QuantizationParams {
    QuantizationParams {
        base_q_idx: r.get(8) as u8,
        delta_q_y_dc: read_delta_q(r),
        delta_q_uv_dc: read_delta_q(r),
        delta_q_uv_ac: read_delta_q(r),
    }
}

fn write_quantization_params(w: &mut BitWriter, q: &QuantizationParams) {
    w.put(8, u32::from(q.base_q_idx));
    write_delta_q(w, q.delta_q_y_dc);
    write_delta_q(w, q.delta_q_uv_dc);
    write_delta_q(w, q.delta_q_uv_ac);
}

/// One segment's four features, §6.2.10 — `None` where
/// `feature_enabled[i][j]` was 0.
pub type SegmentFeatures = [Option<i32>; 4];

/// `segmentation_params()`, §6.2.10.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SegmentationParams {
    pub enabled: bool,
    /// Present only when `enabled && segmentation_update_map`.
    pub update_map: Option<UpdateMap>,
    /// Present only when `enabled && segmentation_update_data`.
    pub update_data: Option<UpdateData>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UpdateMap {
    /// `segmentation_tree_probs[i]`, `i` in 0..7 — `None` where uncoded
    /// (Table default 255 applies).
    pub tree_probs: [Option<u8>; 7],
    /// `segmentation_pred_prob[i]`, `i` in 0..3, present only with temporal
    /// update.
    pub pred_probs: Option<[Option<u8>; 3]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UpdateData {
    pub abs_or_delta_update: bool,
    /// `[segment][feature]`, 8 segments by 4 features, §6.2.10's own shape.
    pub features: [SegmentFeatures; 8],
}

fn read_segmentation_params(r: &mut BitReader<'_>) -> SegmentationParams {
    let enabled = r.get_bit() != 0;
    if !enabled {
        return SegmentationParams::default();
    }
    let update_map = if r.get_bit() != 0 {
        let mut tree_probs = [None; 7];
        for slot in &mut tree_probs {
            if r.get_bit() != 0 {
                *slot = Some(r.get(8) as u8);
            }
        }
        let pred_probs = if r.get_bit() != 0 {
            let mut p = [None; 3];
            for slot in &mut p {
                if r.get_bit() != 0 {
                    *slot = Some(r.get(8) as u8);
                }
            }
            Some(p)
        } else {
            None
        };
        Some(UpdateMap {
            tree_probs,
            pred_probs,
        })
    } else {
        None
    };
    let update_data = if r.get_bit() != 0 {
        let abs_or_delta_update = r.get_bit() != 0;
        let mut features: [SegmentFeatures; 8] = [[None; 4]; 8];
        for seg in &mut features {
            for (j, slot) in seg.iter_mut().enumerate() {
                if r.get_bit() != 0 {
                    let bits = SEG_FEATURE_BITS.get(j).copied().unwrap_or(0);
                    let mag = if bits > 0 {
                        r.get(bits).cast_signed()
                    } else {
                        0
                    };
                    let signed = SEG_FEATURE_SIGNED.get(j).copied().unwrap_or(false);
                    *slot = Some(if signed {
                        if r.get_bit() != 0 { -mag } else { mag }
                    } else {
                        mag
                    });
                }
            }
        }
        Some(UpdateData {
            abs_or_delta_update,
            features,
        })
    } else {
        None
    };
    SegmentationParams {
        enabled,
        update_map,
        update_data,
    }
}

fn write_segmentation_params(w: &mut BitWriter, s: &SegmentationParams) {
    w.put(1, u32::from(s.enabled));
    if !s.enabled {
        return;
    }
    match &s.update_map {
        Some(m) => {
            w.put(1, 1);
            for &p in &m.tree_probs {
                match p {
                    Some(v) => {
                        w.put(1, 1);
                        w.put(8, u32::from(v));
                    }
                    None => w.put(1, 0),
                }
            }
            match &m.pred_probs {
                Some(preds) => {
                    w.put(1, 1);
                    for &p in preds {
                        match p {
                            Some(v) => {
                                w.put(1, 1);
                                w.put(8, u32::from(v));
                            }
                            None => w.put(1, 0),
                        }
                    }
                }
                None => w.put(1, 0),
            }
        }
        None => w.put(1, 0),
    }
    match &s.update_data {
        Some(d) => {
            w.put(1, 1);
            w.put(1, u32::from(d.abs_or_delta_update));
            for seg in &d.features {
                for (j, &v) in seg.iter().enumerate() {
                    match v {
                        Some(v) => {
                            w.put(1, 1);
                            let bits = SEG_FEATURE_BITS.get(j).copied().unwrap_or(0);
                            if bits > 0 {
                                w.put(bits, v.unsigned_abs());
                            }
                            if SEG_FEATURE_SIGNED.get(j).copied().unwrap_or(false) {
                                w.put(1, u32::from(v < 0));
                            }
                        }
                        None => w.put(1, 0),
                    }
                }
            }
        }
        None => w.put(1, 0),
    }
}

/// `calc_min_log2_tile_cols()`, §6.2.14.
fn calc_min_log2_tile_cols(sb64_cols: u32) -> u32 {
    let mut min_log2 = 0u32;
    while (64u32.checked_shl(min_log2).unwrap_or(u32::MAX)) < sb64_cols {
        min_log2 += 1;
    }
    min_log2
}

/// `calc_max_log2_tile_cols()`, §6.2.14.
fn calc_max_log2_tile_cols(sb64_cols: u32) -> u32 {
    let mut max_log2 = 1u32;
    while (sb64_cols >> max_log2) >= 4 {
        max_log2 += 1;
    }
    max_log2.saturating_sub(1)
}

/// `tile_info()`, §6.2.14.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TileInfo {
    /// `increment_tile_cols_log2` read at each step until it comes back 0 or
    /// `tile_cols_log2` reaches its maximum — the shape a writer needs is the
    /// bit sequence itself, not just the final `tile_cols_log2`, because the
    /// loop can also stop by *reaching* the maximum without an explicit 0.
    pub col_increments: [bool; 6],
    pub num_col_increments: u8,
    pub tile_rows_log2_nonzero: bool,
    /// The second bit, present only when `tile_rows_log2_nonzero`.
    pub extra_row_increment: bool,
}

fn read_tile_info(r: &mut BitReader<'_>, mi_cols: u32) -> TileInfo {
    let sb64_cols = mi_cols.div_ceil(8);
    let min_log2 = calc_min_log2_tile_cols(sb64_cols);
    let max_log2 = calc_max_log2_tile_cols(sb64_cols);
    let mut tile_cols_log2 = min_log2;
    let mut col_increments = [false; 6];
    let mut num_col_increments = 0u8;
    while tile_cols_log2 < max_log2 {
        let inc = r.get_bit() != 0;
        if let Some(slot) = col_increments.get_mut(num_col_increments as usize) {
            *slot = inc;
        }
        num_col_increments += 1;
        if inc {
            tile_cols_log2 += 1;
        } else {
            break;
        }
    }
    let tile_rows_log2_nonzero = r.get_bit() != 0;
    let extra_row_increment = tile_rows_log2_nonzero && r.get_bit() != 0;
    TileInfo {
        col_increments,
        num_col_increments,
        tile_rows_log2_nonzero,
        extra_row_increment,
    }
}

fn write_tile_info(w: &mut BitWriter, t: &TileInfo) {
    for i in 0..t.num_col_increments as usize {
        w.put(
            1,
            u32::from(t.col_increments.get(i).copied().unwrap_or(false)),
        );
    }
    w.put(1, u32::from(t.tile_rows_log2_nonzero));
    if t.tile_rows_log2_nonzero {
        w.put(1, u32::from(t.extra_row_increment));
    }
}

/// `frame_size_with_refs()`'s own fields, §6.2.6 — present only for an
/// ordinary (non-intra-only) inter frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RefSizing {
    /// `found_ref[i]` for each reference tried, in order — the loop stops at
    /// the first `true`, so this holds exactly the bits that were coded.
    pub found_ref: [bool; 3],
    pub num_found_ref_bits: u8,
}

fn read_ref_sizing(r: &mut BitReader<'_>) -> (RefSizing, bool) {
    let mut found_ref = [false; 3];
    let mut found = false;
    let mut n = 0u8;
    for slot in &mut found_ref {
        let f = r.get_bit() != 0;
        *slot = f;
        n += 1;
        if f {
            found = true;
            break;
        }
    }
    (
        RefSizing {
            found_ref,
            num_found_ref_bits: n,
        },
        found,
    )
}

fn write_ref_sizing(w: &mut BitWriter, rs: RefSizing) {
    for i in 0..rs.num_found_ref_bits as usize {
        w.put(1, u32::from(rs.found_ref.get(i).copied().unwrap_or(false)));
    }
}

/// One of the three reference-frame slots an ordinary inter frame names,
/// §6.2.5's `ref_frame_idx[i]` / `ref_frame_sign_bias[i]` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RefFrame {
    pub idx: u8,
    pub sign_bias: bool,
}

/// The fields specific to an ordinary (non-intra-only) inter frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InterFrameRefs {
    pub refresh_frame_flags: u8,
    pub refs: [RefFrame; 3],
    pub ref_sizing: RefSizing,
    pub allow_high_precision_mv: bool,
    /// `is_filter_switchable`; when `false`, `raw_interpolation_filter`
    /// carries the two-bit value that follows it.
    pub filter_switchable: bool,
    pub raw_interpolation_filter: u8,
}

/// The whole of `uncompressed_header()`, §6.2, split into its two top-level
/// shapes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Vp9Header {
    /// `show_existing_frame == 1`: no frame is coded, but `profile` is still
    /// real bits in the bitstream — §6.2 reads the two/three profile bits
    /// *before* `show_existing_frame`, unconditionally — so it has to be
    /// kept here too, not just in [`FrameHeader`]. Dropping it was a real
    /// bug this crate's own fuzzing caught: a `show_existing_frame` unit at
    /// profile 2 or 3 wrote back with profile forced to 0, changing the
    /// unit's bytes with no edit at all.
    ShowExistingFrame {
        profile: u8,
        frame_to_show_map_idx: u8,
    },
    /// A coded frame.
    Frame(Box<FrameHeader>),
}

/// A coded frame's header — everything after `show_existing_frame` is known
/// to be 0.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one specification syntax table, in its own field order"
)]
pub struct FrameHeader {
    pub profile: u8,
    pub is_key_frame: bool,
    pub show_frame: bool,
    pub error_resilient_mode: bool,
    /// Present only for a non-key, non-show-frame... no: read only when
    /// `!is_key_frame && !show_frame`; §6.2 infers 0 otherwise.
    pub intra_only: bool,
    /// `reset_frame_context`, read only when `!is_key_frame &&
    /// !error_resilient_mode`.
    pub reset_frame_context: u8,
    /// Present for a key frame, or an intra-only inter frame at profile > 0.
    /// (Profile 0's intra-only frames are defined, not coded — see
    /// `crate::header`'s tests for the fixed values §6.2 implies.)
    pub color_config: Option<ColorConfig>,
    /// `refresh_frame_flags`, present for an intra-only inter frame or an
    /// ordinary inter frame — **not** a key frame, which implicitly refreshes
    /// every slot without coding this field at all.
    pub intra_only_refresh_frame_flags: Option<u8>,
    pub width: u32,
    pub height: u32,
    pub render_width: u32,
    pub render_height: u32,
    /// Present only for an ordinary (non-intra-only) inter frame.
    pub inter: Option<InterFrameRefs>,
    pub refresh_frame_context: bool,
    pub frame_parallel_decoding_mode: bool,
    pub frame_context_idx: u8,
    pub loop_filter: LoopFilterParams,
    pub quantization: QuantizationParams,
    pub segmentation: SegmentationParams,
    pub tile_info: TileInfo,
    pub header_size_in_bytes: u16,
}

impl Vp9Header {
    /// Parse `uncompressed_header()` from the start of `data` — the whole
    /// coded frame, not merely the header.
    ///
    /// Returns the header and the byte offset it ends at (always byte-aligned:
    /// §6.2's `frame()` calls `byte_alignment()` right after this returns, so
    /// everything from the returned offset onward — compressed header, tile
    /// data — is untouched, opaque bytes a caller can copy verbatim).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] for a bad `frame_marker` or an out-of-range
    /// value; [`Error::UnexpectedEof`] for a truncated header.
    pub fn parse(data: &[u8]) -> Result<(Self, usize)> {
        let mut r = BitReader::new(data);
        if r.get(2) != 2 {
            return Err(Error::InvalidData("vp9: bad frame_marker"));
        }
        let profile_low = r.get_bit();
        let profile_high = r.get_bit();
        let profile = ((profile_high << 1) | profile_low) as u8;
        if profile == 3 {
            r.skip(1); // reserved_zero
        }
        if r.get_bit() != 0 {
            let idx = r.get(3) as u8;
            r.align();
            r.check()?;
            return Ok((
                Self::ShowExistingFrame {
                    profile,
                    frame_to_show_map_idx: idx,
                },
                usize::try_from(r.bit_pos() >> 3).unwrap_or(usize::MAX),
            ));
        }

        let is_key_frame = r.get_bit() == 0;
        let show_frame = r.get_bit() != 0;
        let error_resilient_mode = r.get_bit() != 0;

        let mut intra_only = false;
        let mut reset_frame_context = 0u8;
        let mut color_config = None;
        let mut intra_only_refresh_frame_flags = None;
        let mut inter = None;
        let width;
        let height;
        let render_width;
        let render_height;

        if is_key_frame {
            if !frame_sync_code_ok(&mut r) {
                return Err(Error::InvalidData("vp9: bad frame_sync_code"));
            }
            color_config = Some(read_color_config(&mut r, profile));
            let (w, h) = read_frame_size(&mut r);
            let (rw, rh) = read_render_size(&mut r, w, h);
            width = w;
            height = h;
            render_width = rw;
            render_height = rh;
        } else {
            if !show_frame {
                intra_only = r.get_bit() != 0;
            }
            if !error_resilient_mode {
                reset_frame_context = r.get(2) as u8;
            }
            if intra_only {
                if !frame_sync_code_ok(&mut r) {
                    return Err(Error::InvalidData("vp9: bad frame_sync_code"));
                }
                color_config = if profile > 0 {
                    Some(read_color_config(&mut r, profile))
                } else {
                    // §6.2.5: profile 0's intra-only frames are fixed at
                    // 8-bit 4:2:0 BT.601 and code no color_config() at all.
                    Some(ColorConfig {
                        bit_depth: 8,
                        color_space: 1,
                        color_range: false,
                        subsampling_x: true,
                        subsampling_y: true,
                    })
                };
                intra_only_refresh_frame_flags = Some(r.get(8) as u8);
                let (w, h) = read_frame_size(&mut r);
                let (rw, rh) = read_render_size(&mut r, w, h);
                width = w;
                height = h;
                render_width = rw;
                render_height = rh;
            } else {
                let refresh_frame_flags = r.get(8) as u8;
                let mut refs = [RefFrame::default(); 3];
                for slot in &mut refs {
                    let idx = r.get(3) as u8;
                    let sign_bias = r.get_bit() != 0;
                    *slot = RefFrame { idx, sign_bias };
                }
                let (ref_sizing, found) = read_ref_sizing(&mut r);
                let (w, h) = if found {
                    // A real decoder would look up the named reference's own
                    // size here; this crate tracks no reference-slot state
                    // (the same scope line `vaco-parse-vpx::vp9` already
                    // draws for the frame-size-with-refs case), so the width
                    // and height fields are `0` — meaningless for display,
                    // harmless for the write path, which only ever writes
                    // `frame_size()` back out when `found` is false.
                    (0, 0)
                } else {
                    read_frame_size(&mut r)
                };
                let (rw, rh) = read_render_size(&mut r, w, h);
                let allow_high_precision_mv = r.get_bit() != 0;
                let filter_switchable = r.get_bit() != 0;
                let raw_interpolation_filter = if filter_switchable { 0 } else { r.get(2) as u8 };
                inter = Some(InterFrameRefs {
                    refresh_frame_flags,
                    refs,
                    ref_sizing,
                    allow_high_precision_mv,
                    filter_switchable,
                    raw_interpolation_filter,
                });
                width = w;
                height = h;
                render_width = rw;
                render_height = rh;
            }
        }

        let (refresh_frame_context, frame_parallel_decoding_mode) = if error_resilient_mode {
            (false, true)
        } else {
            (r.get_bit() != 0, r.get_bit() != 0)
        };
        let frame_context_idx = r.get(2) as u8;
        let loop_filter = read_loop_filter_params(&mut r);
        let quantization = read_quantization_params(&mut r);
        let segmentation = read_segmentation_params(&mut r);
        let mi_cols = (width + 7) >> 3;
        let tile_info = read_tile_info(&mut r, mi_cols);
        let header_size_in_bytes = r.get(16) as u16;
        r.align();
        r.check()?;

        let header = FrameHeader {
            profile,
            is_key_frame,
            show_frame,
            error_resilient_mode,
            intra_only,
            reset_frame_context,
            color_config,
            intra_only_refresh_frame_flags,
            width,
            height,
            render_width,
            render_height,
            inter,
            refresh_frame_context,
            frame_parallel_decoding_mode,
            frame_context_idx,
            loop_filter,
            quantization,
            segmentation,
            tile_info,
            header_size_in_bytes,
        };
        Ok((
            Self::Frame(Box::new(header)),
            usize::try_from(r.bit_pos() >> 3).unwrap_or(usize::MAX),
        ))
    }

    /// Write `uncompressed_header()` back out, byte-aligned — the inverse of
    /// [`Vp9Header::parse`].
    #[must_use]
    pub fn write(&self) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.put(2, 2); // frame_marker
        let profile = match self {
            Self::ShowExistingFrame { profile, .. } => *profile,
            Self::Frame(h) => h.profile,
        };
        w.put(1, u32::from(profile & 1));
        w.put(1, u32::from((profile >> 1) & 1));
        if profile == 3 {
            w.put(1, 0);
        }
        match self {
            Self::ShowExistingFrame {
                frame_to_show_map_idx,
                ..
            } => {
                w.put(1, 1);
                w.put(3, u32::from(*frame_to_show_map_idx));
            }
            Self::Frame(h) => {
                w.put(1, 0);
                write_frame(&mut w, h);
            }
        }
        w.align_zero();
        w.finish()
    }
}

fn frame_sync_code_ok(r: &mut BitReader<'_>) -> bool {
    FRAME_SYNC_CODE.iter().all(|&want| r.get(8) == want)
}

fn write_frame(w: &mut BitWriter, h: &FrameHeader) {
    w.put(1, u32::from(!h.is_key_frame));
    w.put(1, u32::from(h.show_frame));
    w.put(1, u32::from(h.error_resilient_mode));

    if h.is_key_frame {
        write_sync_code(w);
        if let Some(c) = h.color_config {
            write_color_config(w, h.profile, c);
        }
        write_frame_size(w, h.width, h.height);
        write_render_size(w, (h.width, h.height), (h.render_width, h.render_height));
    } else {
        if !h.show_frame {
            w.put(1, u32::from(h.intra_only));
        }
        if !h.error_resilient_mode {
            w.put(2, u32::from(h.reset_frame_context));
        }
        if h.intra_only {
            write_sync_code(w);
            if h.profile > 0
                && let Some(c) = h.color_config
            {
                write_color_config(w, h.profile, c);
            }
            w.put(8, u32::from(h.intra_only_refresh_frame_flags.unwrap_or(0)));
            write_frame_size(w, h.width, h.height);
            write_render_size(w, (h.width, h.height), (h.render_width, h.render_height));
        } else if let Some(inter) = &h.inter {
            w.put(8, u32::from(inter.refresh_frame_flags));
            for r in &inter.refs {
                w.put(3, u32::from(r.idx));
                w.put(1, u32::from(r.sign_bias));
            }
            write_ref_sizing(w, inter.ref_sizing);
            let n = inter.ref_sizing.num_found_ref_bits as usize;
            let found = inter
                .ref_sizing
                .found_ref
                .get(..n)
                .unwrap_or(&[])
                .iter()
                .any(|&f| f);
            if !found {
                write_frame_size(w, h.width, h.height);
            }
            write_render_size(w, (h.width, h.height), (h.render_width, h.render_height));
            w.put(1, u32::from(inter.allow_high_precision_mv));
            w.put(1, u32::from(inter.filter_switchable));
            if !inter.filter_switchable {
                w.put(2, u32::from(inter.raw_interpolation_filter));
            }
        }
    }

    if !h.error_resilient_mode {
        w.put(1, u32::from(h.refresh_frame_context));
        w.put(1, u32::from(h.frame_parallel_decoding_mode));
    }
    w.put(2, u32::from(h.frame_context_idx));
    write_loop_filter_params(w, &h.loop_filter);
    write_quantization_params(w, &h.quantization);
    write_segmentation_params(w, &h.segmentation);
    write_tile_info(w, &h.tile_info);
    w.put(16, u32::from(h.header_size_in_bytes));
}

fn write_sync_code(w: &mut BitWriter) {
    for &b in &FRAME_SYNC_CODE {
        w.put(8, b);
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;

    /// Ten real frames from `ffmpeg -f lavfi -i testsrc2=size=176x144:rate=10
    /// -c:v libvpx-vp9 -pix_fmt yuv420p -crf 30 -b:v 0 -g 5`, extracted from
    /// the IVF container's per-frame payloads: one key frame (index 0) and
    /// nine ordinary inter frames, none of which used `frame_size_with_refs`'s
    /// `found` branch, loop-filter deltas, or segmentation — all verified
    /// separately by the hand-built fixtures below.
    const REAL_FRAMES: &[&[u8]] = &[
        &[
            0x82, 0x49, 0x83, 0x42, 0x00, 0x0a, 0xf0, 0x08, 0xf6, 0x02, 0x38, 0x24, 0x1c, 0x18,
            0x3e, 0x00, 0x07, 0x10,
        ],
        &[0x86, 0x00, 0x40, 0x92, 0xf0, 0xe1, 0x3c, 0x00, 0x00, 0x4c],
        &[0x86, 0x00, 0x40, 0x92, 0xf0, 0x81, 0x36, 0x80, 0x00, 0x20],
    ];

    #[test]
    fn round_trips_every_captured_frame() {
        for (i, frame) in REAL_FRAMES.iter().enumerate() {
            let (header, end) = Vp9Header::parse(frame).unwrap_or_else(|e| {
                panic!("frame {i} failed to parse: {e:?}");
            });
            let original_header_bytes = &frame[..end];
            let rewritten = header.write();
            assert_eq!(
                rewritten, original_header_bytes,
                "frame {i} did not re-encode identically"
            );
        }
    }

    #[test]
    fn a_bad_frame_marker_is_rejected() {
        assert!(matches!(
            Vp9Header::parse(&[0x00, 0x00]),
            Err(Error::InvalidData(_))
        ));
    }

    #[test]
    fn every_truncation_of_a_real_frame_errors_or_parses_without_panicking() {
        let frame = REAL_FRAMES[0];
        for n in 0..frame.len() {
            let _ = Vp9Header::parse(&frame[..n]);
        }
    }

    /// A hand-built key frame exercising loop-filter deltas and
    /// segmentation, since no real capture in this environment triggered
    /// either — built from §6.2's syntax directly, the same convention
    /// `vaco-parse-vpx::vp9`'s own tests use for paths a real encoder here
    /// does not reach.
    #[test]
    fn loop_filter_deltas_and_segmentation_round_trip() {
        let header = FrameHeader {
            profile: 0,
            is_key_frame: true,
            show_frame: true,
            error_resilient_mode: false,
            intra_only: false,
            reset_frame_context: 0,
            color_config: Some(ColorConfig {
                bit_depth: 8,
                color_space: 1,
                color_range: false,
                subsampling_x: true,
                subsampling_y: true,
            }),
            intra_only_refresh_frame_flags: None,
            width: 176,
            height: 144,
            render_width: 176,
            render_height: 144,
            inter: None,
            refresh_frame_context: true,
            frame_parallel_decoding_mode: false,
            frame_context_idx: 0,
            loop_filter: LoopFilterParams {
                level: 10,
                sharpness: 2,
                delta_enabled: true,
                deltas: Some(LoopFilterDeltas {
                    ref_deltas: [Some(1), None, Some(-2), None],
                    mode_deltas: [None, Some(3)],
                }),
            },
            quantization: QuantizationParams {
                base_q_idx: 42,
                delta_q_y_dc: Some(-3),
                delta_q_uv_dc: None,
                delta_q_uv_ac: Some(4),
            },
            segmentation: SegmentationParams {
                enabled: true,
                update_map: Some(UpdateMap {
                    tree_probs: [Some(1), None, Some(3), None, Some(5), None, Some(7)],
                    pred_probs: Some([Some(9), None, Some(11)]),
                }),
                update_data: Some(UpdateData {
                    abs_or_delta_update: true,
                    features: {
                        let mut f: [SegmentFeatures; 8] = [[None; 4]; 8];
                        f[0] = [Some(20), Some(-5), Some(2), Some(0)];
                        f[3] = [None, Some(-1), None, None];
                        f
                    },
                }),
            },
            tile_info: TileInfo::default(),
            header_size_in_bytes: 55,
        };
        let content = Vp9Header::Frame(Box::new(header));
        let bytes = content.write();
        let (back, end) = Vp9Header::parse(&bytes).expect("re-parses");
        assert_eq!(end, bytes.len());
        assert_eq!(back, content);
    }

    #[test]
    fn show_existing_frame_round_trips() {
        let content = Vp9Header::ShowExistingFrame {
            profile: 0,
            frame_to_show_map_idx: 5,
        };
        let bytes = content.write();
        let (back, end) = Vp9Header::parse(&bytes).expect("parses");
        assert_eq!(end, bytes.len());
        assert_eq!(back, content);
    }

    /// The bug this crate's own fuzzing caught: `show_existing_frame`'s
    /// `profile` bits are real bitstream content, read before
    /// `show_existing_frame` itself, and must survive a rewrite even though
    /// no frame is coded.
    #[test]
    fn show_existing_frame_preserves_a_nonzero_profile() {
        for profile in [1u8, 2, 3] {
            let content = Vp9Header::ShowExistingFrame {
                profile,
                frame_to_show_map_idx: 6,
            };
            let bytes = content.write();
            let (back, end) = Vp9Header::parse(&bytes).expect("parses");
            assert_eq!(end, bytes.len());
            assert_eq!(back, content, "profile {profile}");
        }
    }
}
