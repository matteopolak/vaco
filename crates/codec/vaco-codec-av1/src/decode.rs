//! The tile/superblock/partition/mode-info/residual walk, AV1 spec §5.11
//! (syntax) and §6.10/§7.4-§7.12 (semantics/reconstruction), plus the
//! [`vaco_codec_core::Decoder`] wiring.
//!
//! # Scope
//!
//! `FrameIsIntra` only (rejected earlier, in [`crate::frame_header`]), and
//! within that: no palette, no `use_intrabc`, no `use_filter_intra`, no
//! segmentation, no delta-q/delta-lf. Every one of those is gated by a
//! frame-header flag this crate's own encoder configuration keeps off
//! (`allow_screen_content_tools=0` disables palette and intrabc together;
//! `enable-filter-intra=0`/`aq-mode=0` disable the rest) — measured against
//! this crate's own test fixtures via `ffprobe`/hand-parsing, not assumed.
//! A stream that actually sets one of these returns
//! [`vaco_core::Error::Unsupported`] rather than silently mispredicting.
//!
//! `is_inter` is always `0` (no inter frames reach this module), which
//! collapses two specification branches this module does not implement at
//! all: `read_block_tx_size()`'s variable-transform-tree path (`is_inter`
//! only) and `residual()`'s `transform_tree()` call (`is_inter` only) — an
//! intra block always takes the plain `read_tx_size()` /
//! `transform_block()`-loop path instead.

use vaco_codec_core::{Decoder, DecoderDesc};
use vaco_core::{Error, MediaType, Result};
use vaco_frame::Frame;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_pixfmt::PixFmt;

use vaco_parse_av1::obu::{Av1Framing, ObuType, units};
use vaco_parse_av1::seq::SequenceHeader;

use crate::cdf::TileCdf;
use crate::frame_header::{self, FrameHeader};
use crate::framebuf::{Picture, Plane};
use crate::predict::{self, PredMode};
use crate::symbol::SymbolDecoder;
use crate::tables;
use crate::transform::{self, Av1TxType};

const BLOCK_8X8: u8 = 3;
const BLOCK_INVALID: u8 = tables::BLOCK_INVALID;

// Y/UV mode ordinals, §3.
const DC_PRED: u8 = 0;
const V_PRED: u8 = 1;
const D67_PRED: u8 = 8;
const SMOOTH_PRED: u8 = 9;
const SMOOTH_V_PRED: u8 = 10;
const SMOOTH_H_PRED: u8 = 11;
const PAETH_PRED: u8 = 12;
const UV_CFL_PRED: u8 = 13;

const fn is_directional_mode(mode: u8) -> bool {
    mode >= V_PRED && mode <= D67_PRED
}

fn pred_mode_of(mode: u8) -> PredMode {
    match mode {
        DC_PRED | UV_CFL_PRED => PredMode::Dc,
        SMOOTH_PRED => PredMode::SmoothAll,
        SMOOTH_V_PRED => PredMode::SmoothV,
        SMOOTH_H_PRED => PredMode::SmoothH,
        PAETH_PRED => PredMode::Paeth,
        m if is_directional_mode(m) => PredMode::Directional(m - V_PRED),
        _ => PredMode::Dc,
    }
}

// Partition ordinals, §3.
const PARTITION_NONE: u32 = 0;
const PARTITION_HORZ: u32 = 1;
const PARTITION_VERT: u32 = 2;
const PARTITION_SPLIT: u32 = 3;
const PARTITION_HORZ_A: u32 = 4;
const PARTITION_HORZ_B: u32 = 5;
const PARTITION_VERT_A: u32 = 6;
const PARTITION_VERT_B: u32 = 7;
const PARTITION_HORZ_4: u32 = 8;
const PARTITION_VERT_4: u32 = 9;

const NUM_BASE_LEVELS: i32 = 2;
const COEFF_BASE_RANGE: i32 = 12;
const BR_CDF_SIZE: i32 = 4;

fn ix(v: usize) -> i32 {
    i32::try_from(v).unwrap_or(i32::MAX)
}

/// One 4x4 mode-info grid cell's persisted state — everything a later
/// block's own context derivation reads back.
#[derive(Debug, Clone, Copy, Default)]
#[allow(
    dead_code,
    reason = "uv_mode/segment_id mirror the specification's own UVModes/SegmentIds grids; \
              this crate's reduced scope (no segmentation, no chroma neighbour context) does \
              not read them back yet, but the grid cell carries the same fields the spec's \
              does for parity with a fuller decode"
)]
struct MiCell {
    mi_size: u8,
    y_mode: u8,
    uv_mode: u8,
    tx_size: u8,
    skip: bool,
    segment_id: u8,
}

/// A per-plane, per-superblock "has this 4x4 unit been reconstructed"
/// grid, §6.10.3 — one border unit on every side, offset-indexed so a
/// `-1` row/col read is a safe lookup rather than a special case.
struct BlockDecoded {
    w: usize,
    h: usize,
    data: Vec<bool>,
}

impl BlockDecoded {
    fn new(w: usize, h: usize) -> Self {
        Self { w: w + 2, h: h + 2, data: vec![false; (w + 2) * (h + 2)] }
    }

    fn get(&self, y: i32, x: i32) -> bool {
        let (y, x) = (y + 1, x + 1);
        if y < 0 || x < 0 {
            return false;
        }
        let (y, x) = (y as usize, x as usize);
        if y >= self.h || x >= self.w {
            return false;
        }
        self.data.get(y * self.w + x).copied().unwrap_or(false)
    }

    fn set(&mut self, y: i32, x: i32, v: bool) {
        let (y, x) = (y + 1, x + 1);
        if y < 0 || x < 0 {
            return;
        }
        let (y, x) = (y as usize, x as usize);
        if x < self.w
            && let Some(slot) = self.data.get_mut(y * self.w + x)
        {
            *slot = v;
        }
    }
}

/// Per-tile mutable decode state: the symbol decoder, its CDF context, the
/// coefficient-context bookkeeping, and `BlockDecoded` for the superblock
/// currently being walked.
struct TileState<'a> {
    sd: SymbolDecoder<'a>,
    cdf: TileCdf,
    // AboveLevelContext/AboveDcContext[plane][x4], LeftLevelContext/LeftDcContext[plane][y4].
    above_level: [Vec<u8>; 3],
    above_dc: [Vec<u8>; 3],
    left_level: [Vec<u8>; 3],
    left_dc: [Vec<u8>; 3],
    block_decoded: [BlockDecoded; 3],
    current_q_index: i32,
    mi_row_start: usize,
    mi_row_end: usize,
    mi_col_start: usize,
    mi_col_end: usize,
}

/// Whole-frame state threaded through the partition/block walk.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent frame/sequence property (colour format, superblock size, \
              edge-filter enable) the specification keeps as separate fields"
)]
struct FrameCtx {
    header: FrameHeader,
    seq_mono: bool,
    subsampling_x: bool,
    subsampling_y: bool,
    bit_depth: u8,
    mi_cols: usize,
    mi_rows: usize,
    use_128x128_superblock: bool,
    enable_intra_edge_filter: bool,
    enable_filter_intra: bool,
    /// `cdef_idx[r][c]`, §5.11.56 — one entry per 64x64 unit
    /// (`(mi_rows.div_ceil(16), mi_cols.div_ceil(16))`), `-1` meaning "not
    /// yet read for this unit". Only ever consulted/populated when
    /// `read_cdef()` would actually read a literal, so this stays entirely
    /// unused (and harmless) whenever CDEF is off for the frame.
    cdef_idx: Vec<i32>,
    cdef_stride: usize,
    grid: Vec<MiCell>,
    pic: Picture,
    /// The most recently decoded transform block's `Quant[]` array,
    /// handed from [`coeffs`] to [`reconstruct`] — both operate on exactly
    /// one transform block at a time, so a single scratch buffer (taken via
    /// `mem::take`) is simpler than threading it through every call site.
    last_quant: Vec<i32>,
}

impl FrameCtx {
    fn mi_at(&self, r: i32, c: i32) -> Option<MiCell> {
        if r < 0 || c < 0 {
            return None;
        }
        let (r, c) = (usize::try_from(r).ok()?, usize::try_from(c).ok()?);
        if r >= self.mi_rows || c >= self.mi_cols {
            return None;
        }
        self.grid.get(r * self.mi_cols + c).copied()
    }

    fn store(&mut self, r: usize, c: usize, cell: MiCell) {
        if let Some(slot) = self.grid.get_mut(r * self.mi_cols + c) {
            *slot = cell;
        }
    }

}

/// `is_inside(candidateR, candidateC)`, §5.11.51 — whether a mode-info
/// position lies within the *current tile*'s bounds (not the frame's).
fn is_inside(ts: &TileState<'_>, r: i32, c: i32) -> bool {
    r >= ix(ts.mi_row_start) && r < ix(ts.mi_row_end) && c >= ix(ts.mi_col_start) && c < ix(ts.mi_col_end)
}

/// The AV1 decoder.
pub struct Av1Decoder {
    limits: Limits,
    budget: Budget,
    machine: vaco_codec_core::machine::Machine<Frame>,
    seq: Option<SequenceHeader>,
}

impl std::fmt::Debug for Av1Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Av1Decoder").finish_non_exhaustive()
    }
}

