//! Assembles [`crate::header`], [`crate::tables`], and [`crate::transform`]
//! into a [`vaco_codec_core::Decoder`]: I-picture header, then every
//! macroblock's `CBPCY`/`ACPRED`, then every block's DC/AC decode, dequant,
//! inverse transform, and reconstruction (SS8.1).

use std::collections::VecDeque;

use vaco_bitstream::BitReader;
use vaco_codec_core::Decoder;
use vaco_codec_vlc::VlcTable;
use vaco_core::{Error, MediaType, Result};
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;
use vaco_pixfmt::PixFmt;

use crate::header::{self, PictureHeader, SequenceInfo};
use crate::tables::{self, AcCodingSet};
use crate::transform::inverse_transform_8x8;

/// Per-block neighbour state, one entry per 8x8 block, in a flat
/// `row * cols + col` grid — the DC/AC-prediction context SS8.1.3.2/8.1.3.7
/// need. Luma has its own `2 * mb_h x 2 * mb_w` grid; Cb and Cr each have
/// their own `mb_h x mb_w` grid (4:2:0: one chroma block per component per
/// macroblock).
struct BlockGrid {
    cols: usize,
    dc: Vec<i32>,
    /// First AC row (7 coefficients, columns 1..8 of row 0), for `TOP`
    /// prediction of a block below.
    ac_row: Vec<[i32; 7]>,
    /// First AC column (7 coefficients, rows 1..8 of column 0), for `LEFT`
    /// prediction of a block to the right.
    ac_col: Vec<[i32; 7]>,
}

impl BlockGrid {
    fn new(budget: &mut Budget, rows: usize, cols: usize) -> Result<Self> {
        let n = rows.saturating_mul(cols);
        Ok(Self {
            cols,
            dc: budget.alloc(n)?,
            ac_row: budget.alloc(n)?,
            ac_col: budget.alloc(n)?,
        })
    }

    fn idx(&self, row: usize, col: usize) -> usize {
        row.saturating_mul(self.cols).saturating_add(col)
    }

    fn dc_at(&self, row: usize, col: usize) -> i32 {
        self.dc.get(self.idx(row, col)).copied().unwrap_or(0)
    }

    fn set(&mut self, row: usize, col: usize, dc: i32, ac_row: [i32; 7], ac_col: [i32; 7]) {
        let i = self.idx(row, col);
        if let Some(slot) = self.dc.get_mut(i) {
            *slot = dc;
        }
        if let Some(slot) = self.ac_row.get_mut(i) {
            *slot = ac_row;
        }
        if let Some(slot) = self.ac_col.get_mut(i) {
            *slot = ac_col;
        }
    }
}

/// Left/top/top-left prediction direction (SS8.1.3.2 Figure 39): which
/// neighbour's DC (and, if `ACPRED`, AC row/column) a block predicts from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PredDir {
    Left,
    Top,
}

/// SS8.1.3.3: `DCStepSize` from `MQUANT`.
#[allow(clippy::integer_division, reason = "MQUANT/2 is the spec's own integer-truncating formula, not a shortcut around a real division")]
fn dc_step_size(mquant: u32) -> i32 {
    match mquant {
        1 | 2 => 2 * i32::try_from(mquant).unwrap_or(2),
        3 | 4 => 8,
        _ => {
            let half = mquant / 2;
            i32::try_from(half).unwrap_or(0) + 6
        }
    }
}

/// SS8.1.3.2 Figure 39: DC predictor and direction, Simple/Main I/BI.
/// `default_predictor` is used for any of A/B/C that is out of frame
/// bounds (there is no slice boundary in this crate's single-slice scope).
fn dc_predictor(default_predictor: i32, a: Option<i32>, b: Option<i32>, c: Option<i32>) -> (PredDir, i32) {
    let a = a.unwrap_or(default_predictor);
    let b = b.unwrap_or(default_predictor);
    let c = c.unwrap_or(default_predictor);
    if (b - a).abs() <= (b - c).abs() {
        (PredDir::Left, c)
    } else {
        (PredDir::Top, a)
    }
}

