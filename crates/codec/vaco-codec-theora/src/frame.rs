//! Per-frame decode (`Vaco-Spec-Ref: theora-spec-20170603 chapter 7`),
//! scoped to intra (keyframe) frames only — see the crate root doc for why.
//!
//! Every simplification below follows from that scope and is noted where it
//! departs from the general (inter-capable) procedure the spec describes:
//!
//! - Coded block flags (7.3): skipped. Every block is coded in an intra
//!   frame (7.3 step 1).
//! - Macro block coding modes (7.4) and motion vectors (7.5): skipped. Every
//!   macro block is `INTRA` (7.4 step 1), which needs no motion vector and
//!   uses no reference frame (2.5).
//! - DC prediction (7.8): only one reference-frame class ("None") exists, so
//!   `LASTDC` needs one slot per plane, not three, and every neighbor check
//!   that compares `MBMODES` reference-frame classes is always true.
//! - Reconstruction (7.9.4): the predictor is always the intra predictor
//!   (constant 128, section 7.9.1.1) and `qti` is always 0 (Intra). The
//!   "uncoded block" branch (7.9.4 step 2e) never executes and is not
//!   implemented.
//! - Loop filter (7.10.3): the right/top edge filter is skipped for every
//!   block, and this is exact, not an approximation — the spec only applies
//!   it when the neighbor on that side is *uncoded* (step vii/viii's
//!   `BCODED[bj]` check), which never happens here, so that neighbor's own
//!   left/bottom-edge pass always covers the shared edge instead.

use vaco_bitstream::BitReader;
use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::blocks::FrameGeom;
use crate::huffman::HuffTable;
use crate::idct::{idct_2d, trunc16};
use crate::ident::Ident;
use crate::quant::QuantParams;
use crate::setup::Setup;
use crate::tokens::{self, NATURAL_TO_ZIGZAG};

/// A block's 64-entry zig-zag-order coefficient buffer, wrapped so it can go
/// through [`Budget::alloc`] (which needs `Default`, and `[i32; 64]` itself
/// has no blanket `Default` impl in `core` past length 32).
#[derive(Debug, Clone, Copy)]
struct BlockCoeffs([i32; 64]);

impl Default for BlockCoeffs {
    fn default() -> Self {
        Self([0; 64])
    }
}

/// One reconstructed color plane, full coded-frame size (not yet cropped to
/// the picture region), row 0 at the top — the opposite of the spec's own
/// bottom-up coordinate convention, chosen to match every other decoder in
/// this tree so the caller never has to special-case Theora's output.
#[derive(Debug, Clone)]
pub(crate) struct PlaneBuf {
    pub width: u32,
    pub height: u32,
    data: Vec<u8>,
}

impl PlaneBuf {
    fn alloc(width: u32, height: u32, budget: &mut Budget) -> Result<Self> {
        let n = (width as usize).saturating_mul(height as usize);
        let data = budget.alloc(n)?;
        Ok(Self {
            width,
            height,
            data,
        })
    }

    #[inline]
    fn index(&self, x: u32, y: u32) -> Option<usize> {
        if x >= self.width || y >= self.height {
            return None;
        }
        Some((y as usize).saturating_mul(self.width as usize) + x as usize)
    }

    fn get(&self, x: u32, y: u32) -> u8 {
        self.index(x, y)
            .and_then(|i| self.data.get(i))
            .copied()
            .unwrap_or(0)
    }

    fn set(&mut self, x: u32, y: u32, v: u8) {
        if let Some(i) = self.index(x, y)
            && let Some(slot) = self.data.get_mut(i)
        {
            *slot = v;
        }
    }

    /// Set the pixel at spec (bottom-up) coordinates `(sx, sy)` within a
    /// plane whose spec-space height is `plane_h`.
    fn set_bottom_up(&mut self, sx: u32, sy: u32, plane_h: u32, v: u8) {
        let Some(y) = plane_h.checked_sub(1).and_then(|m| m.checked_sub(sy)) else {
            return;
        };
        self.set(sx, y, v);
    }