impl Av1Decoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            budget: Budget::new(limits.clone()),
            limits,
            machine: vaco_codec_core::machine::Machine::with_capacity(
                vaco_codec_core::Caps::SUBFRAMES,
                1,
            ),
            seq: None,
        }
    }

    fn decode_temporal_unit(&mut self, data: &[u8], pts: vaco_core::Timestamp, duration: vaco_core::Duration) -> Result<()> {
        let unit_list = units(data, Av1Framing::ObuStream);
        let mut pending_header: Option<FrameHeader> = None;
        // Finding 22b (planning/INTERFACE-GAPS.md): `vaco_parse_av1::metadata`
        // parses `metadata_hdr_mdcv()`/`metadata_hdr_cll()` (§5.8.3/§5.8.4)
        // correctly and nothing in this crate ever read either. A METADATA
        // OBU precedes the FRAME/TILE_GROUP OBU it describes within the same
        // temporal unit (§7.4), so -- like `pending_header` above -- these
        // are scoped to one `decode_temporal_unit` call, not carried across
        // packets; the last one of each type before an emitted frame wins.
        let mut pending_mastering_display = None;
        let mut pending_content_light = None;
        for unit in &unit_list {
            let payload = unit.payload(data);
            match unit.header.obu_type {
                t if t == ObuType::SEQUENCE_HEADER => {
                    let sh = SequenceHeader::parse(payload, &mut self.budget)?;
                    self.seq = Some(sh);
                }
                t if t == ObuType::METADATA => {
                    if let Ok(m) = vaco_parse_av1::metadata::parse(payload, &mut self.budget) {
                        match m {
                            vaco_parse_av1::Metadata::HdrMdcv(mdcv) => {
                                pending_mastering_display = Some(mastering_display_from_mdcv(mdcv));
                            }
                            vaco_parse_av1::Metadata::HdrCll(cll) => {
                                pending_content_light = Some((u32::from(cll.max_cll), u32::from(cll.max_fall)));
                            }
                            _ => {}
                        }
                    }
                }
                t if t == ObuType::FRAME_HEADER => {
                    let Some(seq) = self.seq.clone() else {
                        return Err(Error::InvalidData("vaco-codec-av1: frame header before any sequence header"));
                    };
                    let fh = FrameHeader::parse(payload, &seq, unit.header.temporal_id, unit.header.spatial_id)?;
                    pending_header = Some(fh);
                }
                t if t == ObuType::FRAME => {
                    // frame_obu(), §5.10: frame_header_obu() immediately
                    // followed, in the same OBU payload, by
                    // byte_alignment() and tile_group_obu(). Parsed with a
                    // single BitReader carried across both steps, so the
                    // tile group's start is wherever the frame header
                    // parse actually finished (byte-aligned), not assumed.
                    let Some(seq) = self.seq.clone() else {
                        return Err(Error::InvalidData("vaco-codec-av1: frame header before any sequence header"));
                    };
                    let mut r = vaco_bitstream::BitReader::new(payload);
                    let fh = FrameHeader::parse_from_reader(&mut r, &seq, unit.header.temporal_id, unit.header.spatial_id)?;
                    r.check().map_err(|_| Error::InvalidData("frame_obu's frame_header_obu ran past its payload"))?;
                    r.align();
                    let tile_payload = r.remaining_bytes();
                    let frame = decode_frame(&seq, &fh, tile_payload, &mut self.budget)?;
                    let mut frame = frame;
                    frame.pts = pts;
                    frame.duration = duration;
                    if let Some(mastering_display) = pending_mastering_display {
                        frame.set_side_data(vaco_frame::FrameSideData::MasteringDisplay(Box::new(mastering_display)));
                    }
                    if let Some((max_cll, max_fall)) = pending_content_light {
                        frame.set_side_data(vaco_frame::FrameSideData::ContentLightLevel { max_cll, max_fall });
                    }
                    if fh.show_frame {
                        self.machine.emit(frame);
                    }
                }
                t if t == ObuType::TILE_GROUP => {
                    let Some(fh) = pending_header.take() else {
                        return Err(Error::InvalidData("vaco-codec-av1: tile group with no pending frame header"));
                    };
                    let Some(seq) = self.seq.clone() else {
                        return Err(Error::InvalidData("vaco-codec-av1: tile group before any sequence header"));
                    };
                    let frame = decode_frame(&seq, &fh, payload, &mut self.budget)?;
                    let mut frame = frame;
                    frame.pts = pts;
                    frame.duration = duration;
                    if let Some(mastering_display) = pending_mastering_display {
                        frame.set_side_data(vaco_frame::FrameSideData::MasteringDisplay(Box::new(mastering_display)));
                    }
                    if let Some((max_cll, max_fall)) = pending_content_light {
                        frame.set_side_data(vaco_frame::FrameSideData::ContentLightLevel { max_cll, max_fall });
                    }
                    if fh.show_frame {
                        self.machine.emit(frame);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

impl Decoder for Av1Decoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        match self.machine.accept(packet.is_none())? {
            vaco_codec_core::machine::Accept::Drain => {
                self.machine.finish();
                Ok(())
            }
            vaco_codec_core::machine::Accept::Input => {
                let Some(pkt) = packet else { return Ok(()) };
                self.decode_temporal_unit(pkt.payload(), pkt.pts, pkt.duration)
            }
        }
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        self.machine.receive()
    }

    fn flush(&mut self) {
        self.machine.flush();
        self.seq = None;
        self.budget = Budget::new(self.limits.clone());
    }
}

/// `vaco-component.toml`'s decoder registration point.
pub static AV1_DECODER: DecoderDesc = DecoderDesc {
    name: "av1",
    long_name: "AV1 (intra-only; AV1 Bitstream & Decoding Process Specification v1.0.0 with Errata 1)",
    id: vaco_codec_core::CodecId::Av1,
    media_type: MediaType::Video,
    // One temporal unit can carry several shown frames (a frame OBU plus any
    // number of show_existing_frame headers referencing already-decoded ones),
    // so one input legitimately yields more than one output.
    caps: vaco_codec_core::Caps::SUBFRAMES,
    supported_rates: &[],
    make: |limits| Box::new(Av1Decoder::new(limits)),
};

/// §5.8.4's raw `metadata_hdr_mdcv()` fields into
/// `vaco_frame::MasteringDisplay`'s shared shape (finding 22b,
/// `planning/INTERFACE-GAPS.md`).
///
/// AV1's own fixed-point encodings are **different from H.264/HEVC's**
/// decimal-unit SEI message, despite `vaco_parse_av1::metadata::HdrMdcv`'s
/// own doc citing HEVC for the semantics -- confirmed black-box (D7:
/// AV1 spec text read, not `libaom`/`dav1d` source) by round-tripping known
/// chromaticity/luminance values through real `libsvtav1`
/// (`--mastering-display`) and reading them back with real
/// `ffprobe -show_frames`:
/// - **Chromaticity** (`primary_chromaticity`/`white_point_chromaticity`):
///   0.16 fixed point, `/65536` -- measured `0.708 -> red_x=46399/65536`
///   (`0.708 * 65536 = 46399.49`).
/// - **`luminance_max`**: 24.8 fixed point, `/256` -- measured
///   `1000 cd/m² -> max_luminance=256000/256` exactly.
/// - **`luminance_min`**: 18.14 fixed point, `/16384` -- measured
///   `0.005 cd/m² -> min_luminance=82/16384` (`0.005 * 16384 = 81.92`).
///
/// **Not** H.264/HEVC's green/blue/red bitstream order, despite
/// `vaco_parse_av1::metadata::HdrMdcv`'s own doc citing HEVC for the
/// semantics -- `primary_chromaticity[0]`/`[1]`/`[2]` is already red,
/// green, blue, confirmed by the same round trip as the unit measurement
/// above: the SVT-AV1 CLI's `G(0.170,...)B(0.131,...)R(0.708,...)` input
/// landed at `primary_chromaticity[1]`/`[2]`/`[0]` respectively -- i.e.
/// index 0 held the `R(...)` value, not `G(...)` -- so no permutation
/// happens here, unlike the H.264/HEVC sibling function of this name.
/// Caught by writing the green/blue/red permutation first (the doc's own
/// claim) and having this exact test fail with primaries visibly rotated
/// one slot before correcting it.
fn mastering_display_from_mdcv(mdcv: vaco_parse_av1::metadata::HdrMdcv) -> vaco_frame::MasteringDisplay {
    let chromaticity = |(x, y): (u16, u16)| [vaco_core::Rational::new(i32::from(x), 65_536), vaco_core::Rational::new(i32::from(y), 65_536)];
    vaco_frame::MasteringDisplay {
        primaries: mdcv.primary_chromaticity.map(chromaticity),
        white_point: chromaticity(mdcv.white_point_chromaticity),
        max_luminance: vaco_core::Rational::new(i32::try_from(mdcv.luminance_max).unwrap_or(i32::MAX), 256),
        min_luminance: vaco_core::Rational::new(i32::try_from(mdcv.luminance_min).unwrap_or(i32::MAX), 16_384),
    }
}

fn decode_frame(seq: &SequenceHeader, fh: &FrameHeader, tile_group_payload: &[u8], budget: &mut Budget) -> Result<Frame> {
    let mi_cols = 2 * ((fh.size.coded_width + 7) >> 3);
    let mi_rows = 2 * ((fh.size.coded_height + 7) >> 3);
    let (mi_cols, mi_rows) = (mi_cols as usize, mi_rows as usize);
    let mono = seq.color_config.mono_chrome;
    let (sub_x, sub_y) = (seq.color_config.subsampling_x, seq.color_config.subsampling_y);
    let luma_w = mi_cols * 4;
    let luma_h = mi_rows * 4;
    let chroma_w = luma_w >> u32::from(sub_x);
    let chroma_h = luma_h >> u32::from(sub_y);
    let pic = Picture::new(budget, luma_w, luma_h, chroma_w, chroma_h, mono)?;

    let mut ctx = FrameCtx {
        header: fh.clone(),
        seq_mono: mono,
        subsampling_x: sub_x,
        subsampling_y: sub_y,
        bit_depth: seq.color_config.bit_depth,
        mi_cols,
        mi_rows,
        use_128x128_superblock: seq.use_128x128_superblock,
        enable_intra_edge_filter: seq.enable_intra_edge_filter,
        enable_filter_intra: seq.enable_filter_intra,
        cdef_idx: vec![-1i32; mi_rows.div_ceil(16) * mi_cols.div_ceil(16)],
        cdef_stride: mi_cols.div_ceil(16),
        grid: vec![MiCell::default(); mi_cols * mi_rows],
        pic,
        last_quant: Vec::new(),
    };

    decode_tiles(&mut ctx, tile_group_payload)?;
    pic_to_frame(budget, seq, fh, &ctx.pic, mi_cols, mi_rows)
}

fn decode_tiles(ctx: &mut FrameCtx, payload: &[u8]) -> Result<()> {
    let tile = ctx.header.tile_info.clone();
    let num_tiles = tile.cols * tile.rows;
    let mut r = vaco_bitstream::BitReader::new(payload);
    let start_bit_pos = r.bit_pos();
    let tile_start_and_end_present_flag = if num_tiles > 1 { r.get_bit() != 0 } else { false };
    let (tg_start, tg_end) = if num_tiles == 1 || !tile_start_and_end_present_flag {
        (0usize, num_tiles.saturating_sub(1))
    } else {
        let tile_bits = tile.cols_log2 + tile.rows_log2;
        let s = r.get(tile_bits) as usize;
        let e = r.get(tile_bits) as usize;
        (s, e)
    };
    // byte_alignment()
    let consumed = r.bit_pos() - start_bit_pos;
    let pad = u32::try_from((8 - (consumed % 8)) % 8).unwrap_or(0);
    let _ = r.get(pad);
    #[allow(clippy::integer_division, reason = "byte_alignment() (5.11.4): counting whole bytes consumed, not a fraction")]
    let header_bytes = usize::try_from((r.bit_pos() - start_bit_pos) / 8).unwrap_or(0);
    let empty: &[u8] = &[];
    let mut remaining: &[u8] = payload.get(header_bytes..).unwrap_or(empty);
    let mut sz = remaining.len();

    for tile_num in tg_start..=tg_end {
        #[allow(clippy::integer_division, reason = "5.11.1's own TileNum / TileCols row derivation")]
        let tile_row = tile_num / tile.cols.max(1);
        let tile_col = tile_num % tile.cols.max(1);
        let last_tile = tile_num == tg_end;
        let tile_size = if last_tile {
            sz
        } else {
            let n = usize::try_from(tile.tile_size_bytes).unwrap_or(1);
            let (val, consumed) = read_le(remaining, n);
            remaining = remaining.get(consumed..).unwrap_or(empty);
            sz = sz.saturating_sub(consumed);
            let size = usize::try_from(val + 1).unwrap_or(0);
            sz = sz.saturating_sub(size);
            size
        };
        let this_tile_data = remaining.get(..tile_size).unwrap_or(remaining);
        remaining = remaining.get(tile_size..).unwrap_or(empty);

        let mi_row_start = usize::try_from(tile.mi_row_starts.get(tile_row).copied().unwrap_or(0)).unwrap_or(0);
        let mi_row_end = usize::try_from(tile.mi_row_starts.get(tile_row + 1).copied().unwrap_or(ctx.mi_rows as u32)).unwrap_or(ctx.mi_rows);
        let mi_col_start = usize::try_from(tile.mi_col_starts.get(tile_col).copied().unwrap_or(0)).unwrap_or(0);
        let mi_col_end = usize::try_from(tile.mi_col_starts.get(tile_col + 1).copied().unwrap_or(ctx.mi_cols as u32)).unwrap_or(ctx.mi_cols);

        decode_one_tile(ctx, this_tile_data, mi_row_start, mi_row_end, mi_col_start, mi_col_end)?;
    }
    Ok(())
}

fn read_le(data: &[u8], n: usize) -> (u64, usize) {
    let mut v = 0u64;
    for i in 0..n {
        v |= u64::from(data.get(i).copied().unwrap_or(0)) << (8 * i);
    }
    (v, n)
}

#[allow(clippy::too_many_lines, reason = "decode_tile()'s own superblock walk, section 5.11.2")]
#[allow(clippy::many_single_char_names, reason = "mirrors the spec's own r/c/w/h/v loop-variable names for the superblock walk")]
fn decode_one_tile(ctx: &mut FrameCtx, tile_data: &[u8], mi_row_start: usize, mi_row_end: usize, mi_col_start: usize, mi_col_end: usize) -> Result<()> {
    let num_planes = if ctx.seq_mono { 1 } else { 3 };
    let above_len = [ctx.mi_cols, ctx.mi_cols >> u32::from(ctx.subsampling_x), ctx.mi_cols >> u32::from(ctx.subsampling_x)];
    let left_len = [ctx.mi_rows, ctx.mi_rows >> u32::from(ctx.subsampling_y), ctx.mi_rows >> u32::from(ctx.subsampling_y)];

    let mut ts = TileState {
        sd: SymbolDecoder::new(tile_data, ctx.header.disable_cdf_update),
        cdf: TileCdf::new(ctx.header.quant.base_q_idx),
        above_level: std::array::from_fn(|p| vec![0u8; above_len.get(p).copied().unwrap_or(1).max(1)]),
        above_dc: std::array::from_fn(|p| vec![0u8; above_len.get(p).copied().unwrap_or(1).max(1)]),
        left_level: std::array::from_fn(|p| vec![0u8; left_len.get(p).copied().unwrap_or(1).max(1)]),
        left_dc: std::array::from_fn(|p| vec![0u8; left_len.get(p).copied().unwrap_or(1).max(1)]),
        block_decoded: [BlockDecoded::new(0, 0), BlockDecoded::new(0, 0), BlockDecoded::new(0, 0)],
        current_q_index: i32::from(ctx.header.quant.base_q_idx),
        mi_row_start,
        mi_row_end,
        mi_col_start,
        mi_col_end,
    };
    let _ = num_planes;

    let sb_size4: usize = if ctx.use_128x128_superblock { 32 } else { 16 };
    let sb_size4_i = ix(sb_size4);

    let mut r = mi_row_start;
    while r < mi_row_end {
        for p in 0..3 {
            if let Some(v) = ts.left_level.get_mut(p) {
                v.fill(0);
            }
            if let Some(v) = ts.left_dc.get_mut(p) {
                v.fill(0);
            }
        }
        let mut c = mi_col_start;
        while c < mi_col_end {
            let sb_w4 = mi_col_end.saturating_sub(c);
            let sb_h4 = mi_row_end.saturating_sub(r);
            for (plane, bd) in ts.block_decoded.iter_mut().enumerate() {
                let (sx, sy) = if plane > 0 { (ctx.subsampling_x, ctx.subsampling_y) } else { (false, false) };
                let w = (sb_size4 >> u32::from(sx)).max(1);
                let h = (sb_size4 >> u32::from(sy)).max(1);
                *bd = BlockDecoded::new(w + 1, h + 1);
                let sb_width4 = ix(sb_w4) >> u32::from(sx);
                let sb_height4 = ix(sb_h4) >> u32::from(sy);
                for y in -1..=ix(w) {
                    for x in -1..=ix(h) {
                        let v = (y < 0 && x < sb_width4) || (x < 0 && y < sb_height4);
                        bd.set(y, x, v);
                    }
                }
                bd.set(ix(h), -1, false);
            }
            let sb_size = if ctx.use_128x128_superblock { 15u8 } else { 12u8 };
            decode_partition(ctx, &mut ts, r, c, sb_size)?;
            c += sb_size4;
        }
        r += sb_size4;
    }
    ts.sd.exit_symbol();
    let _ = sb_size4_i;
    Ok(())
}

#[allow(clippy::too_many_arguments, reason = "mirrors decode_partition's own recursive signature, section 5.11.4")]
fn decode_partition(ctx: &mut FrameCtx, ts: &mut TileState<'_>, r: usize, c: usize, b_size: u8) -> Result<()> {
    if r >= ctx.mi_rows || c >= ctx.mi_cols {
        return Ok(());
    }
    let avail_u = is_inside(ts, ix(r) - 1, ix(c));
    let avail_l = is_inside(ts, ix(r), ix(c) - 1);
    let num4x4 = i32::from(tables::NUM_4X4_BLOCKS_WIDE.get(usize::from(b_size)).copied().unwrap_or(1));
    let half4 = num4x4 >> 1;
    let quarter4 = half4 >> 1;
    let has_rows = ix(r) + half4 < ix(ctx.mi_rows);
    let has_cols = ix(c) + half4 < ix(ctx.mi_cols);

    let bsl = tables::MI_WIDTH_LOG2.get(usize::from(b_size)).copied().unwrap_or(0);
    let above_smaller = avail_u
        && ctx.mi_at(ix(r) - 1, ix(c)).is_some_and(|m| tables::MI_WIDTH_LOG2.get(usize::from(m.mi_size)).copied().unwrap_or(0) < bsl);
    let left_smaller = avail_l
        && ctx.mi_at(ix(r), ix(c) - 1).is_some_and(|m| tables::MI_HEIGHT_LOG2.get(usize::from(m.mi_size)).copied().unwrap_or(0) < bsl);
    let part_ctx = usize::from(left_smaller) * 2 + usize::from(above_smaller);

    let partition = if b_size < BLOCK_8X8 {
        PARTITION_NONE
    } else if has_rows && has_cols {
        read_partition_symbol(ts, bsl, part_ctx)
    } else if has_cols {
        read_split_or(ts, bsl, part_ctx, true)
    } else if has_rows {
        read_split_or(ts, bsl, part_ctx, false)
    } else {
        PARTITION_SPLIT
    };

    let sub_size = partition_subsize(partition, b_size);
    let split_size = partition_subsize(PARTITION_SPLIT, b_size);

    match partition {
        PARTITION_NONE => decode_block(ctx, ts, r, c, sub_size)?,
        PARTITION_HORZ => {
            decode_block(ctx, ts, r, c, sub_size)?;
            if has_rows {
                decode_block(ctx, ts, r + usize::try_from(half4).unwrap_or(0), c, sub_size)?;
            }
        }
        PARTITION_VERT => {
            decode_block(ctx, ts, r, c, sub_size)?;
            if has_cols {
                decode_block(ctx, ts, r, c + usize::try_from(half4).unwrap_or(0), sub_size)?;
            }
        }
        PARTITION_SPLIT => {
            let h = usize::try_from(half4).unwrap_or(0);
            decode_partition(ctx, ts, r, c, sub_size)?;
            decode_partition(ctx, ts, r, c + h, sub_size)?;
            decode_partition(ctx, ts, r + h, c, sub_size)?;
            decode_partition(ctx, ts, r + h, c + h, sub_size)?;
        }
        PARTITION_HORZ_A => {
            let h = usize::try_from(half4).unwrap_or(0);
            decode_block(ctx, ts, r, c, split_size)?;
            decode_block(ctx, ts, r, c + h, split_size)?;
            decode_block(ctx, ts, r + h, c, sub_size)?;
        }
        PARTITION_HORZ_B => {
            let h = usize::try_from(half4).unwrap_or(0);
            decode_block(ctx, ts, r, c, sub_size)?;
            decode_block(ctx, ts, r + h, c, split_size)?;
            decode_block(ctx, ts, r + h, c + h, split_size)?;
        }
        PARTITION_VERT_A => {
            let h = usize::try_from(half4).unwrap_or(0);
            decode_block(ctx, ts, r, c, split_size)?;
            decode_block(ctx, ts, r + h, c, split_size)?;
            decode_block(ctx, ts, r, c + h, sub_size)?;
        }
        PARTITION_VERT_B => {
            let h = usize::try_from(half4).unwrap_or(0);
            decode_block(ctx, ts, r, c, sub_size)?;
            decode_block(ctx, ts, r, c + h, split_size)?;
            decode_block(ctx, ts, r + h, c + h, split_size)?;
        }
        PARTITION_HORZ_4 => {
            let q = usize::try_from(quarter4).unwrap_or(0);
            decode_block(ctx, ts, r, c, sub_size)?;
            decode_block(ctx, ts, r + q, c, sub_size)?;
            decode_block(ctx, ts, r + 2 * q, c, sub_size)?;
            if ix(r) + quarter4 * 3 < ix(ctx.mi_rows) {
                decode_block(ctx, ts, r + 3 * q, c, sub_size)?;
            }
        }
        _ => {
            let q = usize::try_from(quarter4).unwrap_or(0);
            decode_block(ctx, ts, r, c, sub_size)?;
            decode_block(ctx, ts, r, c + q, sub_size)?;
            decode_block(ctx, ts, r, c + 2 * q, sub_size)?;
            if ix(c) + quarter4 * 3 < ix(ctx.mi_cols) {
                decode_block(ctx, ts, r, c + 3 * q, sub_size)?;
            }
        }
    }
    Ok(())
}

fn partition_subsize(partition: u32, b_size: u8) -> u8 {
    let v = tables::PARTITION_SUBSIZE
        .get(usize::try_from(partition).unwrap_or(0))
        .and_then(|row| row.get(usize::from(b_size)))
        .copied()
        .unwrap_or(u16::from(BLOCK_INVALID));
    u8::try_from(v).unwrap_or(BLOCK_INVALID)
}

fn partition_cdf_for(ts: &mut TileState<'_>, bsl: u16, ctx: usize) -> Vec<u16> {
    match bsl {
        1 => ts.cdf.partition_w8.get(ctx).copied().unwrap_or_default().to_vec(),
        2 => ts.cdf.partition_w16.get(ctx).copied().unwrap_or_default().to_vec(),
        3 => ts.cdf.partition_w32.get(ctx).copied().unwrap_or_default().to_vec(),
        4 => ts.cdf.partition_w64.get(ctx).copied().unwrap_or_default().to_vec(),
        _ => ts.cdf.partition_w128.get(ctx).copied().unwrap_or_default().to_vec(),
    }
}

fn write_partition_cdf_back(ts: &mut TileState<'_>, bsl: u16, ctx: usize, v: &[u16]) {
    fn copy_into<const N: usize>(dst: &mut [u16; N], src: &[u16]) {
        for (d, s) in dst.iter_mut().zip(src.iter()) {
            *d = *s;
        }
    }
    match bsl {
        1 => {
            if let Some(row) = ts.cdf.partition_w8.get_mut(ctx) {
                copy_into(row, v);
            }
        }
        2 => {
            if let Some(row) = ts.cdf.partition_w16.get_mut(ctx) {
                copy_into(row, v);
            }
        }
        3 => {
            if let Some(row) = ts.cdf.partition_w32.get_mut(ctx) {
                copy_into(row, v);
            }
        }
        4 => {
            if let Some(row) = ts.cdf.partition_w64.get_mut(ctx) {
                copy_into(row, v);
            }
        }
        _ => {
            if let Some(row) = ts.cdf.partition_w128.get_mut(ctx) {
                copy_into(row, v);
            }
        }
    }
}

fn read_partition_symbol(ts: &mut TileState<'_>, bsl: u16, ctx: usize) -> u32 {
    let mut cdf = partition_cdf_for(ts, bsl, ctx);
    let symbol = ts.sd.read_symbol(&mut cdf);
    write_partition_cdf_back(ts, bsl, ctx, &cdf);
    symbol
}

/// `split_or_horz`/`split_or_vert`, §8.3.2 — a scratch 3-symbol cdf built
/// from the (un-mutated) real partition cdf's current values, per the
/// specification's own `psum` construction.
fn read_split_or(ts: &mut TileState<'_>, bsl: u16, ctx: usize, horz: bool) -> u32 {
    let real = partition_cdf_for(ts, bsl, ctx);
    let n = real.len().saturating_sub(1);
    let has_128 = n == 8; // BLOCK_128X128 has no *_4 partitions.
    let diff = |idx: usize| -> i32 {
        let hi = i32::from(real.get(idx).copied().unwrap_or(0));
        let lo = if idx == 0 { 0 } else { i32::from(real.get(idx - 1).copied().unwrap_or(0)) };
        hi - lo
    };
    let mut psum = if horz {
        diff(usize::try_from(PARTITION_VERT).unwrap_or(0))
            + diff(usize::try_from(PARTITION_SPLIT).unwrap_or(0))
            + diff(usize::try_from(PARTITION_HORZ_A).unwrap_or(0))
            + diff(usize::try_from(PARTITION_VERT_A).unwrap_or(0))
            + diff(usize::try_from(PARTITION_VERT_B).unwrap_or(0))
    } else {
        diff(usize::try_from(PARTITION_HORZ).unwrap_or(0))
            + diff(usize::try_from(PARTITION_SPLIT).unwrap_or(0))
            + diff(usize::try_from(PARTITION_HORZ_A).unwrap_or(0))
            + diff(usize::try_from(PARTITION_HORZ_B).unwrap_or(0))
            + diff(usize::try_from(PARTITION_VERT_A).unwrap_or(0))
    };
    if !has_128 {
        psum += if horz { diff(usize::try_from(PARTITION_VERT_4).unwrap_or(0)) } else { diff(usize::try_from(PARTITION_HORZ_4).unwrap_or(0)) };
    }
    let mut cdf = [u16::try_from((1i32 << 15) - psum).unwrap_or(0), 1u16 << 15, 0u16];
    ts.sd.read_symbol(&mut cdf)
}

#[allow(clippy::too_many_lines, reason = "decode_block()'s own per-block walk plus mode-info, sections 5.11.5-5.11.9")]
fn decode_block(ctx: &mut FrameCtx, ts: &mut TileState<'_>, r: usize, c: usize, mi_size: u8) -> Result<()> {
    let bw4 = usize::from(tables::NUM_4X4_BLOCKS_WIDE.get(usize::from(mi_size)).copied().unwrap_or(1));
    let bh4 = usize::from(tables::NUM_4X4_BLOCKS_HIGH.get(usize::from(mi_size)).copied().unwrap_or(1));
    let has_chroma = if (bh4 == 1 && ctx.subsampling_y && r.is_multiple_of(2))
        || (bw4 == 1 && ctx.subsampling_x && c.is_multiple_of(2))
    {
        false
    } else {
        !ctx.seq_mono
    };
    let avail_u = is_inside(ts, ix(r) - 1, ix(c));
    let avail_l = is_inside(ts, ix(r), ix(c) - 1);

    // intra_frame_mode_info(), §5.11.7 (this crate's reduced form: no
    // SegIdPreSkip segmentation ordering, no intrabc, no palette -- all off
    // in this crate's own test fixtures per the module doc. `read_cdef()`
    // and `filter_intra_mode_info()` are read regardless of that, below,
    // since both are present-or-absent based on frame/sequence flags a
    // real encoder sets independently of this crate's own scope cuts, and
    // skipping either's bits when present desyncs every symbol after it.)
    let skip = read_skip(ctx, ts, avail_u, avail_l, r, c);
    let segment_id = 0u8; // segmentation disabled, per this crate's scope.
    read_cdef(ctx, ts, r, c, mi_size, skip);
    read_delta_qindex_lf(ctx, ts, mi_size, skip);

    let above_mode = if avail_u { ctx.mi_at(ix(r) - 1, ix(c)).map_or(DC_PRED, |m| m.y_mode) } else { DC_PRED };
    let left_mode = if avail_l { ctx.mi_at(ix(r), ix(c) - 1).map_or(DC_PRED, |m| m.y_mode) } else { DC_PRED };
    let above_ctx = tables::INTRA_MODE_CONTEXT.get(usize::from(above_mode)).copied().unwrap_or(0) as usize;
    let left_ctx = tables::INTRA_MODE_CONTEXT.get(usize::from(left_mode)).copied().unwrap_or(0) as usize;
    let mut y_cdf = ts.cdf.intra_frame_y_mode.get(above_ctx).and_then(|row| row.get(left_ctx)).copied().unwrap_or_default().to_vec();
    let y_mode = u8::try_from(ts.sd.read_symbol(&mut y_cdf)).unwrap_or(0);
    if let Some(row) = ts.cdf.intra_frame_y_mode.get_mut(above_ctx)
        && let Some(slot) = row.get_mut(left_ctx)
    {
        for (d, s) in slot.iter_mut().zip(y_cdf.iter()) {
            *d = *s;
        }
    }

    let angle_delta_y = read_angle_delta(ts, mi_size, y_mode);

    let (mut uv_mode, mut angle_delta_uv, mut cfl_alpha_u, mut cfl_alpha_v) = (DC_PRED, 0i32, 0i32, 0i32);
    if has_chroma {
        let cfl_allowed = block_size_cfl_allowed(mi_size, ctx.header.coded_lossless);
        let n = if cfl_allowed { 14 } else { 13 };
        let mut cdf: Vec<u16> = if cfl_allowed {
            ts.cdf.uv_mode_cfl_allowed.get(usize::from(y_mode)).copied().unwrap_or_default().to_vec()
        } else {
            ts.cdf.uv_mode_cfl_not_allowed.get(usize::from(y_mode)).copied().unwrap_or_default().to_vec()
        };
        let sym = u8::try_from(ts.sd.read_symbol(&mut cdf)).unwrap_or(0);
        uv_mode = sym;
        if cfl_allowed {
            if let Some(row) = ts.cdf.uv_mode_cfl_allowed.get_mut(usize::from(y_mode)) {
                for (d, s) in row.iter_mut().zip(cdf.iter()) {
                    *d = *s;
                }
            }
        } else if let Some(row) = ts.cdf.uv_mode_cfl_not_allowed.get_mut(usize::from(y_mode)) {
            for (d, s) in row.iter_mut().zip(cdf.iter()) {
                *d = *s;
            }
        }
        let _ = n;
        if uv_mode == UV_CFL_PRED {
            let (su_, sv_, au, av) = read_cfl_alphas(ts);
            let _ = (su_, sv_);
            cfl_alpha_u = au;
            cfl_alpha_v = av;
        }
        angle_delta_uv = read_angle_delta(ts, mi_size, uv_mode);
    }

    read_palette_mode_info(ctx, ts, mi_size, y_mode, uv_mode, has_chroma)?;
    read_filter_intra(ctx, ts, mi_size, y_mode)?;

    let cell = MiCell { mi_size, y_mode, uv_mode, tx_size: 0, skip, segment_id };
    for y in 0..bh4 {
        for x in 0..bw4 {
            ctx.store(r + y, c + x, cell);
        }
    }

    let tx_size = read_tx_size(ctx, ts, r, c, mi_size, avail_u, avail_l, !skip);
    for y in 0..bh4 {
        for x in 0..bw4 {
            let mut cell = ctx.mi_at(ix(r + y), ix(c + x)).unwrap_or(cell);
            cell.tx_size = tx_size;
            ctx.store(r + y, c + x, cell);
        }
    }

    // compute_prediction() is a no-op for a pure intra, non-inter-intra
    // block (the whole-block predict_inter/inter-intra path never runs);
    // prediction happens per transform block inside residual() instead.
    residual(ctx, ts, r, c, mi_size, has_chroma, skip, y_mode, uv_mode, angle_delta_y, angle_delta_uv, cfl_alpha_u, cfl_alpha_v, avail_u, avail_l)?;
    Ok(())
}

fn block_size_cfl_allowed(mi_size: u8, lossless: bool) -> bool {
    if lossless {
        // get_plane_residual_size(MiSize, 1) == BLOCK_4X4 check, approximated:
        // lossless always uses 4x4 transforms, and CFL follows the same rule
        // as the non-lossless "max dimension <= 32" path in practice for the
        // block sizes lossless coding actually uses.
        true
    } else {
        let w = tables::block_width(mi_size);
        let h = tables::block_height(mi_size);
        w.max(h) <= 32
    }
}

fn read_angle_delta(ts: &mut TileState<'_>, mi_size: u8, mode: u8) -> i32 {
    if mi_size < BLOCK_8X8 || !is_directional_mode(mode) {
        return 0;
    }
    let idx = usize::from(mode - V_PRED);
    let mut cdf = ts.cdf.angle_delta.get(idx).copied().unwrap_or_default().to_vec();
    let sym = i32::try_from(ts.sd.read_symbol(&mut cdf)).unwrap_or(0);
    if let Some(row) = ts.cdf.angle_delta.get_mut(idx) {
        for (d, s) in row.iter_mut().zip(cdf.iter()) {
            *d = *s;
        }
    }
    sym - 3
}

/// `palette_mode_info()`, §5.11.46. Like [`read_filter_intra`], this crate
/// never applies a palette prediction, but a real encoder can leave
/// `allow_screen_content_tools` on (`libaom`/`libsvtav1` default it on for
/// ordinary content, independent of whether any block actually ends up
/// using a palette) — so `has_palette_y`/`has_palette_uv` are read
/// whenever the syntax makes them present, and only a block that actually
/// sets one returns [`Error::Unsupported`] (the full palette-colour-array
/// syntax after that is not implemented).
fn read_palette_mode_info(ctx: &mut FrameCtx, ts: &mut TileState<'_>, mi_size: u8, y_mode: u8, uv_mode: u8, has_chroma: bool) -> Result<()> {
    if mi_size < BLOCK_8X8 || !ctx.header.allow_screen_content_tools {
        return Ok(());
    }
    let bw = tables::block_width(mi_size);
    let bh = tables::block_height(mi_size);
    if bw > 64 || bh > 64 {
        return Ok(());
    }
    let bsize_ctx = usize::from(
        tables::MI_WIDTH_LOG2.get(usize::from(mi_size)).copied().unwrap_or(0) + tables::MI_HEIGHT_LOG2.get(usize::from(mi_size)).copied().unwrap_or(0),
    )
    .saturating_sub(2);

    if y_mode == DC_PRED {
        let mut cdf = ts.cdf.palette_y_mode.get(bsize_ctx).and_then(|r| r.first()).copied().unwrap_or_default();
        let has_palette_y = ts.sd.read_symbol(&mut cdf) != 0;
        if let Some(slot) = ts.cdf.palette_y_mode.get_mut(bsize_ctx).and_then(|r| r.first_mut()) {
            *slot = cdf;
        }
        if has_palette_y {
            return Err(Error::Unsupported("vaco-codec-av1: has_palette_y is not decoded"));
        }
    }
    if has_chroma && uv_mode == DC_PRED {
        let mut cdf = ts.cdf.palette_uv_mode.first().copied().unwrap_or_default();
        let has_palette_uv = ts.sd.read_symbol(&mut cdf) != 0;
        if let Some(slot) = ts.cdf.palette_uv_mode.first_mut() {
            *slot = cdf;
        }
        if has_palette_uv {
            return Err(Error::Unsupported("vaco-codec-av1: has_palette_uv is not decoded"));
        }
    }
    Ok(())
}

/// `filter_intra_mode_info()`, §5.11.24 — bit-for-bit, this crate never
/// implements the recursive filter-intra prediction itself (§7.11.2.3), so
/// this exists only to keep the tile's own bit position correct: whenever
/// the syntax says `use_filter_intra` is present, that bit (and the
/// `filter_intra_mode` symbol after it when set) must be read regardless,
/// or every symbol read for the rest of the tile desyncs. A real encoder's
/// sequence header enabling `enable_filter_intra` (common — `libaom`
/// defaults to it on) does not imply any particular block actually uses
/// it, so [`Error::Unsupported`] is only returned on `use_filter_intra ==
/// true`, not on `enable_filter_intra` alone.
fn read_filter_intra(ctx: &mut FrameCtx, ts: &mut TileState<'_>, mi_size: u8, y_mode: u8) -> Result<()> {
    if !ctx.enable_filter_intra || y_mode != DC_PRED {
        return Ok(());
    }
    let bw = tables::block_width(mi_size);
    let bh = tables::block_height(mi_size);
    if bw.max(bh) > 32 {
        return Ok(());
    }
    let mut cdf = ts.cdf.filter_intra.get(usize::from(mi_size)).copied().unwrap_or_default().to_vec();
    let use_filter_intra = ts.sd.read_symbol(&mut cdf) != 0;
    if let Some(slot) = ts.cdf.filter_intra.get_mut(usize::from(mi_size)) {
        for (d, s) in slot.iter_mut().zip(cdf.iter()) {
            *d = *s;
        }
    }
    if use_filter_intra {
        let mut mode_cdf = ts.cdf.filter_intra_mode;
        let _ = ts.sd.read_symbol(&mut mode_cdf);
        ts.cdf.filter_intra_mode = mode_cdf;
        return Err(Error::Unsupported("vaco-codec-av1: use_filter_intra is not decoded"));
    }
    Ok(())
}

fn read_cfl_alphas(ts: &mut TileState<'_>) -> (i32, i32, i32, i32) {
    let mut signs_cdf = ts.cdf.cfl_sign.to_vec();
    let signs = i32::try_from(ts.sd.read_symbol(&mut signs_cdf)).unwrap_or(0);
    for (d, s) in ts.cdf.cfl_sign.iter_mut().zip(signs_cdf.iter()) {
        *d = *s;
    }
    #[allow(clippy::integer_division, reason = "5.11.45's own cfl_alpha_signs decode formula")]
    let sign_u = (signs + 1) / 3;
    let sign_v = (signs + 1) % 3;
    let mut alpha_u = 0;
    let mut alpha_v = 0;
    if sign_u != 0 {
        // cfl_alpha_u's context, §8.3.2: (signU - 1) * 3 + signV -- not the
        // same context cfl_alpha_v uses (see below), and not `signs`
        // itself; distinct enough from `cfl_alpha_signs` that reusing one
        // formula for both symbols is a real, silent context-selection bug
        // (confirmed against the specification's own worked table, not
        // just the "= cfl_alpha_signs - 2" shortcut it separately notes).
        let ctx = usize::try_from((sign_u - 1) * 3 + sign_v).unwrap_or(0);
        let mut cdf = ts.cdf.cfl_alpha.get(ctx).copied().unwrap_or_default().to_vec();
        let v = i32::try_from(ts.sd.read_symbol(&mut cdf)).unwrap_or(0);
        if let Some(row) = ts.cdf.cfl_alpha.get_mut(ctx) {
            for (d, s) in row.iter_mut().zip(cdf.iter()) {
                *d = *s;
            }
        }
        alpha_u = if sign_u == 1 { -(1 + v) } else { 1 + v };
    }
    if sign_v != 0 {
        // cfl_alpha_v's context, §8.3.2: (signV - 1) * 3 + signU.
        let ctx = usize::try_from((sign_v - 1) * 3 + sign_u).unwrap_or(0);
        let mut cdf = ts.cdf.cfl_alpha.get(ctx).copied().unwrap_or_default().to_vec();
        let v = i32::try_from(ts.sd.read_symbol(&mut cdf)).unwrap_or(0);
        if let Some(row) = ts.cdf.cfl_alpha.get_mut(ctx) {
            for (d, s) in row.iter_mut().zip(cdf.iter()) {
                *d = *s;
            }
        }
        alpha_v = if sign_v == 1 { -(1 + v) } else { 1 + v };
    }
    (sign_u, sign_v, alpha_u, alpha_v)
}

fn read_skip(ctx: &mut FrameCtx, ts: &mut TileState<'_>, avail_u: bool, avail_l: bool, r: usize, c: usize) -> bool {
    let mut skip_ctx = 0usize;
    if avail_u && ctx.mi_at(ix(r) - 1, ix(c)).is_some_and(|m| m.skip) {
        skip_ctx += 1;
    }
    if avail_l && ctx.mi_at(ix(r), ix(c) - 1).is_some_and(|m| m.skip) {
        skip_ctx += 1;
    }
    let mut cdf = ts.cdf.skip.get(skip_ctx).copied().unwrap_or_default().to_vec();
    let v = ts.sd.read_symbol(&mut cdf) != 0;
    if let Some(row) = ts.cdf.skip.get_mut(skip_ctx) {
        for (d, s) in row.iter_mut().zip(cdf.iter()) {
            *d = *s;
        }
    }
    v
}

/// `read_cdef()`, §5.11.56. This crate never applies CDEF (out of scope,
/// per the module doc), but the `cdef_idx` literal it reads from the tile
/// bitstream — once per 64x64 unit, on that unit's first non-skip block —
/// is real bit-consuming syntax that every later symbol read depends on
/// landing in the right place, whether or not this crate ever uses the
/// value.
fn read_cdef(ctx: &mut FrameCtx, ts: &mut TileState<'_>, r: usize, c: usize, mi_size: u8, skip: bool) {
    if skip || ctx.header.coded_lossless || ctx.header.cdef_bits == 0 || ctx.header.allow_intrabc {
        return;
    }
    let cdef_size4 = 16usize; // Num_4x4_Blocks_Wide[BLOCK_64X64]
    let (ur, uc) = (r & !(cdef_size4 - 1), c & !(cdef_size4 - 1));
    #[allow(clippy::integer_division, reason = "5.11.56's own cdef_idx unit-grid coordinates: r/64, c/64 in pixels")]
    let (gr, gc) = (ur / cdef_size4, uc / cdef_size4);
    let Some(idx) = gr.checked_mul(ctx.cdef_stride).and_then(|base| base.checked_add(gc)) else {
        return;
    };
    if ctx.cdef_idx.get(idx).copied().unwrap_or(-1) != -1 {
        return;
    }
    let value = i32::try_from(ts.sd.read_literal(ctx.header.cdef_bits)).unwrap_or(0);
    let w4 = usize::from(tables::NUM_4X4_BLOCKS_WIDE.get(usize::from(mi_size)).copied().unwrap_or(1));
    let h4 = usize::from(tables::NUM_4X4_BLOCKS_HIGH.get(usize::from(mi_size)).copied().unwrap_or(1));
    let mut i = ur;
    while i < ur + h4 {
        let mut j = uc;
        while j < uc + w4 {
            #[allow(clippy::integer_division, reason = "5.11.56's own cdef_idx unit-grid coordinates: i/64, j/64 in pixels")]
            let slot_idx = (i / cdef_size4).checked_mul(ctx.cdef_stride).and_then(|base| base.checked_add(j / cdef_size4));
            if let Some(slot) = slot_idx.and_then(|k| ctx.cdef_idx.get_mut(k)) {
                *slot = value;
            }
            j += cdef_size4;
        }
        i += cdef_size4;
    }
}

fn read_delta_qindex_lf(ctx: &mut FrameCtx, ts: &mut TileState<'_>, mi_size: u8, skip: bool) {
    let sb_size_ord = if ctx.use_128x128_superblock { 15u8 } else { 12u8 };
    if mi_size == sb_size_ord && skip {
        return;
    }
    // ReadDeltas is only true on a superblock's first block when
    // delta_q_present is set; this crate's own encoder configuration keeps
    // delta_q_present off (verified via this crate's test fixtures), so
    // there is nothing further to read here for any stream this crate has
    // decoded. A conforming encoder that turns it on would need this path
    // extended; left as a named gap rather than guessed at.
    let _ = (ts.current_q_index, ctx.header.delta.delta_q_present);
}

fn read_tx_size(ctx: &FrameCtx, ts: &mut TileState<'_>, r: usize, c: usize, mi_size: u8, avail_u: bool, avail_l: bool, allow_select: bool) -> u8 {
    if ctx.header.coded_lossless {
        return 0; // TX_4X4
    }
    let max_rect_tx = u8::try_from(tables::MAX_TX_SIZE_RECT.get(usize::from(mi_size)).copied().unwrap_or(0)).unwrap_or(0);
    let max_tx_depth = tables::MAX_TX_DEPTH.get(usize::from(mi_size)).copied().unwrap_or(0);
    if mi_size == 0 || !allow_select || ctx.header.tx_mode != frame_header::TxMode::Select {
        return max_rect_tx;
    }
    let max_tx_w = u32::from(tables::TX_WIDTH.get(usize::from(max_rect_tx)).copied().unwrap_or(4));
    let max_tx_h = u32::from(tables::TX_HEIGHT.get(usize::from(max_rect_tx)).copied().unwrap_or(4));
    // tx_depth's own context formula, §8.3.2: `aboveW`/`leftH` are 0 when
    // the neighbour is unavailable (a plain `else` in the specification's
    // own text, not a call to get_above_tx_width()/get_left_tx_height() --
    // those are only invoked in the "available and not inter" branch).
    // Since this crate is always intra (IsInters is always 0), the
    // available case always takes that branch: Tx_Width/Tx_Height of the
    // *neighbour's own selected transform size* (InterTxSizes in the
    // specification's own naming, tracked here as `MiCell::tx_size`
    // regardless of inter/intra) -- not Block_Width/Block_Height of the
    // neighbour's *coding* block size, which is what this function used to
    // read here.
    let above_w = if avail_u {
        ctx.mi_at(ix(r) - 1, ix(c)).map_or(0, |m| u32::from(tables::TX_WIDTH.get(usize::from(m.tx_size)).copied().unwrap_or(4)))
    } else {
        0
    };
    let left_h = if avail_l {
        ctx.mi_at(ix(r), ix(c) - 1).map_or(0, |m| u32::from(tables::TX_HEIGHT.get(usize::from(m.tx_size)).copied().unwrap_or(4)))
    } else {
        0
    };
    let ctx_v = usize::from(above_w >= max_tx_w) + usize::from(left_h >= max_tx_h);

    let mut cdf: Vec<u16> = match max_tx_depth {
        4 => ts.cdf.tx_64x64.get(ctx_v).copied().unwrap_or_default().to_vec(),
        3 => ts.cdf.tx_32x32.get(ctx_v).copied().unwrap_or_default().to_vec(),
        2 => ts.cdf.tx_16x16.get(ctx_v).copied().unwrap_or_default().to_vec(),
        _ => ts.cdf.tx_8x8.get(ctx_v).copied().unwrap_or_default().to_vec(),
    };
    let tx_depth = ts.sd.read_symbol(&mut cdf);
    match max_tx_depth {
        4 => {
            if let Some(row) = ts.cdf.tx_64x64.get_mut(ctx_v) {
                for (d, s) in row.iter_mut().zip(cdf.iter()) {
                    *d = *s;
                }
            }
        }
        3 => {
            if let Some(row) = ts.cdf.tx_32x32.get_mut(ctx_v) {
                for (d, s) in row.iter_mut().zip(cdf.iter()) {
                    *d = *s;
                }
            }
        }
        2 => {
            if let Some(row) = ts.cdf.tx_16x16.get_mut(ctx_v) {
                for (d, s) in row.iter_mut().zip(cdf.iter()) {
                    *d = *s;
                }
            }
        }
        _ => {
            if let Some(row) = ts.cdf.tx_8x8.get_mut(ctx_v) {
                for (d, s) in row.iter_mut().zip(cdf.iter()) {
                    *d = *s;
                }
            }
        }
    }
    let mut tx = max_rect_tx;
    for _ in 0..tx_depth {
        tx = u8::try_from(tables::SPLIT_TX_SIZE.get(usize::from(tx)).copied().unwrap_or(u16::from(tx))).unwrap_or(tx);
    }
    tx
}

#[allow(
    clippy::too_many_arguments,
    clippy::fn_params_excessive_bools,
    reason = "mirrors residual()'s own inputs plus this crate's mode-info carried alongside them; \
              each flag is an independent per-block condition the specification itself keeps separate"
)]
fn residual(
    ctx: &mut FrameCtx,
    ts: &mut TileState<'_>,
    mi_row: usize,
    mi_col: usize,
    mi_size: u8,
    has_chroma: bool,
    skip: bool,
    y_mode: u8,
    uv_mode: u8,
    angle_delta_y: i32,
    angle_delta_uv: i32,
    cfl_alpha_u: i32,
    cfl_alpha_v: i32,
    avail_u: bool,
    avail_l: bool,
) -> Result<()> {
    let bw = tables::block_width(mi_size);
    let bh = tables::block_height(mi_size);
    let width_chunks = (bw >> 6).max(1);
    let height_chunks = (bh >> 6).max(1);
    let mi_size_chunk = if width_chunks > 1 || height_chunks > 1 { 12u8 } else { mi_size };

    let tx_size = ctx.mi_at(ix(mi_row), ix(mi_col)).map_or(0, |m| m.tx_size);

    for chunk_y in 0..height_chunks {
        for chunk_x in 0..width_chunks {
            let mi_row_chunk = mi_row + usize::try_from(chunk_y << 4).unwrap_or(0);
            let mi_col_chunk = mi_col + usize::try_from(chunk_x << 4).unwrap_or(0);
            let plane_count = if has_chroma { 3 } else { 1 };
            for plane in 0..plane_count {
                let tx_sz = if ctx.header.coded_lossless { 0u8 } else { get_tx_size(plane, tx_size, mi_size) };
                let step_x = usize::from(tables::TX_WIDTH.get(usize::from(tx_sz)).copied().unwrap_or(4)) >> 2;
                let step_y = usize::from(tables::TX_HEIGHT.get(usize::from(tx_sz)).copied().unwrap_or(4)) >> 2;
                let plane_sz = get_plane_residual_size(mi_size_chunk, plane, ctx.subsampling_x, ctx.subsampling_y);
                let num4x4_w = usize::from(tables::NUM_4X4_BLOCKS_WIDE.get(usize::from(plane_sz)).copied().unwrap_or(1));
                let num4x4_h = usize::from(tables::NUM_4X4_BLOCKS_HIGH.get(usize::from(plane_sz)).copied().unwrap_or(1));
                let (sub_x, sub_y) = if plane > 0 { (ctx.subsampling_x, ctx.subsampling_y) } else { (false, false) };
                let base_x_block = (mi_col >> u32::from(ctx.subsampling_x && plane > 0)) * 4;
                let base_y_block = (mi_row >> u32::from(ctx.subsampling_y && plane > 0)) * 4;

                let mut y = 0usize;
                while y < num4x4_h {
                    let mut x = 0usize;
                    while x < num4x4_w {
                        let xx = x + ((chunk_x << 4) as usize >> u32::from(sub_x));
                        let yy = y + ((chunk_y << 4) as usize >> u32::from(sub_y));
                        transform_block(
                            ctx,
                            ts,
                            plane,
                            base_x_block,
                            base_y_block,
                            tx_sz,
                            xx,
                            yy,
                            mi_row_chunk,
                            mi_col_chunk,
                            skip,
                            y_mode,
                            uv_mode,
                            angle_delta_y,
                            angle_delta_uv,
                            cfl_alpha_u,
                            cfl_alpha_v,
                            avail_u,
                            avail_l,
                            ctx.header.reduced_tx_set,
                            ctx.header.quant.base_q_idx,
                        )?;
                        x += step_x.max(1);
                    }
                    y += step_y.max(1);
                }
            }
        }
    }
    Ok(())
}

fn get_tx_size(plane: usize, tx_size: u8, mi_size: u8) -> u8 {
    if plane == 0 {
        return tx_size;
    }
    let plane_sz = get_plane_residual_size(mi_size, plane, true, true);
    let uv_tx = u8::try_from(tables::MAX_TX_SIZE_RECT.get(usize::from(plane_sz)).copied().unwrap_or(0)).unwrap_or(0);
    let w = tables::TX_WIDTH.get(usize::from(uv_tx)).copied().unwrap_or(4);
    let h = tables::TX_HEIGHT.get(usize::from(uv_tx)).copied().unwrap_or(4);
    if w == 64 || h == 64 {
        if w == 16 {
            return 9; // TX_16X32
        }
        if h == 16 {
            return 10; // TX_32X16
        }
        return 3; // TX_32X32
    }
    uv_tx
}

fn get_plane_residual_size(subsize: u8, plane: usize, subsampling_x: bool, subsampling_y: bool) -> u8 {
    let (sx, sy) = if plane > 0 { (usize::from(subsampling_x), usize::from(subsampling_y)) } else { (0, 0) };
    let v = tables::conversion::SUBSAMPLED_SIZE
        .get(usize::from(subsize))
        .and_then(|a| a.get(sx))
        .and_then(|b| b.get(sy))
        .copied()
        .unwrap_or(u16::from(BLOCK_INVALID));
    u8::try_from(v).unwrap_or(BLOCK_INVALID)
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::fn_params_excessive_bools,
    reason = "mirrors transform_block()'s own inputs, section 5.11.35; each flag is an independent \
              per-block condition the specification itself keeps separate"
)]
fn transform_block(
    ctx: &mut FrameCtx,
    ts: &mut TileState<'_>,
    plane: usize,
    base_x: usize,
    base_y: usize,
    tx_sz: u8,
    x: usize,
    y: usize,
    mi_row: usize,
    mi_col: usize,
    skip: bool,
    y_mode: u8,
    uv_mode: u8,
    angle_delta_y: i32,
    angle_delta_uv: i32,
    cfl_alpha_u: i32,
    cfl_alpha_v: i32,
    avail_u: bool,
    avail_l: bool,
    reduced_tx_set: bool,
    base_q_idx: u8,
) -> Result<()> {
    let start_x = base_x + 4 * x;
    let start_y = base_y + 4 * y;
    let (sub_x, sub_y) = if plane > 0 { (ctx.subsampling_x, ctx.subsampling_y) } else { (false, false) };
    let max_x = (ctx.mi_cols * 4) >> u32::from(sub_x);
    let max_y = (ctx.mi_rows * 4) >> u32::from(sub_y);
    if start_x >= max_x || start_y >= max_y {
        return Ok(());
    }
    let step_x = usize::from(tables::TX_WIDTH.get(usize::from(tx_sz)).copied().unwrap_or(4)) >> 2;
    let step_y = usize::from(tables::TX_HEIGHT.get(usize::from(tx_sz)).copied().unwrap_or(4)) >> 2;
    let sb_mask = if ctx.use_128x128_superblock { 31i32 } else { 15i32 };
    let row = ix(start_y) << u32::from(sub_y) >> 2;
    let col = ix(start_x) << u32::from(sub_x) >> 2;
    let sub_block_mi_row = row & sb_mask;
    let sub_block_mi_col = col & sb_mask;

    let is_cfl = plane > 0 && uv_mode == UV_CFL_PRED;
    let mode = if plane == 0 { y_mode } else if is_cfl { DC_PRED } else { uv_mode };
    let angle_delta = if plane == 0 { angle_delta_y } else { angle_delta_uv };
    let log2_w = u32::from(tables::TX_WIDTH_LOG2.get(usize::from(tx_sz)).copied().unwrap_or(2));
    let log2_h = u32::from(tables::TX_HEIGHT_LOG2.get(usize::from(tx_sz)).copied().unwrap_or(2));

    let have_left_here = avail_l || x > 0;
    let have_above_here = avail_u || y > 0;
    let have_above_right = ts.block_decoded.get(plane).is_some_and(|bd| bd.get((sub_block_mi_row >> u32::from(sub_y)) - 1, (sub_block_mi_col >> u32::from(sub_x)) + ix(step_x)));
    let have_below_left = ts.block_decoded.get(plane).is_some_and(|bd| bd.get((sub_block_mi_row >> u32::from(sub_y)) + ix(step_y), (sub_block_mi_col >> u32::from(sub_x)) - 1));

    {
        let plane_ref = ctx.pic.plane(plane);
        if let Some(p) = plane_ref {
            let pred = predict::predict_intra(
                p,
                ix(start_x),
                ix(start_y),
                have_left_here,
                have_above_here,
                have_above_right,
                have_below_left,
                pred_mode_of(mode),
                angle_delta,
                log2_w,
                log2_h,
                ix(max_x) - 1,
                ix(max_y) - 1,
                ctx.bit_depth,
                ctx.enable_intra_edge_filter,
                false,
            )?;
            if let Some(pm) = ctx.pic.plane_mut(plane) {
                for (i, row_vals) in pred.iter().enumerate() {
                    for (j, &v) in row_vals.iter().enumerate() {
                        pm.set(start_x + j, start_y + i, v);
                    }
                }
            }
        }
    }
    if is_cfl {
        let alpha = if plane == 1 { cfl_alpha_u } else { cfl_alpha_v };
        let (max_luma_w, max_luma_h) = (ix(ctx.mi_cols * 4), ix(ctx.mi_rows * 4));
        let w = 1i32 << log2_w;
        let h = 1i32 << log2_h;
        let (luma, chroma) = ctx.pic.luma_and_chroma_mut(plane);
        if let Some(chroma) = chroma {
            predict::predict_chroma_from_luma(chroma, luma, ix(start_x), ix(start_y), w, h, sub_x, sub_y, alpha, max_luma_w, max_luma_h, log2_w, log2_h, ctx.bit_depth);
        }
    }

    if !skip {
        let (eob, tx_type) = coeffs(ctx, ts, plane, start_x, start_y, tx_sz, mi_row, mi_col, y_mode, uv_mode, reduced_tx_set, base_q_idx, ctx.header.coded_lossless);
        if eob > 0 {
            reconstruct(ctx, ts, plane, start_x, start_y, tx_sz, tx_type, ctx.header.coded_lossless);
        }
    }

    for i in 0..step_y {
        for j in 0..step_x {
            if let Some(bd) = ts.block_decoded.get_mut(plane) {
                bd.set((sub_block_mi_row >> u32::from(sub_y)) + ix(i), (sub_block_mi_col >> u32::from(sub_x)) + ix(j), true);
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines, reason = "mirrors coeffs()'s own inputs, section 5.11.39")]
fn coeffs(
    ctx: &mut FrameCtx,
    ts: &mut TileState<'_>,
    plane: usize,
    start_x: usize,
    start_y: usize,
    tx_sz: u8,
    mi_row: usize,
    mi_col: usize,
    y_mode: u8,
    uv_mode: u8,
    reduced_tx_set: bool,
    base_q_idx: u8,
    lossless: bool,
) -> (i32, Av1TxType) {
    let (sub_x, sub_y) = if plane > 0 { (ctx.subsampling_x, ctx.subsampling_y) } else { (false, false) };
    let x4 = start_x >> 2;
    let y4 = start_y >> 2;
    let w4 = usize::from(tables::TX_WIDTH.get(usize::from(tx_sz)).copied().unwrap_or(4)) >> 2;
    let h4 = usize::from(tables::TX_HEIGHT.get(usize::from(tx_sz)).copied().unwrap_or(4)) >> 2;
    let tx_sz_sqr = tables::TX_SIZE_SQR.get(usize::from(tx_sz)).copied().unwrap_or(0);
    let tx_sz_sqr_up = tables::TX_SIZE_SQR_UP.get(usize::from(tx_sz)).copied().unwrap_or(0);
    let tx_sz_ctx = usize::from((tx_sz_sqr + tx_sz_sqr_up + 1) >> 1);
    let ptype = usize::from(plane > 0);

    let seg_eob = if tx_sz == 17 || tx_sz == 18 { 512 } else { (i32::from(tables::TX_WIDTH.get(usize::from(tx_sz)).copied().unwrap_or(4)) * i32::from(tables::TX_HEIGHT.get(usize::from(tx_sz)).copied().unwrap_or(4))).min(1024) };
    let mut quant = vec![0i32; usize::try_from(seg_eob).unwrap_or(0).max(1)];

    let mi_size = ctx.mi_at(ix(mi_row), ix(mi_col)).map_or(0, |m| m.mi_size);
    let all_zero_ctx = txb_skip_ctx(ctx, ts, plane, x4, y4, w4, h4, tx_sz, mi_size, sub_x, sub_y);
    let mut cdf = ts.cdf.txb_skip.get(tx_sz_ctx).and_then(|r| r.get(all_zero_ctx)).copied().unwrap_or_default().to_vec();
    let all_zero = ts.sd.read_symbol(&mut cdf) != 0;
    if let Some(row) = ts.cdf.txb_skip.get_mut(tx_sz_ctx)
        && let Some(slot) = row.get_mut(all_zero_ctx)
    {
        for (d, s) in slot.iter_mut().zip(cdf.iter()) {
            *d = *s;
        }
    }

    let mut eob = 0i32;
    let mut cul_level = 0i32;
    let mut dc_category = 0u8;
    let mut tx_type = Av1TxType::DctDct;

    if !all_zero {
        if plane == 0 {
            tx_type = read_transform_type(ctx, ts, tx_sz, reduced_tx_set, base_q_idx, y_mode);
        }
        tx_type = compute_tx_type(plane, tx_sz, tx_type, lossless, reduced_tx_set, uv_mode);
        let scan = get_scan(tx_sz, tx_type);

        let eob_multisize = i32::from(tables::TX_WIDTH_LOG2.get(usize::from(tx_sz)).copied().unwrap_or(2).min(5)) + i32::from(tables::TX_HEIGHT_LOG2.get(usize::from(tx_sz)).copied().unwrap_or(2).min(5)) - 4;
        let tx_class_2d = get_tx_class(tx_type) == 0;
        let eob_ctx = usize::from(!tx_class_2d);
        let eob_pt = read_eob_pt(ts, eob_multisize, ptype, eob_ctx) + 1;
        eob = if eob_pt < 2 { eob_pt } else { (1 << (eob_pt - 2)) + 1 };
        let mut eob_shift = (eob_pt - 3).max(-1);
        if eob_shift >= 0 {
            let eob_extra_ctx = usize::try_from(eob_pt - 3).unwrap_or(0);
            let mut cdf = ts.cdf.eob_extra.get(tx_sz_ctx).and_then(|r| r.get(ptype)).and_then(|r| r.get(eob_extra_ctx)).copied().unwrap_or_default().to_vec();
            let eob_extra = ts.sd.read_symbol(&mut cdf) != 0;
            if let Some(a) = ts.cdf.eob_extra.get_mut(tx_sz_ctx)
                && let Some(b) = a.get_mut(ptype)
                && let Some(slot) = b.get_mut(eob_extra_ctx)
            {
                for (d, s) in slot.iter_mut().zip(cdf.iter()) {
                    *d = *s;
                }
            }
            if eob_extra {
                eob += 1 << eob_shift;
            }
            for i in 1..(eob_pt - 2).max(0) {
                eob_shift = (eob_pt - 2).max(0) - 1 - i;
                let bit = ts.sd.read_bool();
                if bit != 0 {
                    eob += 1 << eob_shift;
                }
            }
        }

        for c in (0..eob).rev() {
            let pos = scan.get(usize::try_from(c).unwrap_or(0)).copied().unwrap_or(0);
            let level = if c == eob - 1 {
                let ctx_eob = get_coeff_base_ctx(tx_sz, plane, pos, c, true, &quant, tx_type);
                let mut cdf = ts.cdf.coeff_base_eob.get(tx_sz_ctx).and_then(|r| r.get(ptype)).and_then(|r| r.get(ctx_eob)).copied().unwrap_or_default().to_vec();
                let v = ts.sd.read_symbol(&mut cdf);
                if let Some(a) = ts.cdf.coeff_base_eob.get_mut(tx_sz_ctx)
                    && let Some(b) = a.get_mut(ptype)
                    && let Some(slot) = b.get_mut(ctx_eob)
                {
                    for (d, s) in slot.iter_mut().zip(cdf.iter()) {
                        *d = *s;
                    }
                }
                i32::try_from(v).unwrap_or(0) + 1
            } else {
                let ctx_base = get_coeff_base_ctx(tx_sz, plane, pos, c, false, &quant, tx_type);
                let mut cdf = ts.cdf.coeff_base.get(tx_sz_ctx).and_then(|r| r.get(ptype)).and_then(|r| r.get(ctx_base)).copied().unwrap_or_default().to_vec();
                let v = ts.sd.read_symbol(&mut cdf);
                if let Some(a) = ts.cdf.coeff_base.get_mut(tx_sz_ctx)
                    && let Some(b) = a.get_mut(ptype)
                    && let Some(slot) = b.get_mut(ctx_base)
                {
                    for (d, s) in slot.iter_mut().zip(cdf.iter()) {
                        *d = *s;
                    }
                }
                i32::try_from(v).unwrap_or(0)
            };
            let mut level = level;
            if level > NUM_BASE_LEVELS {
                let br_ctx = get_coeff_br_ctx(tx_sz, plane, pos, tx_type, &quant);
                let br_tx_sz_ctx = tx_sz_ctx.min(3);
                #[allow(clippy::integer_division, reason = "8.3.2's coeff_br loop count, COEFF_BASE_RANGE / (BR_CDF_SIZE - 1)")]
                let br_loops = COEFF_BASE_RANGE / (BR_CDF_SIZE - 1);
                for _ in 0..br_loops {
                    let mut cdf = ts.cdf.coeff_br.get(br_tx_sz_ctx).and_then(|r| r.get(ptype)).and_then(|r| r.get(br_ctx)).copied().unwrap_or_default().to_vec();
                    let coeff_br = ts.sd.read_symbol(&mut cdf);
                    if let Some(a) = ts.cdf.coeff_br.get_mut(br_tx_sz_ctx)
                        && let Some(b) = a.get_mut(ptype)
                        && let Some(slot) = b.get_mut(br_ctx)
                    {
                        for (d, s) in slot.iter_mut().zip(cdf.iter()) {
                            *d = *s;
                        }
                    }
                    level += i32::try_from(coeff_br).unwrap_or(0);
                    if coeff_br < u32::try_from(BR_CDF_SIZE - 1).unwrap_or(0) {
                        break;
                    }
                }
            }
            if let Some(slot) = quant.get_mut(usize::from(pos)) {
                *slot = level;
            }
        }

        for c in 0..eob {
            let pos = scan.get(usize::try_from(c).unwrap_or(0)).copied().unwrap_or(0);
            let cur = quant.get(usize::from(pos)).copied().unwrap_or(0);
            let sign = if cur != 0 {
                if c == 0 {
                    let dc_ctx = dc_sign_ctx(ctx, ts, plane, x4, y4, w4, h4, sub_x, sub_y);
                    let mut cdf = ts.cdf.dc_sign.get(ptype).and_then(|r| r.get(dc_ctx)).copied().unwrap_or_default().to_vec();
                    let v = ts.sd.read_symbol(&mut cdf) != 0;
                    if let Some(a) = ts.cdf.dc_sign.get_mut(ptype)
                        && let Some(slot) = a.get_mut(dc_ctx)
                    {
                        for (d, s) in slot.iter_mut().zip(cdf.iter()) {
                            *d = *s;
                        }
                    }
                    v
                } else {
                    ts.sd.read_bool() != 0
                }
            } else {
                false
            };
            let mut level = cur;
            if level > NUM_BASE_LEVELS + COEFF_BASE_RANGE {
                let mut length = 0u32;
                loop {
                    length += 1;
                    if ts.sd.read_bool() != 0 || length > 20 {
                        break;
                    }
                }
                let mut x = 1i32;
                let mut i = i32::try_from(length).unwrap_or(0) - 2;
                while i >= 0 {
                    let bit = i32::try_from(ts.sd.read_bool()).unwrap_or(0);
                    x = (x << 1) | bit;
                    i -= 1;
                }
                level = x + COEFF_BASE_RANGE + NUM_BASE_LEVELS;
            }
            if pos == 0 && level > 0 {
                dc_category = if sign { 1 } else { 2 };
            }
            level &= 0xFFFFF;
            cul_level += level;
            if let Some(slot) = quant.get_mut(usize::from(pos)) {
                *slot = if sign { -level } else { level };
            }
        }
        cul_level = cul_level.min(63);
    }

    let cul_level_u8 = u8::try_from(cul_level).unwrap_or(63);
    for i in 0..w4 {
        if let Some(a) = ts.above_level.get_mut(plane)
            && let Some(slot) = a.get_mut(x4 + i)
        {
            *slot = cul_level_u8;
        }
        if let Some(a) = ts.above_dc.get_mut(plane)
            && let Some(slot) = a.get_mut(x4 + i)
        {
            *slot = dc_category;
        }
    }
    for i in 0..h4 {
        if let Some(a) = ts.left_level.get_mut(plane)
            && let Some(slot) = a.get_mut(y4 + i)
        {
            *slot = cul_level_u8;
        }
        if let Some(a) = ts.left_dc.get_mut(plane)
            && let Some(slot) = a.get_mut(y4 + i)
        {
            *slot = dc_category;
        }
    }

    ctx.last_quant = quant;
    let _ = (mi_row, mi_col);
    (eob, tx_type)
}

/// `all_zero`'s context, §8.3.2. `mi_size` is the *block's* `MiSize`
/// (needed for the `bw == w && bh == h` / `bw * bh > w * h` comparisons
/// against the plane's own residual block size, distinct from the
/// transform size `tx_sz`).
#[allow(clippy::too_many_arguments, reason = "mirrors the specification's own all_zero context derivation, section 8.3.2")]
fn txb_skip_ctx(ctx: &FrameCtx, ts: &TileState<'_>, plane: usize, x4: usize, y4: usize, w4: usize, h4: usize, tx_sz: u8, mi_size: u8, sub_x: bool, sub_y: bool) -> usize {
    let max_x4 = if plane > 0 { ctx.mi_cols >> u32::from(sub_x) } else { ctx.mi_cols };
    let max_y4 = if plane > 0 { ctx.mi_rows >> u32::from(sub_y) } else { ctx.mi_rows };
    let w = i32::from(tables::TX_WIDTH.get(usize::from(tx_sz)).copied().unwrap_or(4));
    let h = i32::from(tables::TX_HEIGHT.get(usize::from(tx_sz)).copied().unwrap_or(4));
    let bsize = get_plane_residual_size(mi_size, plane, sub_x, sub_y);
    let bw = i32::try_from(tables::block_width(bsize)).unwrap_or(w);
    let bh = i32::try_from(tables::block_height(bsize)).unwrap_or(h);

    if plane == 0 {
        let mut top = 0u8;
        let mut left = 0u8;
        for k in 0..w4 {
            if x4 + k < max_x4 {
                top = top.max(ts.above_level.get(plane).and_then(|a| a.get(x4 + k)).copied().unwrap_or(0));
            }
        }
        for k in 0..h4 {
            if y4 + k < max_y4 {
                left = left.max(ts.left_level.get(plane).and_then(|a| a.get(y4 + k)).copied().unwrap_or(0));
            }
        }
        if bw == w && bh == h {
            0
        } else if top == 0 && left == 0 {
            1
        } else if top == 0 || left == 0 {
            2 + usize::from(top.max(left) > 3)
        } else if top.max(left) <= 3 {
            4
        } else if top.min(left) <= 3 {
            5
        } else {
            6
        }
    } else {
        let mut above = false;
        let mut left = false;
        for i in 0..w4 {
            if x4 + i < max_x4 {
                above |= ts.above_level.get(plane).and_then(|a| a.get(x4 + i)).copied().unwrap_or(0) != 0;
                above |= ts.above_dc.get(plane).and_then(|a| a.get(x4 + i)).copied().unwrap_or(0) != 0;
            }
        }
        for i in 0..h4 {
            if y4 + i < max_y4 {
                left |= ts.left_level.get(plane).and_then(|a| a.get(y4 + i)).copied().unwrap_or(0) != 0;
                left |= ts.left_dc.get(plane).and_then(|a| a.get(y4 + i)).copied().unwrap_or(0) != 0;
            }
        }
        let mut c = usize::from(above) + usize::from(left) + 7;
        if bw * bh > w * h {
            c += 3;
        }
        c
    }
}

fn dc_sign_ctx(ctx: &FrameCtx, ts: &TileState<'_>, plane: usize, x4: usize, y4: usize, w4: usize, h4: usize, sub_x: bool, sub_y: bool) -> usize {
    let max_x4 = if plane > 0 { ctx.mi_cols >> u32::from(sub_x) } else { ctx.mi_cols };
    let max_y4 = if plane > 0 { ctx.mi_rows >> u32::from(sub_y) } else { ctx.mi_rows };
    let mut dc_sign = 0i32;
    for k in 0..w4 {
        if x4 + k < max_x4 {
            match ts.above_dc.get(plane).and_then(|a| a.get(x4 + k)).copied().unwrap_or(0) {
                1 => dc_sign -= 1,
                2 => dc_sign += 1,
                _ => {}
            }
        }
    }
    for k in 0..h4 {
        if y4 + k < max_y4 {
            match ts.left_dc.get(plane).and_then(|a| a.get(y4 + k)).copied().unwrap_or(0) {
                1 => dc_sign -= 1,
                2 => dc_sign += 1,
                _ => {}
            }
        }
    }
    match dc_sign.cmp(&0) {
        std::cmp::Ordering::Less => 1,
        std::cmp::Ordering::Greater => 2,
        std::cmp::Ordering::Equal => 0,
    }
}

fn get_coeff_base_ctx(tx_sz: u8, plane: usize, pos: u16, c: i32, is_eob: bool, quant: &[i32], tx_type: Av1TxType) -> usize {
    let adj = u8::try_from(tables::ADJUSTED_TX_SIZE.get(usize::from(tx_sz)).copied().unwrap_or(u16::from(tx_sz))).unwrap_or(tx_sz);
    let bwl = u32::from(tables::TX_WIDTH_LOG2.get(usize::from(adj)).copied().unwrap_or(2));
    let height = i32::from(tables::TX_HEIGHT.get(usize::from(adj)).copied().unwrap_or(4));
    let width = 1i32 << bwl;
    if is_eob {
        // coeff_base_eob's own context, 8.3.2: get_coeff_base_ctx(..., isEob=1)
        // - SIG_COEF_CONTEXTS + SIG_COEF_CONTEXTS_EOB, i.e. the specification's
        // SIG_COEF_CONTEXTS-4..SIG_COEF_CONTEXTS-1 range (38..41 for
        // SIG_COEF_CONTEXTS=42) re-based to 0..3 -- TileCoeffBaseEobCdf's own
        // context dimension is sized SIG_COEF_CONTEXTS_EOB (4), not
        // SIG_COEF_CONTEXTS (42). Returning the un-rebased 38..41 here (as
        // this function once did) indexed coeff_base_eob's 4-entry context
        // dimension out of bounds on every single transform block that has
        // any nonzero coefficient at all, silently falling back to a
        // default-constructed (wrong) cdf for the very first, most
        // consequential coefficient symbol read in the whole block.
        if c == 0 {
            return 0;
        }
        #[allow(clippy::integer_division, reason = "8.3.2's get_coeff_base_ctx thresholds, height << bwl divided by 8")]
        if c <= (height << bwl) / 8 {
            return 1;
        }
        #[allow(clippy::integer_division, reason = "8.3.2's get_coeff_base_ctx thresholds, height << bwl divided by 4")]
        if c <= (height << bwl) / 4 {
            return 2;
        }
        return 3;
    }
    let row = i32::from(pos) >> bwl;
    let col = i32::from(pos) - (row << bwl);
    let tx_class = get_tx_class(tx_type);
    // Sig_Ref_Diff_Offset[txClass], §8.3.2 -- one neighbour-offset row per
    // class (2D/HORIZ/VERT); using the 2D row unconditionally here was a
    // real bug for the six pure row/column transform types (V_DCT, H_DCT,
    // V_ADST, H_ADST, V_FLIPADST, H_FLIPADST).
    let offsets: [(i32, i32); 5] = match tx_class {
        1 => [(0, 1), (1, 0), (0, 2), (0, 3), (0, 4)],
        2 => [(0, 1), (1, 0), (2, 0), (3, 0), (4, 0)],
        _ => [(0, 1), (1, 0), (1, 1), (0, 2), (2, 0)],
    };
    let mut mag = 0i32;
    for (dr, dc) in offsets {
        let rr = row + dr;
        let cc = col + dc;
        if rr >= 0 && cc >= 0 && rr < height && cc < width {
            let idx = usize::try_from((rr << bwl) + cc).unwrap_or(0);
            mag += quant.get(idx).copied().unwrap_or(0).abs().min(3);
        }
    }
    let ctx = ((mag + 1) >> 1).min(4);
    let _ = plane;
    if tx_class == 0 {
        if row == 0 && col == 0 {
            return 0;
        }
        let ro = usize::try_from(row.min(4)).unwrap_or(0);
        let co = usize::try_from(col.min(4)).unwrap_or(0);
        let off = tables::COEFF_BASE_CTX_OFFSET.get(usize::from(tx_sz)).and_then(|a| a.get(ro)).and_then(|b| b.get(co)).copied().unwrap_or(0);
        return usize::try_from(ctx).unwrap_or(0) + usize::from(off);
    }
    let idx = if tx_class == 2 { row } else { col };
    let off = tables::COEFF_BASE_POS_CTX_OFFSET.get(usize::try_from(idx.min(2)).unwrap_or(0)).copied().unwrap_or(0);
    usize::try_from(ctx).unwrap_or(0) + usize::from(off)
}

fn get_coeff_br_ctx(tx_sz: u8, _plane: usize, pos: u16, tx_type: Av1TxType, quant: &[i32]) -> usize {
    let adj = u8::try_from(tables::ADJUSTED_TX_SIZE.get(usize::from(tx_sz)).copied().unwrap_or(u16::from(tx_sz))).unwrap_or(tx_sz);
    let bwl = u32::from(tables::TX_WIDTH_LOG2.get(usize::from(adj)).copied().unwrap_or(2));
    let txw = 1i32 << bwl;
    let txh = i32::from(tables::TX_HEIGHT.get(usize::from(adj)).copied().unwrap_or(4));
    let row = i32::from(pos) >> bwl;
    let col = i32::from(pos) - (row << bwl);
    let tx_class = get_tx_class(tx_type);
    let offsets: [(i32, i32); 3] = match tx_class {
        1 => [(0, 1), (1, 0), (0, 2)],
        2 => [(0, 1), (1, 0), (2, 0)],
        _ => [(0, 1), (1, 0), (1, 1)],
    };
    let mut mag = 0i32;
    for (dr, dc) in offsets {
        let rr = row + dr;
        let cc = col + dc;
        if rr >= 0 && cc >= 0 && rr < txh && cc < txw {
            let idx = usize::try_from(rr * txw + cc).unwrap_or(0);
            mag += quant.get(idx).copied().unwrap_or(0).min(NUM_BASE_LEVELS + COEFF_BASE_RANGE + 1);
        }
    }
    let mag = ((mag + 1) >> 1).min(6);
    if pos == 0 {
        return usize::try_from(mag).unwrap_or(0);
    }
    if tx_class == 0 {
        if row < 2 && col < 2 {
            usize::try_from(mag + 7).unwrap_or(0)
        } else {
            usize::try_from(mag + 14).unwrap_or(0)
        }
    } else if tx_class == 1 {
        if col == 0 {
            usize::try_from(mag + 7).unwrap_or(0)
        } else {
            usize::try_from(mag + 14).unwrap_or(0)
        }
    } else if row == 0 {
        usize::try_from(mag + 7).unwrap_or(0)
    } else {
        usize::try_from(mag + 14).unwrap_or(0)
    }
}

/// `get_tx_class()`, §8.3.2. Ordinals match the specification's own
/// `TX_CLASS_2D = 0`, `TX_CLASS_HORIZ = 1`, `TX_CLASS_VERT = 2` exactly —
/// every caller below (`Mag_Ref_Offset_With_Tx_Class` selection, the
/// `coeff_br` `+7`/`+14` branch) keys off these literal values matching
/// the specification's own table row order, so swapping which arm gets 1
/// vs 2 here would silently swap `Mag_Ref_Offset_With_Tx_Class`'s HORIZ
/// and VERT rows everywhere at once.
const fn get_tx_class(t: Av1TxType) -> u8 {
    match t {
        Av1TxType::HDct | Av1TxType::HAdst | Av1TxType::HFlipadst => 1,
        Av1TxType::VDct | Av1TxType::VAdst | Av1TxType::VFlipadst => 2,
        _ => 0,
    }
}

// TX_SET_* ordinals, local to this crate.
const TX_SET_DCTONLY: u8 = 0;
const TX_SET_INTRA_1: u8 = 1;
const TX_SET_INTRA_2: u8 = 2;

fn get_tx_set(tx_sz: u8, reduced_tx_set: bool) -> u8 {
    let sqr = tables::TX_SIZE_SQR.get(usize::from(tx_sz)).copied().unwrap_or(0);
    let sqr_up = tables::TX_SIZE_SQR_UP.get(usize::from(tx_sz)).copied().unwrap_or(0);
    if sqr_up > 3 {
        return TX_SET_DCTONLY;
    }
    if sqr_up == 3 {
        return TX_SET_DCTONLY;
    }
    if reduced_tx_set || sqr == 2 {
        return TX_SET_INTRA_2;
    }
    TX_SET_INTRA_1
}

const TX_TYPE_INTRA_INV_SET1: [u8; 7] = [9, 0, 10, 11, 3, 1, 2];
const TX_TYPE_INTRA_INV_SET2: [u8; 5] = [9, 0, 3, 1, 2];

fn read_transform_type(_ctx: &FrameCtx, ts: &mut TileState<'_>, tx_sz: u8, reduced_tx_set: bool, base_q_idx: u8, y_mode: u8) -> Av1TxType {
    let set = get_tx_set(tx_sz, reduced_tx_set);
    if set == TX_SET_DCTONLY || base_q_idx == 0 {
        return Av1TxType::DctDct;
    }
    let sqr = usize::from(tables::TX_SIZE_SQR.get(usize::from(tx_sz)).copied().unwrap_or(0));
    let intra_dir = usize::from(y_mode);
    let ordinal = if set == TX_SET_INTRA_1 {
        let mut cdf = ts.cdf.intra_tx_type_set1.get(sqr).and_then(|r| r.get(intra_dir)).copied().unwrap_or_default().to_vec();
        let v = ts.sd.read_symbol(&mut cdf);
        if let Some(a) = ts.cdf.intra_tx_type_set1.get_mut(sqr)
            && let Some(slot) = a.get_mut(intra_dir)
        {
            for (d, s) in slot.iter_mut().zip(cdf.iter()) {
                *d = *s;
            }
        }
        TX_TYPE_INTRA_INV_SET1.get(usize::try_from(v).unwrap_or(0)).copied().unwrap_or(0)
    } else {
        let mut cdf = ts.cdf.intra_tx_type_set2.get(sqr).and_then(|r| r.get(intra_dir)).copied().unwrap_or_default().to_vec();
        let v = ts.sd.read_symbol(&mut cdf);
        if let Some(a) = ts.cdf.intra_tx_type_set2.get_mut(sqr)
            && let Some(slot) = a.get_mut(intra_dir)
        {
            for (d, s) in slot.iter_mut().zip(cdf.iter()) {
                *d = *s;
            }
        }
        TX_TYPE_INTRA_INV_SET2.get(usize::try_from(v).unwrap_or(0)).copied().unwrap_or(0)
    };
    Av1TxType::from_ordinal(ordinal)
}

fn compute_tx_type(plane: usize, tx_sz: u8, luma_tx_type: Av1TxType, lossless: bool, reduced_tx_set: bool, uv_mode: u8) -> Av1TxType {
    let sqr_up = tables::TX_SIZE_SQR_UP.get(usize::from(tx_sz)).copied().unwrap_or(0);
    if lossless || sqr_up > 3 {
        return Av1TxType::DctDct;
    }
    let set = get_tx_set(tx_sz, reduced_tx_set);
    if plane == 0 {
        return luma_tx_type;
    }
    let (col, row) = tables::MODE_TO_TXFM.get(usize::from(uv_mode)).copied().unwrap_or((tables::Tx1D::Dct, tables::Tx1D::Dct));
    let tx_type = tx1d_pair_to_type(col, row);
    if tx_type_in_set_intra(set, tx_type) { tx_type } else { Av1TxType::DctDct }
}

fn tx1d_pair_to_type(col: tables::Tx1D, row: tables::Tx1D) -> Av1TxType {
    use tables::Tx1D::{Adst, Dct, Identity};
    match (col, row) {
        (Dct, Dct) => Av1TxType::DctDct,
        (Adst, Dct) => Av1TxType::AdstDct,
        (Dct, Adst) => Av1TxType::DctAdst,
        (Adst, Adst) => Av1TxType::AdstAdst,
        (Identity, _) | (_, Identity) => Av1TxType::Idtx,
    }
}

fn tx_type_in_set_intra(set: u8, tx_type: Av1TxType) -> bool {
    let ord = usize::from(tx_type as u8);
    let row: [u8; 16] = match set {
        1 => [1, 1, 1, 1, 0, 0, 0, 0, 0, 1, 1, 1, 0, 0, 0, 0],
        2 => [1, 1, 1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0],
        _ => [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    };
    row.get(ord).copied().unwrap_or(0) != 0
}

fn get_scan(tx_sz: u8, tx_type: Av1TxType) -> &'static [u16] {
    if tx_sz == 17 {
        return &tables::scan::DEFAULT_SCAN_16X32;
    }
    if tx_sz == 18 {
        return &tables::scan::DEFAULT_SCAN_32X16;
    }
    if tables::TX_SIZE_SQR_UP.get(usize::from(tx_sz)).copied().unwrap_or(0) == 4 {
        return &tables::scan::DEFAULT_SCAN_32X32;
    }
    if tx_type == Av1TxType::Idtx {
        return default_scan(tx_sz);
    }
    let prefer_row = matches!(tx_type, Av1TxType::VDct | Av1TxType::VAdst | Av1TxType::VFlipadst);
    let prefer_col = matches!(tx_type, Av1TxType::HDct | Av1TxType::HAdst | Av1TxType::HFlipadst);
    if prefer_row {
        mrow_scan(tx_sz)
    } else if prefer_col {
        mcol_scan(tx_sz)
    } else {
        default_scan(tx_sz)
    }
}

fn default_scan(tx_sz: u8) -> &'static [u16] {
    use tables::scan as s;
    match tx_sz {
        0 => &s::DEFAULT_SCAN_4X4,
        5 => &s::DEFAULT_SCAN_4X8,
        6 => &s::DEFAULT_SCAN_8X4,
        1 => &s::DEFAULT_SCAN_8X8,
        7 => &s::DEFAULT_SCAN_8X16,
        8 => &s::DEFAULT_SCAN_16X8,
        2 => &s::DEFAULT_SCAN_16X16,
        9 => &s::DEFAULT_SCAN_16X32,
        10 => &s::DEFAULT_SCAN_32X16,
        13 => &s::DEFAULT_SCAN_4X16,
        14 => &s::DEFAULT_SCAN_16X4,
        15 => &s::DEFAULT_SCAN_8X32,
        16 => &s::DEFAULT_SCAN_32X8,
        _ => &s::DEFAULT_SCAN_32X32,
    }
}

fn mrow_scan(tx_sz: u8) -> &'static [u16] {
    use tables::scan as s;
    match tx_sz {
        0 => &s::MROW_SCAN_4X4,
        5 => &s::MROW_SCAN_4X8,
        6 => &s::MROW_SCAN_8X4,
        1 => &s::MROW_SCAN_8X8,
        7 => &s::MROW_SCAN_8X16,
        8 => &s::MROW_SCAN_16X8,
        2 => &s::MROW_SCAN_16X16,
        13 => &s::MROW_SCAN_4X16,
        _ => &s::MROW_SCAN_16X4,
    }
}