/// SS8.1.3.1 Figure 37: DC differential magnitude + sign decode.
fn decode_dc_differential(r: &mut BitReader<'_>, table: &[vaco_codec_vlc::VlcEntry], mquant: u32) -> Result<i32> {
    let vlc = VlcTable::new(table);
    let mag = vlc.decode(r).ok_or(Error::InvalidData("vc1: DC VLC decode failed"))?;
    let mut differential = if mag == tables::ESCAPE_DC {
        let bits = match mquant {
            1 => 10,
            2 => 9,
            _ => 8,
        };
        i32::try_from(r.get(bits)).unwrap_or(0)
    } else {
        let mut d = i32::try_from(mag).unwrap_or(0);
        if mag != 0 {
            if mquant == 1 {
                d = d * 4 + i32::try_from(r.get(2)).unwrap_or(0) - 3;
            } else if mquant == 2 {
                d = d * 2 + i32::try_from(r.get(1)).unwrap_or(0) - 1;
            }
        }
        d
    };
    if differential != 0 && r.get_bit() != 0 {
        differential = -differential;
    }
    Ok(differential)
}

/// One decoded (run, level, `last_flag`) AC coefficient, SS8.1.3.4 Figure 41.
struct AcSymbol {
    run: u32,
    level: i32,
    last: bool,
}

#[allow(clippy::too_many_lines, reason = "one entropy-decode state machine (VLC index / escape mode 1/2/3), splitting it would just move the shared mutable escape-mode-3 state across a function boundary")]
fn decode_ac_symbol(
    r: &mut BitReader<'_>,
    set: &AcCodingSet,
    first_mode3: &mut bool,
    mode3_level_bits: &mut u32,
    mode3_run_bits: &mut u32,
    pquant: u32,
) -> Result<AcSymbol> {
    let vlc = VlcTable::new(set.code);
    let index = vlc.decode(r).ok_or(Error::InvalidData("vc1: AC VLC decode failed"))?;
    if index != set.escape_index {
        let idx = index as usize;
        let &(run, level) = set.run_level.get(idx).ok_or(Error::InvalidData("vc1: AC index out of range"))?;
        let last = idx >= set.start_index_of_last;
        let sign = r.get_bit();
        let level = if sign == 1 { -i32::from(level) } else { i32::from(level) };
        return Ok(AcSymbol { run: u32::from(run), level, last });
    }

    let escmode = VlcTable::new(&tables::ESCMODE).decode(r).ok_or(Error::InvalidData("vc1: ESCMODE decode failed"))?;
    match escmode {
        1 => {
            let idx2 = vlc.decode(r).ok_or(Error::InvalidData("vc1: ACCOEF2 decode failed"))?;
            let idx = idx2 as usize;
            let &(run, level) = set.run_level.get(idx).ok_or(Error::InvalidData("vc1: AC index out of range"))?;
            let last = idx >= set.start_index_of_last;
            let delta = if last {
                set.last_delta_level_by_run.get(run as usize).copied().unwrap_or(0)
            } else {
                set.not_last_delta_level_by_run.get(run as usize).copied().unwrap_or(0)
            };
            let mut level = i32::from(level) + i32::from(delta);
            if r.get_bit() == 1 {
                level = -level;
            }
            Ok(AcSymbol { run: u32::from(run), level, last })
        }
        2 => {
            let idx2 = vlc.decode(r).ok_or(Error::InvalidData("vc1: ACCOEF2 decode failed"))?;
            let idx = idx2 as usize;
            let &(run, level) = set.run_level.get(idx).ok_or(Error::InvalidData("vc1: AC index out of range"))?;
            let last = idx >= set.start_index_of_last;
            let level_idx = (level.saturating_sub(1)) as usize;
            let delta = if last {
                set.last_delta_run_by_level.get(level_idx).copied().unwrap_or(0)
            } else {
                set.not_last_delta_run_by_level.get(level_idx).copied().unwrap_or(0)
            };
            let run = u32::from(run) + u32::from(delta) + 1;
            let mut level = i32::from(level);
            if r.get_bit() == 1 {
                level = -level;
            }
            Ok(AcSymbol { run, level, last })
        }
        _ => {
            // Mode 3: fixed-length escape (SS7.1.4.9-7.1.4.14). `ESCLVLSZ`
            // and `ESCRUNSZ` are read only the first time Mode 3 is used in
            // the picture; every later Mode 3 escape reuses both sizes.
            let last = r.get_bit() == 1;
            if *first_mode3 {
                *first_mode3 = false;
                let table = if (1..=7).contains(&pquant) {
                    &tables::ESCLVLSZ_CONSERVATIVE[..]
                } else {
                    &tables::ESCLVLSZ_EFFICIENT[..]
                };
                *mode3_level_bits = VlcTable::new(table)
                    .decode(r)
                    .ok_or(Error::InvalidData("vc1: ESCLVLSZ decode failed"))?;
                let escrunsz = r.get(2);
                *mode3_run_bits = tables::escrunsz_to_run_bits(escrunsz);
            }
            let run = r.get(*mode3_run_bits);
            let sign = r.get_bit();
            let level = i32::try_from(r.get(*mode3_level_bits)).unwrap_or(0);
            let level = if sign == 1 { -level } else { level };
            Ok(AcSymbol { run, level, last })
        }
    }
}