    fn get_bottom_up(&self, sx: u32, sy: u32, plane_h: u32) -> u8 {
        let Some(y) = plane_h.checked_sub(1).and_then(|m| m.checked_sub(sy)) else {
            return 0;
        };
        self.get(sx, y)
    }
}

/// A fully reconstructed, loop-filtered frame, still at full coded-frame
/// dimensions (crop to `Ident`'s picture region is the caller's job).
#[derive(Debug, Clone)]
pub(crate) struct DecodedFrame {
    pub planes: [PlaneBuf; 3],
}

/// Section 7.1: frame header. Only `FTYPE == 0` (intra) is decodable by this
/// crate; the caller turns a nonzero `FTYPE` into `Error::Unsupported`
/// before this is even called, since nothing past the header can be decoded
/// without a reference frame this crate never builds.
struct FrameHeader {
    qis: [u32; 3],
    nqis: u32,
}

fn parse_frame_header(r: &mut BitReader<'_>) -> Result<FrameHeader> {
    if r.get_bit() != 0 {
        return Err(Error::InvalidData("theora: not a data packet"));
    }
    let ftype = r.get_bit();
    if ftype != 0 {
        return Err(Error::Unsupported(
            "theora: inter (delta) frame decode is not supported; only keyframes are",
        ));
    }
    let mut qis = [0u32; 3];
    let mut nqis = 1u32;
    let q0 = r.get(6);
    if let Some(slot) = qis.first_mut() {
        *slot = q0;
    }
    if r.get_bit() != 0 {
        let q1 = r.get(6);
        if let Some(slot) = qis.get_mut(1) {
            *slot = q1;
        }
        nqis = 2;
        if r.get_bit() != 0 {
            let q2 = r.get(6);
            if let Some(slot) = qis.get_mut(2) {
                *slot = q2;
            }
            nqis = 3;
        }
    }
    let _reserved = r.get(3);
    r.check()
        .map_err(|_| Error::InvalidData("theora: truncated frame header"))?;
    Ok(FrameHeader { qis, nqis })
}

/// Section 7.2.1: the "unbounded" run-length bit string, used by block-level
/// qi decode (7.6) for streams with more than one qi value.
fn run_length_code(r: &mut BitReader<'_>) -> (u32, u32) {
    let mut ones = 0u32;
    while ones < 6 {
        if r.get_bit() == 0 {
            break;
        }
        ones += 1;
    }
    match ones {
        0 => (1, 0),
        1 => (2, 1),
        2 => (4, 1),
        3 => (6, 2),
        4 => (10, 3),
        5 => (18, 4),
        _ => (34, 12),
    }
}

fn decode_long_run_string(r: &mut BitReader<'_>, nbits: u32, budget: &mut Budget) -> Result<Vec<bool>> {
    let mut bits: Vec<bool> = budget.alloc(nbits as usize)?;
    if nbits == 0 {
        return Ok(bits);
    }
    let mut len = 0usize;
    let mut bit = r.get_bit() != 0;
    loop {
        let (rstart, rbits) = run_length_code(r);
        let roffs = r.get(rbits);
        let rlen = rstart.saturating_add(roffs);
        for _ in 0..rlen {
            if len >= bits.len() {
                break;
            }
            if let Some(slot) = bits.get_mut(len) {
                *slot = bit;
            }
            len += 1;
        }
        if len >= nbits as usize {
            break;
        }
        if rlen == 4129 {
            bit = r.get_bit() != 0;
        } else {
            bit = !bit;
        }
    }
    Ok(bits)
}

/// Section 7.6: block-level qi index decode. Every block is coded (intra
/// frame), so the "coded and still unassigned" condition is just "still
/// unassigned".
fn decode_block_qiis(
    r: &mut BitReader<'_>,
    nbs: u32,
    nqis: u32,
    budget: &mut Budget,
) -> Result<Vec<u8>> {
    let mut qiis: Vec<u8> = budget.alloc(nbs as usize)?;
    for qii in 0..nqis.saturating_sub(1) {
        let nbits = u32::try_from(qiis.iter().filter(|&&q| u32::from(q) == qii).count())
            .unwrap_or(u32::MAX);
        let bits = decode_long_run_string(r, nbits, budget)?;
        let mut it = bits.into_iter();
        for slot in &mut qiis {
            if u32::from(*slot) == qii && it.next() == Some(true) {
                *slot = slot.saturating_add(1);
            }
        }
    }
    Ok(qiis)
}