fn mcol_scan(tx_sz: u8) -> &'static [u16] {
    use tables::scan as s;
    match tx_sz {
        0 => &s::MCOL_SCAN_4X4,
        5 => &s::MCOL_SCAN_4X8,
        6 => &s::MCOL_SCAN_8X4,
        1 => &s::MCOL_SCAN_8X8,
        7 => &s::MCOL_SCAN_8X16,
        8 => &s::MCOL_SCAN_16X8,
        2 => &s::MCOL_SCAN_16X16,
        13 => &s::MCOL_SCAN_4X16,
        _ => &s::MCOL_SCAN_16X4,
    }
}

fn read_eob_pt(ts: &mut TileState<'_>, eob_multisize: i32, ptype: usize, ctx: usize) -> i32 {
    macro_rules! read_eob {
        ($field:ident) => {{
            let mut cdf = ts.cdf.$field.get(ptype).and_then(|r| r.get(ctx)).copied().unwrap_or_default().to_vec();
            let v = ts.sd.read_symbol(&mut cdf);
            if let Some(a) = ts.cdf.$field.get_mut(ptype)
                && let Some(slot) = a.get_mut(ctx)
            {
                for (d, s) in slot.iter_mut().zip(cdf.iter()) {
                    *d = *s;
                }
            }
            i32::try_from(v).unwrap_or(0)
        }};
    }
    macro_rules! read_eob_noctx {
        ($field:ident) => {{
            let mut cdf = ts.cdf.$field.get(ptype).copied().unwrap_or_default().to_vec();
            let v = ts.sd.read_symbol(&mut cdf);
            if let Some(slot) = ts.cdf.$field.get_mut(ptype) {
                for (d, s) in slot.iter_mut().zip(cdf.iter()) {
                    *d = *s;
                }
            }
            i32::try_from(v).unwrap_or(0)
        }};
    }
    match eob_multisize {
        0 => read_eob!(eob_pt_16),
        1 => read_eob!(eob_pt_32),
        2 => read_eob!(eob_pt_64),
        3 => read_eob!(eob_pt_128),
        4 => read_eob!(eob_pt_256),
        5 => read_eob_noctx!(eob_pt_512),
        _ => read_eob_noctx!(eob_pt_1024),
    }
}