/// SS8.1.3.5 Figure 42: run-level decode into a 64-element coefficient
/// array (index 0 reserved for the DC coefficient, filled by the caller).
fn decode_ac_run_level(
    r: &mut BitReader<'_>,
    set: &AcCodingSet,
    first_mode3: &mut bool,
    mode3_level_bits: &mut u32,
    mode3_run_bits: &mut u32,
    pquant: u32,
) -> Result<[i32; 64]> {
    let mut array = [0i32; 64];
    let mut pos = 1usize;
    loop {
        let sym = decode_ac_symbol(r, set, first_mode3, mode3_level_bits, mode3_run_bits, pquant)?;
        pos = pos.saturating_add(sym.run as usize);
        if let Some(slot) = array.get_mut(pos) {
            *slot = sym.level;
        }
        pos = pos.saturating_add(1);
        if sym.last || pos >= 64 {
            break;
        }
    }
    Ok(array)
}

/// SS8.1.3.6 Table 73 + SS11.9.1: scan-array selection and inverse zigzag
/// scan into an 8x8 row-major grid.
fn inverse_zigzag(array: &[i32; 64], acpred: bool, dir: PredDir) -> [i32; 64] {
    let scan: &[usize; 64] = if !acpred {
        &tables::NORMAL_SCAN
    } else if dir == PredDir::Top {
        &tables::HORIZONTAL_SCAN
    } else {
        &tables::VERTICAL_SCAN
    };
    let mut out = [0i32; 64];
    for (i, &pos) in scan.iter().enumerate() {
        let Some(&v) = array.get(i) else { continue };
        if let Some(slot) = out.get_mut(pos) {
            *slot = v;
        }
    }
    out
}

/// SS8.1.3.8: AC coefficient dequantization. `double_quant = 2*MQUANT +
/// HALFQP` always in this crate's scope (blocks are always coded with
/// `PQUANT`, never `VOPDQUANT`; see this crate's top-level doc).
fn dequant_ac(coeff: i32, double_quant: i32, uniform: bool, mquant: u32) -> i32 {
    if coeff == 0 {
        return 0;
    }
    if uniform {
        coeff * double_quant
    } else {
        let quant_scale = i32::try_from(mquant).unwrap_or(0);
        coeff * double_quant + coeff.signum() * quant_scale
    }
}

fn clip_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

struct FrameCtx<'a> {
    ph: &'a PictureHeader,
    seq: &'a SequenceInfo,
    mquant: u32,
    double_quant: i32,
}