/// Section 7.42: which of the four "Huffman table groups" a token index
/// belongs to.
const fn huffman_group(ti: u32) -> u32 {
    match ti {
        0 => 0,
        1..=5 => 1,
        6..=14 => 2,
        15..=27 => 3,
        _ => 4,
    }
}

/// Section 7.7.3: decode every block's DCT coefficients (zig-zag order) and
/// coefficient counts.
fn decode_coefficients(
    r: &mut BitReader<'_>,
    geom: &FrameGeom,
    tables: &[HuffTable; 80],
    budget: &mut Budget,
) -> Result<(Vec<BlockCoeffs>, Vec<u8>)> {
    let nbs = geom.nbs;
    let mut coeffs: Vec<BlockCoeffs> = budget.alloc(nbs as usize)?;
    let mut ncoeffs: Vec<u8> = budget.alloc(nbs as usize)?;
    let mut tis: Vec<u32> = budget.alloc(nbs as usize)?;
    let mut remaining = nbs;
    let mut eobs = 0u32;
    let mut hti_l = 0u32;
    let mut hti_c = 0u32;

    for ti in 0..64u32 {
        if ti <= 1 {
            hti_l = r.get(4);
            hti_c = r.get(4);
        }
        for bi in 0..nbs {
            budget.consume_fuel(1)?;
            let cur_ti = tis.get(bi as usize).copied().unwrap_or(64);
            if cur_ti != ti {
                continue;
            }
            if let Some(slot) = ncoeffs.get_mut(bi as usize) {
                *slot = u8::try_from(ti).unwrap_or(64);
            }
            if eobs > 0 {
                if let Some(c) = coeffs.get_mut(bi as usize) {
                    for slot in c.0.iter_mut().skip(ti as usize) {
                        *slot = 0;
                    }
                }
                if let Some(slot) = tis.get_mut(bi as usize) {
                    *slot = 64;
                }
                remaining = remaining.saturating_sub(1);
                eobs -= 1;
                continue;
            }
            let hg = huffman_group(ti);
            let hti = if bi < geom.nlbs {
                16 * hg + hti_l
            } else {
                16 * hg + hti_c
            };
            let Some(table) = tables.get(hti as usize) else {
                return Err(Error::InvalidData("theora: huffman table index out of range"));
            };
            let token = table.decode(r);
            if token < 7 {
                let run = tokens::expand_eob_run(token, r, remaining);
                if let Some(c) = coeffs.get_mut(bi as usize) {
                    for slot in c.0.iter_mut().skip(ti as usize) {
                        *slot = 0;
                    }
                }
                if let Some(slot) = tis.get_mut(bi as usize) {
                    *slot = 64;
                }
                remaining = remaining.saturating_sub(1);
                eobs = run.saturating_sub(1);
            } else {
                let Some(c) = coeffs.get_mut(bi as usize) else {
                    continue;
                };
                let out = tokens::decode_coeff_token(token, r, ti, &mut c.0);
                if let Some(nc) = out.new_ncoeffs
                    && let Some(slot) = ncoeffs.get_mut(bi as usize)
                {
                    *slot = u8::try_from(nc.min(64)).unwrap_or(64);
                }
                let new_ti = out.new_ti.min(64);
                if let Some(slot) = tis.get_mut(bi as usize) {
                    *slot = new_ti;
                }
                if new_ti >= 64 {
                    remaining = remaining.saturating_sub(1);
                }
            }
        }
    }
    r.check()
        .map_err(|_| Error::InvalidData("theora: truncated DCT coefficient data"))?;
    Ok((coeffs, ncoeffs))
}