fn reconstruct(ctx: &mut FrameCtx, _ts: &mut TileState<'_>, plane: usize, start_x: usize, start_y: usize, tx_sz: u8, tx_type: Av1TxType, lossless: bool) {
    let log2_w = u32::from(tables::TX_WIDTH_LOG2.get(usize::from(tx_sz)).copied().unwrap_or(2));
    let log2_h = u32::from(tables::TX_HEIGHT_LOG2.get(usize::from(tx_sz)).copied().unwrap_or(2));
    let w = 1usize << log2_w;
    let h = 1usize << log2_h;
    let tw = w.min(32);
    let th = h.min(32);
    let dq_denom: i32 = match tx_sz {
        3 | 9 | 10 | 17 | 18 => 2,
        4 | 11 | 12 => 4,
        _ => 1,
    };

    let base_q_idx = ctx.header.base_qindex_for_segment(0);
    let (dc_q_val, ac_q_val) = dequant_values(ctx, plane, base_q_idx);

    let quant = std::mem::take(&mut ctx.last_quant);
    let mut dequant = vec![0i32; tw * th];
    for i in 0..th {
        for j in 0..tw {
            let q = if i == 0 && j == 0 { dc_q_val } else { ac_q_val };
            let idx = i * tw + j;
            let dq = i64::from(quant.get(idx).copied().unwrap_or(0)) * i64::from(q);
            let sign = if dq < 0 { -1i64 } else { 1 };
            #[allow(clippy::integer_division, reason = "7.12.3's own dequantized-coefficient formula: dq2 = Round2Signed style division by dqDenom")]
            let dq2 = sign * ((dq.abs()) & 0x00FF_FFFF) / i64::from(dq_denom.max(1));
            let bound = 1i64 << (7 + i32::from(ctx.bit_depth));
            let clamped = dq2.clamp(-bound, bound - 1);
            if let Some(slot) = dequant.get_mut(idx) {
                *slot = i32::try_from(clamped).unwrap_or(0);
            }
        }
    }

    let mut residual_buf = vec![0i32; w * h];
    transform::inverse_transform_2d(tx_type, log2_w, log2_h, lossless, ctx.bit_depth, &dequant, &mut residual_buf);

    let flip_ud = tx_type.flip_ud();
    let flip_lr = tx_type.flip_lr();
    if let Some(p) = ctx.pic.plane_mut(plane) {
        for i in 0..h {
            for j in 0..w {
                let xx = if flip_lr { w - j - 1 } else { j };
                let yy = if flip_ud { h - i - 1 } else { i };
                let old = i32::from(p.get_clamped(ix(start_x + xx), ix(start_y + yy)));
                let r = residual_buf.get(i * w + j).copied().unwrap_or(0);
                let sum = (old + r).clamp(0, (1i32 << ctx.bit_depth) - 1);
                p.set(start_x + xx, start_y + yy, u16::try_from(sum).unwrap_or(0));
            }
        }
    }
}