/// Decode one 8x8 intra block: DC differential, DC predictor/dequant, `CBP`-
/// gated AC decode, AC predictor/dequant, inverse zigzag, inverse transform,
/// and reconstruction into `plane` at `(base_x, base_y)`. Returns the
/// dequantized DC value and first AC row/column, for use as a later
/// neighbour's predictor.
#[allow(clippy::too_many_arguments, reason = "one block-reconstruction call site; the arguments are the whole of a block's neighbour/coding context")]
fn decode_intra_block(
    r: &mut BitReader<'_>,
    ctx: &FrameCtx<'_>,
    ac_set: &AcCodingSet,
    dc_table: &[vaco_codec_vlc::VlcEntry],
    is_luma: bool,
    coded: bool,
    acpred: bool,
    grid: &BlockGrid,
    row: usize,
    col: usize,
    first_mode3: &mut bool,
    mode3_level_bits: &mut u32,
    mode3_run_bits: &mut u32,
    plane: &mut [u8],
    stride: usize,
    plane_w: usize,
    plane_h: usize,
    base_x: usize,
    base_y: usize,
) -> Result<(i32, [i32; 7], [i32; 7])> {
    // SS8.1.3.2's own formula: `(1024 + DCStepSize>>1) / DCStepSize` --
    // integer-truncating division is the specified rounding, not an
    // approximation of a real one.
    #[allow(clippy::integer_division, reason = "the spec's own default_predictor formula truncates, not approximates")]
    let default_predictor = if ctx.seq.overlap && ctx.ph.pquant >= 9 {
        0
    } else {
        let step = dc_step_size(ctx.mquant);
        if step == 0 { 0 } else { (1024 + step / 2) / step }
    };
    let pred_a = row.checked_sub(1).map(|r0| grid.dc_at(r0, col));
    let pred_b = if row > 0 && col > 0 { Some(grid.dc_at(row - 1, col - 1)) } else { None };
    let pred_c = col.checked_sub(1).map(|c0| grid.dc_at(row, c0));
    let (dir, dc_pred) = dc_predictor(default_predictor, pred_a, pred_b, pred_c);

    let differential = decode_dc_differential(r, dc_table, ctx.mquant)?;
    let dc_q = dc_pred + differential;
    let dc_step = dc_step_size(ctx.mquant);
    let dc_coeff = dc_q * dc_step;

    let mut coeffs = [0i32; 64];
    if let Some(slot) = coeffs.first_mut() {
        *slot = dc_coeff;
    }
    if coded {
        let raw = decode_ac_run_level(r, ac_set, first_mode3, mode3_level_bits, mode3_run_bits, ctx.ph.pquant)?;
        for i in 1..64usize {
            let Some(&v) = raw.get(i) else { continue };
            let dequant = dequant_ac(v, ctx.double_quant, ctx.ph.uniform_quantizer, ctx.mquant);
            if let Some(slot) = coeffs.get_mut(i) {
                *slot = dequant;
            }
        }
    }

    let grid_coeffs = inverse_zigzag(&coeffs, acpred, dir);
    let mut grid_coeffs = grid_coeffs;
    if acpred {
        match dir {
            PredDir::Top => {
                if let Some(top_row) = row.checked_sub(1).map(|r0| grid.ac_row.get(grid.idx(r0, col)).copied().unwrap_or([0; 7])) {
                    for (k, &pred) in top_row.iter().enumerate() {
                        if let Some(slot) = grid_coeffs.get_mut(k + 1) {
                            *slot += pred;
                        }
                    }
                }
            }
            PredDir::Left => {
                if let Some(left_col) = col.checked_sub(1).map(|c0| grid.ac_col.get(grid.idx(row, c0)).copied().unwrap_or([0; 7])) {
                    for (k, &pred) in left_col.iter().enumerate() {
                        if let Some(slot) = grid_coeffs.get_mut((k + 1) * 8) {
                            *slot += pred;
                        }
                    }
                }
            }
        }
    }

    let mut own_row = [0i32; 7];
    let mut own_col = [0i32; 7];
    for k in 0..7usize {
        if let Some(slot) = own_row.get_mut(k) {
            *slot = grid_coeffs.get(k + 1).copied().unwrap_or(0);
        }
        if let Some(slot) = own_col.get_mut(k) {
            *slot = grid_coeffs.get((k + 1) * 8).copied().unwrap_or(0);
        }
    }

    let residual = inverse_transform_8x8(&grid_coeffs);
    let add_offset = ctx.seq.overlap;
    for y in 0..8usize {
        for x in 0..8usize {
            let px = base_x + x;
            let py = base_y + y;
            if px >= plane_w || py >= plane_h {
                continue;
            }
            let Some(&v) = residual.get(y * 8 + x) else { continue };
            let sample = if add_offset { clip_u8(v + 128) } else { clip_u8(v) };
            let off = py.saturating_mul(stride).saturating_add(px);
            if let Some(dst) = plane.get_mut(off) {
                *dst = sample;
            }
        }
    }
    let _ = is_luma;
    Ok((dc_q, own_row, own_col))
}