fn dc_of(coeffs: &[BlockCoeffs], bi: u32) -> i32 {
    coeffs
        .get(bi as usize)
        .and_then(|c| c.0.first())
        .copied()
        .unwrap_or(0)
}

/// Section 7.8.1, Table 7.47: weights and divisor for each set of available
/// DC predictors. `p` is `[left, lower-left, lower, lower-right]`.
#[allow(
    clippy::unnested_or_patterns,
    reason = "one flat `[bool; 4]` pattern per row of the spec's own Table 7.47 reads more directly than a nested pattern would"
)]
const fn dc_weights(p: [bool; 4]) -> (i32, i32, i32, i32, i32) {
    match p {
        [true, false, false, false] | [true, true, false, false] => (1, 0, 0, 0, 1),
        [false, true, false, false] => (0, 1, 0, 0, 1),
        [false, false, true, false] | [false, true, true, false] | [false, false, true, true] => {
            (0, 0, 1, 0, 1)
        }
        [true, false, true, false] => (1, 0, 1, 0, 2),
        [true, true, true, false] | [true, true, true, true] => (29, -26, 29, 0, 32),
        [false, false, false, true] => (0, 0, 0, 1, 1),
        [true, false, false, true] | [true, true, false, true] | [true, false, true, true] => {
            (75, 0, 0, 53, 128)
        }
        [false, true, false, true] => (0, 1, 0, 1, 2),
        [false, true, true, true] => (0, 3, 10, 3, 16),
        [false, false, false, false] => (0, 0, 0, 0, 1),
    }
}

/// Section 7.8: undo DC prediction, in place, over every plane's DC
/// coefficient (zig-zag index 0).
#[allow(
    clippy::integer_division,
    reason = "the DC predictor's own division must truncate towards zero (section 7.8.1's own text), which is exactly what `/` on signed integers does; a shift would round the wrong way for negative sums"
)]
fn undo_dc_prediction(coeffs: &mut [BlockCoeffs], geom: &FrameGeom) {
    for plane in &geom.planes {
        let mut lastdc = 0i32;
        for by in 0..plane.blocks_tall {
            for bx in 0..plane.blocks_wide {
                let Some(bi) = plane.coded_of(bx, by) else {
                    continue;
                };
                let left = (bx > 0).then(|| plane.coded_of(bx - 1, by)).flatten();
                let ll = (bx > 0 && by > 0)
                    .then(|| plane.coded_of(bx - 1, by - 1))
                    .flatten();
                let lower = (by > 0).then(|| plane.coded_of(bx, by - 1)).flatten();
                let lr = (by > 0)
                    .then(|| plane.coded_of(bx + 1, by - 1))
                    .flatten();

                let (p0, p1, p2, p3) = (left.is_some(), ll.is_some(), lower.is_some(), lr.is_some());
                let dcpred = if !p0 && !p1 && !p2 && !p3 {
                    lastdc
                } else {
                    let (w0, w1, w2, w3, pdiv) = dc_weights([p0, p1, p2, p3]);
                    let mut sum = 0i32;
                    if let Some(nb) = left {
                        sum = sum.wrapping_add(w0.wrapping_mul(dc_of(coeffs, nb)));
                    }
                    if let Some(nb) = ll {
                        sum = sum.wrapping_add(w1.wrapping_mul(dc_of(coeffs, nb)));
                    }
                    if let Some(nb) = lower {
                        sum = sum.wrapping_add(w2.wrapping_mul(dc_of(coeffs, nb)));
                    }
                    if let Some(nb) = lr {
                        sum = sum.wrapping_add(w3.wrapping_mul(dc_of(coeffs, nb)));
                    }
                    // Truncation towards zero (section 7.8.1's own text);
                    // Rust's `/` on signed integers already does this.
                    let mut pred = sum / pdiv.max(1);
                    if p0 && p1 && p2 {
                        // Outranging check, step 12h: only when left,
                        // lower-left, and lower are all available.
                        let (lv, llv, lowv) = (
                            left.map_or(0, |n| dc_of(coeffs, n)),
                            ll.map_or(0, |n| dc_of(coeffs, n)),
                            lower.map_or(0, |n| dc_of(coeffs, n)),
                        );
                        if (pred - lowv).abs() > 128 {
                            pred = lowv;
                        } else if (pred - lv).abs() > 128 {
                            pred = lv;
                        } else if (pred - llv).abs() > 128 {
                            pred = llv;
                        }
                    }
                    pred
                };
                let dc = trunc16(dc_of(coeffs, bi).wrapping_add(dcpred));
                if let Some(c) = coeffs.get_mut(bi as usize).and_then(|c| c.0.first_mut()) {
                    *c = dc;
                }
                lastdc = dc;
            }
        }
    }
}