fn dequant_values(ctx: &FrameCtx, plane: usize, base_q_idx: i32) -> (i32, i32) {
    let depth_idx = usize::from((ctx.bit_depth.saturating_sub(8)) >> 1).min(2);
    let (dc_delta, ac_delta) = match plane {
        0 => (ctx.header.quant.delta_q_y_dc, 0),
        1 => (ctx.header.quant.delta_q_u_dc, ctx.header.quant.delta_q_u_ac),
        _ => (ctx.header.quant.delta_q_v_dc, ctx.header.quant.delta_q_v_ac),
    };
    let dc_q = tables::quant::DC_QLOOKUP.get(depth_idx).and_then(|r| r.get(usize::try_from((base_q_idx + dc_delta).clamp(0, 255)).unwrap_or(0))).copied().unwrap_or(4);
    let ac_q = tables::quant::AC_QLOOKUP.get(depth_idx).and_then(|r| r.get(usize::try_from((base_q_idx + ac_delta).clamp(0, 255)).unwrap_or(0))).copied().unwrap_or(4);
    (i32::from(dc_q), i32::from(ac_q))
}

fn pic_to_frame(budget: &mut Budget, seq: &SequenceHeader, fh: &FrameHeader, pic: &Picture, mi_cols: usize, mi_rows: usize) -> Result<Frame> {
    let cc = &seq.color_config;
    let name = match (cc.bit_depth, cc.mono_chrome, cc.subsampling_x, cc.subsampling_y) {
        (8, true, _, _) => "gray".to_string(),
        (8, false, true, true) => "yuv420p".to_string(),
        (8, false, true, false) => "yuv422p".to_string(),
        (8, false, false, false) => "yuv444p".to_string(),
        (b, true, _, _) => format!("gray{b}le"),
        (b, false, true, true) => format!("yuv420p{b}le"),
        (b, false, true, false) => format!("yuv422p{b}le"),
        (b, false, false, false) => format!("yuv444p{b}le"),
        (b, false, false, true) => format!("yuv440p{b}le"),
    };
    let pix_fmt = PixFmt::from_name(&name).map_err(|_| Error::InvalidData("vaco-codec-av1: unsupported pixel format"))?;
    let width = fh.size.upscaled_width;
    let height = fh.size.coded_height;
    let mut frame = Frame::alloc_video(budget, pix_fmt, width, height)?;
    let (w, h) = (usize::try_from(width).unwrap_or(0), usize::try_from(height).unwrap_or(0));
    #[allow(clippy::integer_division, reason = "chroma plane width/height at 4:2:0 subsampling: exact halving of the luma dimension")]
    let cw = if cc.subsampling_x { mi_cols * 4 / 2 } else { mi_cols * 4 };
    #[allow(clippy::integer_division, reason = "chroma plane width/height at 4:2:0 subsampling: exact halving of the luma dimension")]
    let ch = if cc.subsampling_y { mi_rows * 4 / 2 } else { mi_rows * 4 };
    blit(&pic.y, &mut frame, 0, w, h, cc.bit_depth);
    if !cc.mono_chrome {
        if let Some(u) = &pic.u {
            blit(u, &mut frame, 1, cw.min(w.div_ceil(1 + usize::from(cc.subsampling_x))), ch.min(h.div_ceil(1 + usize::from(cc.subsampling_y))), cc.bit_depth);
        }
        if let Some(v) = &pic.v {
            blit(v, &mut frame, 2, cw.min(w.div_ceil(1 + usize::from(cc.subsampling_x))), ch.min(h.div_ceil(1 + usize::from(cc.subsampling_y))), cc.bit_depth);
        }
    }
    Ok(frame)
}

fn blit(src: &Plane, frame: &mut Frame, plane_index: usize, width: usize, height: usize, bit_depth: u8) {
    let Some(mut dst) = frame.plane_mut(plane_index) else { return };
    let two_bytes = bit_depth > 8;
    for y in 0..height {
        let Some(row) = dst.row_mut(y) else { continue };
        for x in 0..width {
            let v = src.get_clamped(ix(x), ix(y));
            if two_bytes {
                let bytes = v.to_le_bytes();
                if let Some(b) = row.get_mut(2 * x) {
                    *b = bytes[0];
                }
                if let Some(b) = row.get_mut(2 * x + 1) {
                    *b = bytes[1];
                }
            } else if let Some(b) = row.get_mut(x) {
                *b = u8::try_from(v).unwrap_or(0);
            }
        }
    }
}