#[derive(Debug)]
pub struct Vc1Decoder {
    limits: Limits,
    seq: Option<SequenceInfo>,
    pending: VecDeque<Frame>,
}

impl Vc1Decoder {
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self { limits, seq: None, pending: VecDeque::new() }
    }

    fn decode_frame(&mut self, payload: &[u8]) -> Result<Frame> {
        let seq = self.seq.ok_or(Error::InvalidData("vc1: set_extradata must be called before decoding"))?;
        if seq.overlap {
            return Err(Error::Unsupported("vc1: OVERLAP == 1 (overlap smoothing) is not implemented"));
        }
        if seq.loopfilter {
            return Err(Error::Unsupported("vc1: LOOPFILTER == 1 (in-loop deblocking) is not implemented"));
        }
        if payload.len() <= 1 {
            return Err(Error::Unsupported("vc1: skipped P frame (coded size <= 1 byte) with no preceding I frame"));
        }

        let mut r = BitReader::new(payload);
        let ph = header::parse_i_picture_header(&mut r, &seq)?;

        let (ac_y, ac_c) = match (ph.pqindex <= 8, ph.transacfrm2, ph.transacfrm) {
            (true, 0, 0) => (&tables::HIGH_RATE_INTRA, &tables::HIGH_RATE_INTER),
            _ => {
                return Err(Error::Unsupported(
                    "vc1: only the High Rate Intra/Inter AC coding sets (PQINDEX <= 8, TRANSACFRM == TRANSACFRM2 == 0) are transcribed",
                ));
            }
        };
        let dc_luma_table: &[vaco_codec_vlc::VlcEntry] =
            if ph.transdctab { &tables::DC_HIGH_LUMA } else { &tables::DC_LOW_LUMA };
        let dc_chroma_table: &[vaco_codec_vlc::VlcEntry] =
            if ph.transdctab { &tables::DC_HIGH_CHROMA } else { &tables::DC_LOW_CHROMA };

        let mquant = ph.pquant;
        let double_quant = 2 * i32::try_from(mquant).unwrap_or(0) + i32::from(ph.halfqp);
        let ctx = FrameCtx { ph: &ph, seq: &seq, mquant, double_quant };

        let mut budget = Budget::new(self.limits.clone());
        budget.check_frame(seq.width, seq.height, 3)?;
        let mut frame = Frame::alloc_video(&mut budget, PixFmt::Yuv420p, seq.width, seq.height)?;
        let FrameData::Video { planes, .. } = &mut frame.data else {
            return Err(Error::InvalidData("vc1: allocated frame has no planes"));
        };

        let mb_w = seq.width.div_ceil(16) as usize;
        let mb_h = seq.height.div_ceil(16) as usize;
        let mut luma_grid = BlockGrid::new(&mut budget, mb_h.saturating_mul(2), mb_w.saturating_mul(2))?;
        let mut cb_grid = BlockGrid::new(&mut budget, mb_h, mb_w)?;
        let mut cr_grid = BlockGrid::new(&mut budget, mb_h, mb_w)?;
        let mut luma_coded: Vec<bool> = budget.alloc(mb_h.saturating_mul(2).saturating_mul(mb_w.saturating_mul(2)))?;
        let luma_coded_cols = mb_w.saturating_mul(2);

        let (luma_stride, luma_w, luma_h) = {
            let p = planes.first().ok_or(Error::InvalidData("vc1: frame has no luma plane"))?;
            (p.stride, seq.width as usize, seq.height as usize)
        };
        let (chroma_stride, chroma_w, chroma_h) = {
            let p = planes.get(1).ok_or(Error::InvalidData("vc1: frame has no chroma plane"))?;
            (p.stride, (seq.width as usize).div_ceil(2), (seq.height as usize).div_ceil(2))
        };

        let mut first_mode3 = true;
        let mut mode3_level_bits = 0u32;
        let mut mode3_run_bits = 0u32;

        for mb_row in 0..mb_h {
            for mb_col in 0..mb_w {
                let coded_idx = |br: usize, bc: usize| br.saturating_mul(luma_coded_cols).saturating_add(bc);
                let get_coded = |br: Option<usize>, bc: Option<usize>| -> bool {
                    match (br, bc) {
                        (Some(br), Some(bc)) => luma_coded.get(coded_idx(br, bc)).copied().unwrap_or(false),
                        _ => false,
                    }
                };
                let by0 = mb_row.saturating_mul(2);
                let bx0 = mb_col.saturating_mul(2);
                let t2 = get_coded(by0.checked_sub(1), Some(bx0));
                let t3 = get_coded(by0.checked_sub(1), Some(bx0.saturating_add(1)));
                let l1 = get_coded(Some(by0), bx0.checked_sub(1));
                let l3 = get_coded(Some(by0.saturating_add(1)), bx0.checked_sub(1));
                let lt3 = get_coded(by0.checked_sub(1), bx0.checked_sub(1));

                let decoded_cbpcy = VlcTable::new(&tables::CBPCY_I).decode(&mut r).ok_or(Error::InvalidData("vc1: CBPCY VLC decode failed"))?;

                let pred_y0 = if lt3 == t2 { l1 } else { t2 };
                let pred_y0 = pred_y0 ^ (((decoded_cbpcy >> 5) & 1) != 0);
                let pred_y1 = if t2 == t3 { pred_y0 } else { t3 };
                let pred_y1 = pred_y1 ^ (((decoded_cbpcy >> 4) & 1) != 0);
                let pred_y2 = if l1 == pred_y0 { l3 } else { pred_y0 };
                let pred_y2 = pred_y2 ^ (((decoded_cbpcy >> 3) & 1) != 0);
                let pred_y3 = if pred_y0 == pred_y1 { pred_y2 } else { pred_y1 };
                let pred_y3 = pred_y3 ^ (((decoded_cbpcy >> 2) & 1) != 0);
                let cb_coded = (decoded_cbpcy & 0b10) != 0;
                let cr_coded = (decoded_cbpcy & 0b01) != 0;

                let acpred = r.get_bit() != 0;

                let luma_positions = [
                    (by0, bx0, pred_y0),
                    (by0, bx0 + 1, pred_y1),
                    (by0 + 1, bx0, pred_y2),
                    (by0 + 1, bx0 + 1, pred_y3),
                ];
                for &(br, bc, coded) in &luma_positions {
                    let base_x = bc.saturating_mul(8);
                    let base_y = br.saturating_mul(8);
                    let plane = planes.first_mut().ok_or(Error::InvalidData("vc1: frame has no luma plane"))?;
                    let (dc, ac_row, ac_col) = decode_intra_block(
                        &mut r, &ctx, ac_y, dc_luma_table, true, coded, acpred, &luma_grid, br, bc,
                        &mut first_mode3, &mut mode3_level_bits, &mut mode3_run_bits,
                        plane.data.make_mut(), luma_stride, luma_w, luma_h, base_x, base_y,
                    )?;
                    luma_grid.set(br, bc, dc, ac_row, ac_col);
                    if let Some(slot) = luma_coded.get_mut(coded_idx(br, bc)) {
                        *slot = coded;
                    }
                }

                for (plane_idx, coded, grid) in [(1usize, cb_coded, &mut cb_grid), (2usize, cr_coded, &mut cr_grid)] {
                    let base_x = mb_col.saturating_mul(8);
                    let base_y = mb_row.saturating_mul(8);
                    let plane = planes.get_mut(plane_idx).ok_or(Error::InvalidData("vc1: frame missing chroma plane"))?;
                    let (dc, ac_row, ac_col) = decode_intra_block(
                        &mut r, &ctx, ac_c, dc_chroma_table, false, coded, acpred, grid, mb_row, mb_col,
                        &mut first_mode3, &mut mode3_level_bits, &mut mode3_run_bits,
                        plane.data.make_mut(), chroma_stride, chroma_w, chroma_h, base_x, base_y,
                    )?;
                    grid.set(mb_row, mb_col, dc, ac_row, ac_col);
                }
            }
        }

        Ok(frame)
    }
}