/// Section 7.9.4: reconstruct one block's residual (post-IDCT, pre-clamp)
/// into an 8x8 array, dispatching to the DC-only fast path (step vii) or the
/// full dequantize + IDCT path (step viii) as `ncoeffs` dictates.
fn reconstruct_residual(
    quant: &QuantParams,
    qis: &[u32; 3],
    qii: u8,
    coeffs_bi: &[i32; 64],
    ncoeffs_bi: u8,
    pli: usize,
) -> [[i32; 8]; 8] {
    let qi0 = qis.first().copied().unwrap_or(0);
    if ncoeffs_bi < 2 {
        let dc_qmat = quant.matrix(0, pli, qi0);
        let qmat0 = dc_qmat.first().copied().unwrap_or(0);
        let c0 = coeffs_bi.first().copied().unwrap_or(0);
        let dc = trunc16((c0.wrapping_mul(qmat0).wrapping_add(15)) >> 5);
        [[dc; 8]; 8]
    } else {
        let qi = qis.get(qii as usize).copied().unwrap_or(qi0);
        let dc_qmat = quant.matrix(0, pli, qi0);
        let ac_qmat = quant.matrix(0, pli, qi);
        let mut dqc = [0i32; 64];
        if let Some(slot) = dqc.first_mut() {
            let c0 = coeffs_bi.first().copied().unwrap_or(0);
            let q0 = dc_qmat.first().copied().unwrap_or(0);
            *slot = trunc16(c0.wrapping_mul(q0));
        }
        for ci in 1..64usize {
            let zzi = NATURAL_TO_ZIGZAG.get(ci).copied().unwrap_or(0);
            let coeff = coeffs_bi.get(zzi).copied().unwrap_or(0);
            let qval = ac_qmat.get(ci).copied().unwrap_or(0);
            if let Some(slot) = dqc.get_mut(ci) {
                *slot = trunc16(coeff.wrapping_mul(qval));
            }
        }
        idct_2d(&dqc)
    }
}

const fn lflim(res: i32, limit: i32) -> i32 {
    if res <= -2 * limit {
        0
    } else if res <= -limit {
        -res - 2 * limit
    } else if res < limit {
        res
    } else if res < 2 * limit {
        -res + 2 * limit
    } else {
        0
    }
}

/// Section 7.10.1: the 4-tap horizontal filter, at spec (bottom-up)
/// coordinates.
fn filter_horizontal(plane: &mut PlaneBuf, plane_h: u32, fx: u32, fy: u32, limit: i32) {
    for by in 0..8u32 {
        let y = fy + by;
        let (p0, p1, p2, p3) = (
            i32::from(plane.get_bottom_up(fx, y, plane_h)),
            i32::from(plane.get_bottom_up(fx + 1, y, plane_h)),
            i32::from(plane.get_bottom_up(fx + 2, y, plane_h)),
            i32::from(plane.get_bottom_up(fx + 3, y, plane_h)),
        );
        let r = (p0 - 3 * p1 + 3 * p2 - p3 + 4) >> 3;
        let l = lflim(r, limit);
        let v1 = (p1 + l).clamp(0, 255);
        let v2 = (p2 - l).clamp(0, 255);
        plane.set_bottom_up(fx + 1, y, plane_h, u8::try_from(v1).unwrap_or(0));
        plane.set_bottom_up(fx + 2, y, plane_h, u8::try_from(v2).unwrap_or(0));
    }
}

/// Section 7.10.2: the 4-tap vertical filter, at spec (bottom-up)
/// coordinates.
fn filter_vertical(plane: &mut PlaneBuf, plane_h: u32, fx: u32, fy: u32, limit: i32) {
    for bx in 0..8u32 {
        let x = fx + bx;
        let (p0, p1, p2, p3) = (
            i32::from(plane.get_bottom_up(x, fy, plane_h)),
            i32::from(plane.get_bottom_up(x, fy + 1, plane_h)),
            i32::from(plane.get_bottom_up(x, fy + 2, plane_h)),
            i32::from(plane.get_bottom_up(x, fy + 3, plane_h)),
        );
        let r = (p0 - 3 * p1 + 3 * p2 - p3 + 4) >> 3;
        let l = lflim(r, limit);
        let v1 = (p1 + l).clamp(0, 255);
        let v2 = (p2 - l).clamp(0, 255);
        plane.set_bottom_up(x, fy + 1, plane_h, u8::try_from(v1).unwrap_or(0));
        plane.set_bottom_up(x, fy + 2, plane_h, u8::try_from(v2).unwrap_or(0));
    }
}

/// Decode one intra frame's payload (everything after the 7-bit frame-type
/// prologue is handled inline via [`parse_frame_header`]) into full
/// coded-frame-sized, loop-filtered planes.
pub(crate) fn decode_frame_payload(
    payload: &[u8],
    ident: &Ident,
    setup: &Setup,
    geom: &FrameGeom,
    budget: &mut Budget,
) -> Result<DecodedFrame> {
    let mut r = BitReader::new(payload);
    let header = parse_frame_header(&mut r)?;

    let qiis = decode_block_qiis(&mut r, geom.nbs, header.nqis, budget)?;
    let (mut coeffs, ncoeffs) = decode_coefficients(&mut r, geom, &setup.tables, budget)?;
    undo_dc_prediction(&mut coeffs, geom);

    let (lw, lh) = (ident.fmbw.saturating_mul(16), ident.fmbh.saturating_mul(16));
    let (cbw, cbh) = ident.pf.chroma_blocks(ident.fmbw, ident.fmbh);
    let (cw, ch) = (cbw.saturating_mul(8), cbh.saturating_mul(8));

    let mut planes = [
        PlaneBuf::alloc(lw, lh, budget)?,
        PlaneBuf::alloc(cw, ch, budget)?,
        PlaneBuf::alloc(cw, ch, budget)?,
    ];

    for (pli, geom_plane) in geom.planes.iter().enumerate() {
        let plane_h_px = geom_plane.blocks_tall.saturating_mul(8);
        for by in 0..geom_plane.blocks_tall {
            for bx in 0..geom_plane.blocks_wide {
                budget.consume_fuel(1)?;
                let Some(bi) = geom_plane.coded_of(bx, by) else {
                    continue;
                };
                let nc = ncoeffs.get(bi as usize).copied().unwrap_or(0);
                let qii = qiis.get(bi as usize).copied().unwrap_or(0);
                let Some(c) = coeffs.get(bi as usize) else {
                    continue;
                };
                let res = reconstruct_residual(&setup.quant, &header.qis, qii, &c.0, nc, pli);
                let Some(target) = planes.get_mut(pli) else {
                    continue;
                };
                let (bx_px, by_px) = (bx.saturating_mul(8), by.saturating_mul(8));
                for (ry, row) in res.iter().enumerate() {
                    for (rx, &v) in row.iter().enumerate() {
                        let p = (128 + v).clamp(0, 255);
                        target.set_bottom_up(
                            bx_px + u32::try_from(rx).unwrap_or(0),
                            by_px + u32::try_from(ry).unwrap_or(0),
                            plane_h_px,
                            u8::try_from(p).unwrap_or(0),
                        );
                    }
                }
            }
        }
    }

    // Section 7.10.3: loop filter, left and bottom edges of every block only
    // (see the module doc for why the right/top pass is provably a no-op
    // here).
    let limit = i32::try_from(
        setup
            .lflims
            .get(header.qis.first().copied().unwrap_or(0) as usize)
            .copied()
            .unwrap_or(0),
    )
    .unwrap_or(0);
    if limit > 0 {
        for (pli, geom_plane) in geom.planes.iter().enumerate() {
            let plane_h_px = geom_plane.blocks_tall.saturating_mul(8);
            for by in 0..geom_plane.blocks_tall {
                for bx in 0..geom_plane.blocks_wide {
                    budget.consume_fuel(1)?;
                    let (bx_px, by_px) = (bx.saturating_mul(8), by.saturating_mul(8));
                    let Some(plane) = planes.get_mut(pli) else {
                        continue;
                    };
                    if bx_px > 0 {
                        filter_horizontal(plane, plane_h_px, bx_px - 2, by_px, limit);
                    }
                    if by_px > 0 {
                        filter_vertical(plane, plane_h_px, bx_px, by_px - 2, limit);
                    }
                }
            }
        }
    }

    Ok(DecodedFrame { planes })
}