impl Decoder for Vc1Decoder {
    fn send_packet(&mut self, packet: Option<&Packet>) -> Result<()> {
        let Some(packet) = packet else {
            return Ok(());
        };
        let mut frame = self.decode_frame(packet.payload())?;
        frame.pts = packet.pts;
        frame.duration = packet.duration;
        self.pending.push_back(frame);
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        self.pending.pop_front().ok_or(Error::NeedMoreInput)
    }

    fn flush(&mut self) {
        self.pending.clear();
    }

    fn set_extradata(&mut self, extradata: &[u8]) -> Result<()> {
        self.seq = Some(header::parse_extradata(extradata)?);
        Ok(())
    }
}

fn make(limits: Limits) -> Box<dyn Decoder> {
    Box::new(Vc1Decoder::new(limits))
}

/// The registry descriptor for VC-1 decode.
///
/// `caps: Caps::PATENT_ENCUMBERED` plus `vaco-component.toml`'s
/// `encumbered = true` / `default = false` pair (D4/D4.1) — VC-1 is
/// Microsoft/MPEG-LA-pool patent-encumbered and was, before this crate,
/// entirely absent from `planning/research/07-legal-patents-licensing.md`.
/// No ruling on this project's own exposure has been made; this mirrors
/// `vaco-codec-h264`'s gate exactly, pending one.
pub const DECODER_VC1: ::vaco_codec_core::DecoderDesc = ::vaco_codec_core::DecoderDesc {
    name: "vc1",
    long_name: "SMPTE VC-1 (Simple/Main profile, progressive I-frame only)",
    id: ::vaco_codec_core::CodecId::Vc1,
    media_type: MediaType::Video,
    caps: ::vaco_codec_core::Caps::PATENT_ENCUMBERED,
    supported_rates: &[],
    make,
};

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "a test that cannot set up is a failed test")]
mod tests {
    use super::*;