/// Crop a fully reconstructed frame's planes down to `Ident`'s picture
/// region, mapping the spec's bottom-left-origin `(PICX, PICY)` into this
/// crate's top-down plane storage.
///
/// Chroma crop bounds are computed by simple proportional scaling. The
/// spec's exact odd-offset/odd-size chroma sample-correspondence rules
/// (section 4.4.4) are not implemented: real encoders overwhelmingly use
/// even, block-aligned picture regions, and getting the last chroma row or
/// column wrong on an odd, unaligned crop is exactly the kind of
/// "unstructured, rarely hit" gap this crate's own shipping bar tolerates —
/// see the crate root doc's "known gap" section.
pub(crate) fn crop_plane(
    src: &PlaneBuf,
    full_w: u32,
    full_h: u32,
    x: u32,
    y_from_bottom: u32,
    w: u32,
    h: u32,
) -> Vec<u8> {
    let mut out = vec![0u8; (w as usize).saturating_mul(h as usize)];
    let top_y = full_h
        .saturating_sub(1)
        .saturating_sub(y_from_bottom)
        .saturating_sub(h.saturating_sub(1));
    for row in 0..h {
        let src_y = top_y.saturating_add(row);
        if src_y >= full_h {
            continue;
        }
        for col in 0..w {
            let src_x = x.saturating_add(col);
            if src_x >= full_w {
                continue;
            }
            let v = src.get(src_x, src_y);
            let idx = (row as usize).saturating_mul(w as usize) + col as usize;
            if let Some(slot) = out.get_mut(idx) {
                *slot = v;
            }
        }
    }
    out
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    #[test]
    fn dc_weights_covers_every_nonzero_pattern() {
        for a in [false, true] {
            for b in [false, true] {
                for c in [false, true] {
                    for d in [false, true] {
                        let _ = dc_weights([a, b, c, d]);
                    }
                }
            }
        }
    }

    #[test]
    fn lflim_is_zero_outside_the_response_band() {
        assert_eq!(lflim(0, 10), 0);
        assert_eq!(lflim(100, 10), 0);
        assert_eq!(lflim(-100, 10), 0);
    }

    #[test]
    fn lflim_is_identity_inside_the_flat_band() {
        assert_eq!(lflim(5, 10), 5);
        assert_eq!(lflim(-5, 10), -5);
    }

    #[test]
    fn plane_buf_bottom_up_round_trips() {
        let mut budget = Budget::new(vaco_limits::Limits::permissive());
        let mut p = PlaneBuf::alloc(16, 16, &mut budget).unwrap();
        p.set_bottom_up(0, 0, 16, 42);
        assert_eq!(p.get_bottom_up(0, 0, 16), 42);
        assert_eq!(p.get(0, 15), 42); // bottom-up row 0 is buffer row height-1
    }
}