    #[test]
    fn descriptor_answers_to_its_own_name() {
        assert_eq!(DECODER_VC1.name, "vc1");
        assert_eq!(DECODER_VC1.id, ::vaco_codec_core::CodecId::Vc1);
    }

    #[test]
    fn decode_without_extradata_is_a_clean_error() {
        let mut dec = Vc1Decoder::new(Limits::permissive());
        let mut budget = Budget::new(Limits::permissive());
        let pkt = Packet::from_slice(&mut budget, &[0u8; 40]).unwrap();
        assert!(dec.send_packet(Some(&pkt)).is_err());
    }

    #[test]
    fn garbage_payload_after_extradata_is_a_clean_error_not_a_panic() {
        let mut dec = Vc1Decoder::new(Limits::permissive());
        let mut ed = [0u8; 12];
        ed[..4].copy_from_slice(&0x4100_0001u32.to_be_bytes());
        ed[4..8].copy_from_slice(&64u32.to_le_bytes());
        ed[8..12].copy_from_slice(&64u32.to_le_bytes());
        dec.set_extradata(&ed).unwrap();
        let mut budget = Budget::new(Limits::permissive());
        let pkt = Packet::from_slice(&mut budget, &[0xFFu8; 64]).unwrap();
        // Must not panic; a wrong-looking bitstream is a decode error, not
        // a crash -- this file's own CBPCY/AC decode paths all use
        // `VlcTable::decode`'s `None`-on-no-match behaviour precisely so
        // this holds even on data this crate never intended to accept.
        let _ = dec.send_packet(Some(&pkt));
    }
}
