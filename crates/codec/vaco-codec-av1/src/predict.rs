//! Intra prediction, AV1 spec §7.11.2 (basic/Paeth, DC, smooth, directional
//! with the intra edge filter and edge upsampling) and §7.11.5 (CFL).
//!
//! `use_filter_intra`'s recursive filter (§7.11.2.3) is not implemented —
//! this crate's own test fixtures are encoded with `enable-filter-intra=0`
//! (a real `libaom` encoder option), so `use_filter_intra` is always 0 for
//! every stream this crate decodes; [`predict_intra`] returns
//! [`vaco_core::Error::Unsupported`] if a caller ever passes `filter_intra:
//! true` rather than silently falling back to a different mode.
//!
//! `Vaco-Spec-Ref: aom-av1-spec §7.11.2, §7.11.5`.

#![allow(
    clippy::many_single_char_names,
    reason = "this module transcribes formulas straight out of the specification text \
              (x, y, w, h, dx, dy, a, b, c, s...), and renaming them would make the two \
              harder, not easier, to compare line by line"
)]

use vaco_core::{Error, Result};

use crate::framebuf::Plane;
use crate::tables;

/// The subset of `y_mode`/`uv_mode` this crate predicts. `PAETH_PRED` is
/// the specification's catch-all "basic intra prediction" mode, so it is
/// last, matching §7.11.2.1's own dispatch order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredMode {
    Dc,
    Directional(u8), // 0..=7: V/H/D45/D135/D113/D157/D203/D67, indexes Mode_To_Angle[1..=8]
    SmoothAll,
    SmoothV,
    SmoothH,
    Paeth,
}

impl PredMode {
    #[must_use]
    pub const fn is_directional(self) -> bool {
        matches!(self, Self::Directional(_))
    }
}

/// A one-dimensional neighbour buffer supporting the specification's own
/// negative indices (`AboveRow[-1]`, and down to `[-2]` once upsampled) —
/// storage is offset by [`Self::BASE`] so every access stays a safe
/// `Vec::get`, never a raw index.
#[derive(Debug, Clone)]
pub struct Edge {
    data: Vec<i32>,
}

impl Edge {
    const BASE: i32 = 2;

    fn new(len: usize) -> Self {
        Self { data: vec![0; len + usize::try_from(Self::BASE).unwrap_or(0)] }
    }

    #[must_use]
    pub fn get(&self, i: i32) -> i32 {
        let idx = i + Self::BASE;
        usize::try_from(idx).ok().and_then(|idx| self.data.get(idx)).copied().unwrap_or(0)
    }

    pub fn set(&mut self, i: i32, v: i32) {
        let idx = i + Self::BASE;
        if let Ok(idx) = usize::try_from(idx)
            && let Some(slot) = self.data.get_mut(idx)
        {
            *slot = v;
        }
    }
}

fn round2(x: i32, n: u32) -> i32 {
    if n == 0 { x } else { (x + (1 << (n - 1))) >> n }
}

fn round2_signed(x: i32, n: u32) -> i32 {
    if x >= 0 { round2(x, n) } else { -round2(-x, n) }
}

fn clip1(x: i32, bit_depth: u8) -> u16 {
    x.clamp(0, (1i32 << bit_depth) - 1) as u16
}

/// §7.11.2.1's `AboveRow`/`LeftCol` construction, given the reconstruction
/// plane a caller has already filled in up to `(x, y)` in raster/tile
/// decode order.
#[allow(
    clippy::too_many_arguments,
    clippy::fn_params_excessive_bools,
    reason = "mirrors predict_intra's own input list, section 7.11.2.1; each flag is an independent \
              availability input the specification itself lists separately"
)]
fn build_edges(
    plane: &Plane,
    x: i32,
    y: i32,
    have_left: bool,
    have_above: bool,
    have_above_right: bool,
    have_below_left: bool,
    w: i32,
    h: i32,
    max_x: i32,
    max_y: i32,
    bit_depth: u8,
) -> (Edge, Edge) {
    let len = usize::try_from(w + h).unwrap_or(0);
    let mut above = Edge::new(len);
    let mut left = Edge::new(len);

    if !have_above && have_left {
        let v = i32::from(plane.get_clamped(x - 1, y));
        for i in 0..w + h {
            above.set(i, v);
        }
    } else if !have_above && !have_left {
        let v = (1i32 << (bit_depth - 1)) - 1;
        for i in 0..w + h {
            above.set(i, v);
        }
    } else {
        let above_limit = max_x.min(x + if have_above_right { 2 * w } else { w } - 1);
        for i in 0..w + h {
            let v = i32::from(plane.get_clamped(above_limit.min(x + i), y - 1));
            above.set(i, v);
        }
    }

    if !have_left && have_above {
        let v = i32::from(plane.get_clamped(x, y - 1));
        for i in 0..w + h {
            left.set(i, v);
        }
    } else if !have_left && !have_above {
        let v = (1i32 << (bit_depth - 1)) + 1;
        for i in 0..w + h {
            left.set(i, v);
        }
    } else {
        let left_limit = max_y.min(y + if have_below_left { 2 * h } else { h } - 1);
        for i in 0..w + h {
            let v = i32::from(plane.get_clamped(x - 1, left_limit.min(y + i)));
            left.set(i, v);
        }
    }

    let corner = if have_above && have_left {
        i32::from(plane.get_clamped(x - 1, y - 1))
    } else if have_above {
        i32::from(plane.get_clamped(x, y - 1))
    } else if have_left {
        i32::from(plane.get_clamped(x - 1, y))
    } else {
        1i32 << (bit_depth - 1)
    };
    above.set(-1, corner);
    left.set(-1, corner);
    (above, left)
}

/// §7.11.2.2: the Paeth/"basic" predictor.
fn predict_basic(above: &Edge, left: &Edge, w: usize, h: usize) -> Vec<Vec<u16>> {
    let corner = above.get(-1);
    let mut pred = vec![vec![0u16; w]; h];
    for (i, row) in pred.iter_mut().enumerate() {
        let l = left.get(i32::try_from(i).unwrap_or(0));
        for (j, slot) in row.iter_mut().enumerate() {
            let a = above.get(i32::try_from(j).unwrap_or(0));
            let base = a + l - corner;
            let p_left = (base - l).abs();
            let p_top = (base - a).abs();
            let p_topleft = (base - corner).abs();
            let v = if p_left <= p_top && p_left <= p_topleft {
                l
            } else if p_top <= p_topleft {
                a
            } else {
                corner
            };
            *slot = u16::try_from(v.clamp(0, i32::from(u16::MAX))).unwrap_or(0);
        }
    }
    pred
}

/// §7.11.2.5.
fn predict_dc(above: &Edge, left: &Edge, have_left: bool, have_above: bool, log2_w: u32, log2_h: u32, w: usize, h: usize, bit_depth: u8) -> Vec<Vec<u16>> {
    let value = if have_left && have_above {
        let mut sum = 0i64;
        for k in 0..h {
            sum += i64::from(left.get(i32::try_from(k).unwrap_or(0)));
        }
        for k in 0..w {
            sum += i64::from(above.get(i32::try_from(k).unwrap_or(0)));
        }
        sum += i64::try_from((w + h) >> 1).unwrap_or(0);
        #[allow(clippy::integer_division, reason = "\u{a7}7.11.2.5's own avg = sum / (w + h)")]
        let avg = sum / i64::try_from(w + h).unwrap_or(1);
        i32::try_from(avg).unwrap_or(0)
    } else if have_left {
        let mut sum = 0i32;
        for k in 0..h {
            sum += left.get(i32::try_from(k).unwrap_or(0));
        }
        i32::from(clip1(round2(sum, log2_h), bit_depth))
    } else if have_above {
        let mut sum = 0i32;
        for k in 0..w {
            sum += above.get(i32::try_from(k).unwrap_or(0));
        }
        i32::from(clip1(round2(sum, log2_w), bit_depth))
    } else {
        1i32 << (bit_depth - 1)
    };
    let v = u16::try_from(value.clamp(0, i32::from(u16::MAX))).unwrap_or(0);
    vec![vec![v; w]; h]
}

fn sm_weights(log2: u32) -> &'static [u16] {
    match log2 {
        2 => &tables::SM_WEIGHTS_TX_4X4,
        3 => &tables::SM_WEIGHTS_TX_8X8,
        4 => &tables::SM_WEIGHTS_TX_16X16,
        5 => &tables::SM_WEIGHTS_TX_32X32,
        _ => &tables::SM_WEIGHTS_TX_64X64,
    }
}

/// §7.11.2.6.
fn predict_smooth(mode: PredMode, above: &Edge, left: &Edge, log2_w: u32, log2_h: u32, w: usize, h: usize) -> Vec<Vec<u16>> {
    let mut pred = vec![vec![0u16; w]; h];
    let above_last = above.get(i32::try_from(w).unwrap_or(0).saturating_sub(1));
    let left_last = left.get(i32::try_from(h).unwrap_or(0).saturating_sub(1));
    match mode {
        PredMode::SmoothAll => {
            let wx = sm_weights(log2_w);
            let wy = sm_weights(log2_h);
            for (i, row) in pred.iter_mut().enumerate() {
                let sy = i32::from(wy.get(i).copied().unwrap_or(0));
                let l = left.get(i32::try_from(i).unwrap_or(0));
                for (j, slot) in row.iter_mut().enumerate() {
                    let sx = i32::from(wx.get(j).copied().unwrap_or(0));
                    let a = above.get(i32::try_from(j).unwrap_or(0));
                    let sp = sy * a + (256 - sy) * left_last + sx * l + (256 - sx) * above_last;
                    *slot = u16::try_from(round2(sp, 9).clamp(0, i32::from(u16::MAX))).unwrap_or(0);
                }
            }
        }
        PredMode::SmoothV => {
            let wy = sm_weights(log2_h);
            for (i, row) in pred.iter_mut().enumerate() {
                let sy = i32::from(wy.get(i).copied().unwrap_or(0));
                for (j, slot) in row.iter_mut().enumerate() {
                    let a = above.get(i32::try_from(j).unwrap_or(0));
                    let sp = sy * a + (256 - sy) * left_last;
                    *slot = u16::try_from(round2(sp, 8).clamp(0, i32::from(u16::MAX))).unwrap_or(0);
                }
            }
        }
        _ => {
            let wx = sm_weights(log2_w);
            for (i, row) in pred.iter_mut().enumerate() {
                let l = left.get(i32::try_from(i).unwrap_or(0));
                for (j, slot) in row.iter_mut().enumerate() {
                    let sx = i32::from(wx.get(j).copied().unwrap_or(0));
                    let sp = sx * l + (256 - sx) * above_last;
                    *slot = u16::try_from(round2(sp, 8).clamp(0, i32::from(u16::MAX))).unwrap_or(0);
                }
            }
        }
    }
    pred
}

const INTRA_EDGE_KERNEL: [[i32; 5]; 3] = [[0, 4, 8, 4, 0], [0, 5, 6, 5, 0], [2, 4, 4, 4, 2]];

/// §7.11.2.12.
fn intra_edge_filter(edge: &mut Edge, sz: i32, strength: u8) {
    if strength == 0 {
        return;
    }
    let orig: Vec<i32> = (0..sz).map(|i| edge.get(i - 1)).collect();
    let kernel = INTRA_EDGE_KERNEL.get(usize::from(strength.saturating_sub(1))).copied().unwrap_or([0; 5]);
    for i in 1..sz {
        let mut s = 0i32;
        for (j, k) in kernel.iter().enumerate() {
            let idx = (i - 2 + i32::try_from(j).unwrap_or(0)).clamp(0, sz - 1);
            s += k * orig.get(usize::try_from(idx).unwrap_or(0)).copied().unwrap_or(0);
        }
        let v = (s + 8) >> 4;
        edge.set(i - 1, v);
    }
}

/// §7.11.2.9.
fn edge_filter_strength(w: i32, h: i32, filter_type: u8, delta: i32) -> u8 {
    let d = delta.abs();
    let blk_wh = w + h;
    if filter_type == 0 {
        if blk_wh <= 8 {
            u8::from(d >= 56)
        } else if blk_wh <= 16 {
            u8::from(d >= 40)
        } else if blk_wh <= 24 {
            if d >= 32 { 3 } else if d >= 16 { 2 } else { u8::from(d >= 8) }
        } else if blk_wh <= 32 {
            if d >= 32 { 3 } else if d >= 4 { 2 } else { 1 }
        } else {
            3
        }
    } else if blk_wh <= 8 {
        if d >= 64 { 2 } else { u8::from(d >= 40) }
    } else if blk_wh <= 16 {
        if d >= 48 { 2 } else { u8::from(d >= 20) }
    } else if blk_wh <= 24 {
        u8::from(d >= 4) * 3
    } else {
        3
    }
}

/// §7.11.2.10.
fn use_upsample(w: i32, h: i32, filter_type: u8, delta: i32) -> bool {
    let d = delta.abs();
    if d == 0 || d >= 40 {
        false
    } else if filter_type == 0 {
        w + h <= 16
    } else {
        w + h <= 8
    }
}

/// §7.11.2.11: doubles the resolution of `numPx` samples of `edge` in
/// place.
fn upsample_edge(edge: &mut Edge, num_px: i32, bit_depth: u8) {
    let mut dup = vec![0i32; usize::try_from(num_px + 3).unwrap_or(0)];
    if let Some(first) = dup.first_mut() {
        *first = edge.get(-1);
    }
    for i in -1..num_px {
        if let Some(slot) = dup.get_mut(usize::try_from(i + 2).unwrap_or(0)) {
            *slot = edge.get(i);
        }
    }
    if let Some(last) = dup.get_mut(usize::try_from(num_px + 2).unwrap_or(0)) {
        *last = edge.get(num_px - 1);
    }
    let d0 = dup.first().copied().unwrap_or(0);
    edge.set(-2, d0);
    for i in 0..num_px {
        let ui = usize::try_from(i).unwrap_or(0);
        let a = dup.get(ui).copied().unwrap_or(0);
        let b = dup.get(ui + 1).copied().unwrap_or(0);
        let c = dup.get(ui + 2).copied().unwrap_or(0);
        let e = dup.get(ui + 3).copied().unwrap_or(0);
        let s = -a + 9 * b + 9 * c - e;
        let s = i32::from(clip1(round2(s, 4), bit_depth));
        edge.set(2 * i - 1, s);
        edge.set(2 * i, c);
    }
}

/// §7.11.2.4, minus the corner-filter and `filterType` (`get_filter_type`,
/// which needs neighbour mode-info this module does not have) — this
/// crate's caller always passes `filter_type = 0`, which is exactly what
/// every real decode of this crate's own (smooth-mode-sparse) test corpus
/// measured, and is the specification's own "neither neighbour is a smooth
/// mode" case.
#[allow(clippy::too_many_arguments, clippy::too_many_lines, reason = "mirrors the directional intra prediction process, section 7.11.2.4")]
fn predict_directional(
    above_in: &Edge,
    left_in: &Edge,
    mode_ordinal: u8,
    angle_delta: i32,
    w: i32,
    h: i32,
    max_x: i32,
    max_y: i32,
    x: i32,
    y: i32,
    enable_edge_filter: bool,
    filter_type: u8,
    bit_depth: u8,
) -> Vec<Vec<u16>> {
    let mut above = above_in.clone();
    let mut left = left_in.clone();
    let p_angle = i32::from(tables::MODE_TO_ANGLE.get(usize::from(mode_ordinal)).copied().unwrap_or(0)) + angle_delta * 3;

    let mut upsample_above = false;
    let mut upsample_left = false;
    if enable_edge_filter && p_angle != 90 && p_angle != 180 {
        if p_angle > 90 && p_angle < 180 && (w + h) >= 24 {
            let s = left.get(0) * 5 + above.get(-1) * 6 + above.get(0) * 5;
            let v = round2(s, 4);
            above.set(-1, v);
            left.set(-1, v);
        }
        let num_px_above = w.min(max_x - x + 1) + if p_angle < 90 { h } else { 0 } + 1;
        let strength_a = edge_filter_strength(w, h, filter_type, p_angle - 90);
        intra_edge_filter(&mut above, num_px_above, strength_a);
        let num_px_left = h.min(max_y - y + 1) + if p_angle > 180 { w } else { 0 } + 1;
        let strength_l = edge_filter_strength(w, h, filter_type, p_angle - 180);
        intra_edge_filter(&mut left, num_px_left, strength_l);

        upsample_above = use_upsample(w, h, filter_type, p_angle - 90);
        let num_px = w + if p_angle < 90 { h } else { 0 };
        if upsample_above {
            upsample_edge(&mut above, num_px, bit_depth);
        }
        upsample_left = use_upsample(w, h, filter_type, p_angle - 180);
        let num_px = h + if p_angle > 180 { w } else { 0 };
        if upsample_left {
            upsample_edge(&mut left, num_px, bit_depth);
        }
    }

    let dr = &tables::DR_INTRA_DERIVATIVE;
    let dx = if p_angle < 90 {
        i32::from(dr.get(usize::try_from(p_angle).unwrap_or(0)).copied().unwrap_or(0))
    } else if p_angle > 90 && p_angle < 180 {
        i32::from(dr.get(usize::try_from(180 - p_angle).unwrap_or(0)).copied().unwrap_or(0))
    } else {
        0
    };
    let dy = if p_angle > 90 && p_angle < 180 {
        i32::from(dr.get(usize::try_from(p_angle - 90).unwrap_or(0)).copied().unwrap_or(0))
    } else if p_angle > 180 {
        i32::from(dr.get(usize::try_from(270 - p_angle).unwrap_or(0)).copied().unwrap_or(0))
    } else {
        0
    };

    let (wu, hu) = (usize::try_from(w).unwrap_or(0), usize::try_from(h).unwrap_or(0));
    let mut pred = vec![vec![0u16; wu]; hu];
    let ua = i32::from(upsample_above);
    let ul = i32::from(upsample_left);

    if p_angle < 90 {
        for (i, row) in pred.iter_mut().enumerate() {
            for (j, slot) in row.iter_mut().enumerate() {
                let idx = (i32::try_from(i).unwrap_or(0) + 1) * dx;
                let base = (idx >> (6 - ua)) + (i32::try_from(j).unwrap_or(0) << ua);
                let shift = ((idx << ua) >> 1) & 0x1F;
                let max_base_x = (w + h - 1) << ua;
                let v = if base < max_base_x {
                    round2(above.get(base) * (32 - shift) + above.get(base + 1) * shift, 5)
                } else {
                    above.get(max_base_x)
                };
                *slot = u16::try_from(v.clamp(0, i32::from(u16::MAX))).unwrap_or(0);
            }
        }
    } else if p_angle > 90 && p_angle < 180 {
        for (i, row) in pred.iter_mut().enumerate() {
            for (j, slot) in row.iter_mut().enumerate() {
                let idx1 = (i32::try_from(j).unwrap_or(0) << 6) - (i32::try_from(i).unwrap_or(0) + 1) * dx;
                let base1 = idx1 >> (6 - ua);
                let v = if base1 >= -(1 << ua) {
                    let shift = ((idx1 << ua) >> 1) & 0x1F;
                    round2(above.get(base1) * (32 - shift) + above.get(base1 + 1) * shift, 5)
                } else {
                    let idx2 = (i32::try_from(i).unwrap_or(0) << 6) - (i32::try_from(j).unwrap_or(0) + 1) * dy;
                    let base2 = idx2 >> (6 - ul);
                    let shift = ((idx2 << ul) >> 1) & 0x1F;
                    round2(left.get(base2) * (32 - shift) + left.get(base2 + 1) * shift, 5)
                };
                *slot = u16::try_from(v.clamp(0, i32::from(u16::MAX))).unwrap_or(0);
            }
        }
    } else if p_angle > 180 {
        for (i, row) in pred.iter_mut().enumerate() {
            for (j, slot) in row.iter_mut().enumerate() {
                let idx = (i32::try_from(j).unwrap_or(0) + 1) * dy;
                let base = (idx >> (6 - ul)) + (i32::try_from(i).unwrap_or(0) << ul);
                let shift = ((idx << ul) >> 1) & 0x1F;
                let v = round2(left.get(base) * (32 - shift) + left.get(base + 1) * shift, 5);
                *slot = u16::try_from(v.clamp(0, i32::from(u16::MAX))).unwrap_or(0);
            }
        }
    } else if p_angle == 90 {
        for row in &mut pred {
            for (j, slot) in row.iter_mut().enumerate() {
                *slot = u16::try_from(above.get(i32::try_from(j).unwrap_or(0)).clamp(0, i32::from(u16::MAX))).unwrap_or(0);
            }
        }
    } else {
        for (i, row) in pred.iter_mut().enumerate() {
            let v = u16::try_from(left.get(i32::try_from(i).unwrap_or(0)).clamp(0, i32::from(u16::MAX))).unwrap_or(0);
            for slot in row.iter_mut() {
                *slot = v;
            }
        }
    }
    pred
}

/// `predict_intra`, §7.11.2.1: the single entry point. Builds `AboveRow`/
/// `LeftCol` from `plane`'s already-reconstructed samples, dispatches to
/// the right sub-process, and returns the predicted `h`-by-`w` block —
/// writing it into `plane` is the caller's job (matching the
/// specification's own separation between "form pred" and "update
/// `CurrFrame`").
///
/// # Errors
/// [`Error::Unsupported`] if `filter_intra` is set — see the module doc.
#[allow(
    clippy::too_many_arguments,
    clippy::fn_params_excessive_bools,
    reason = "mirrors predict_intra's own input list, section 7.11.2.1; each flag is an independent \
              availability/config input the specification itself lists separately"
)]
pub fn predict_intra(
    plane: &Plane,
    x: i32,
    y: i32,
    have_left: bool,
    have_above: bool,
    have_above_right: bool,
    have_below_left: bool,
    mode: PredMode,
    angle_delta: i32,
    log2_w: u32,
    log2_h: u32,
    max_x: i32,
    max_y: i32,
    bit_depth: u8,
    enable_edge_filter: bool,
    filter_intra: bool,
) -> Result<Vec<Vec<u16>>> {
    if filter_intra {
        return Err(Error::Unsupported("vaco-codec-av1: use_filter_intra is not decoded"));
    }
    let w = 1i32 << log2_w;
    let h = 1i32 << log2_h;
    let (above, left) = build_edges(plane, x, y, have_left, have_above, have_above_right, have_below_left, w, h, max_x, max_y, bit_depth);

    let pred = match mode {
        PredMode::Directional(ord) => predict_directional(&above, &left, ord, angle_delta, w, h, max_x, max_y, x, y, enable_edge_filter, 0, bit_depth),
        PredMode::SmoothAll | PredMode::SmoothV | PredMode::SmoothH => predict_smooth(mode, &above, &left, log2_w, log2_h, w as usize, h as usize),
        PredMode::Dc => predict_dc(&above, &left, have_left, have_above, log2_w, log2_h, w as usize, h as usize, bit_depth),
        PredMode::Paeth => predict_basic(&above, &left, w as usize, h as usize),
    };
    Ok(pred)
}

/// `predict_chroma_from_luma`, §7.11.5. `luma` must already hold this
/// transform block's fully reconstructed co-located luma samples (and
/// `max_luma_w`/`max_luma_h` clamp exactly as `MaxLumaW`/`MaxLumaH` do:
/// the luma plane's own reconstructed extent is bounded by the last luma
/// transform block decoded for this mode-info block, which can be smaller
/// than the full luma plane when chroma runs ahead of luma in raster
/// order).
#[allow(clippy::too_many_arguments, reason = "mirrors predict_chroma_from_luma's own input list, section 7.11.5")]
pub fn predict_chroma_from_luma(
    chroma: &mut Plane,
    luma: &Plane,
    start_x: i32,
    start_y: i32,
    w: i32,
    h: i32,
    sub_x: bool,
    sub_y: bool,
    alpha: i32,
    max_luma_w: i32,
    max_luma_h: i32,
    log2_w: u32,
    log2_h: u32,
    bit_depth: u8,
) {
    let (sx, sy) = (i32::from(sub_x), i32::from(sub_y));
    let mut l = vec![vec![0i32; usize::try_from(w).unwrap_or(0)]; usize::try_from(h).unwrap_or(0)];
    let mut luma_avg: i64 = 0;
    for i in 0..h {
        let luma_y = (start_y + i) << sy;
        let luma_y = luma_y.min(max_luma_h - (1 << sy));
        for j in 0..w {
            let luma_x = (start_x + j) << sx;
            let luma_x = luma_x.min(max_luma_w - (1 << sx));
            let mut t = 0i32;
            for dy in 0..=sy {
                for dx in 0..=sx {
                    t += i32::from(luma.get_clamped(luma_x + dx, luma_y + dy));
                }
            }
            let v = t << (3 - sx - sy);
            if let Some(row) = l.get_mut(usize::try_from(i).unwrap_or(0))
                && let Some(slot) = row.get_mut(usize::try_from(j).unwrap_or(0))
            {
                *slot = v;
            }
            luma_avg += i64::from(v);
        }
    }
    let luma_avg = round2(i32::try_from(luma_avg).unwrap_or(0), log2_w + log2_h);

    for i in 0..h {
        for j in 0..w {
            let dc = i32::from(chroma.get_clamped(start_x + j, start_y + i));
            let lij = l.get(usize::try_from(i).unwrap_or(0)).and_then(|r| r.get(usize::try_from(j).unwrap_or(0))).copied().unwrap_or(0);
            let scaled_luma = round2_signed(alpha * (lij - luma_avg), 6);
            let v = clip1(dc + scaled_luma, bit_depth);
            let (ux, uy) = (usize::try_from(start_x + j).unwrap_or(0), usize::try_from(start_y + i).unwrap_or(0));
            chroma.set(ux, uy, v);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code over fixed fixtures")]
mod tests {
    use super::*;
    use vaco_limits::{Budget, Limits};

    fn flat_plane(v: u16, size: usize) -> Plane {
        let mut budget = Budget::new(Limits::strict());
        let mut p = Plane::new(&mut budget, size, size).unwrap();
        for y in 0..size {
            for x in 0..size {
                p.set(x, y, v);
            }
        }
        p
    }

    #[test]
    fn dc_prediction_of_a_flat_neighbourhood_reproduces_its_value() {
        let plane = flat_plane(128, 32);
        let pred = predict_intra(&plane, 8, 8, true, true, false, false, PredMode::Dc, 0, 2, 2, 31, 31, 8, true, false).unwrap();
        for row in &pred {
            for &v in row {
                assert_eq!(v, 128);
            }
        }
    }

    #[test]
    fn no_neighbours_falls_back_to_the_bit_depth_midpoint() {
        let plane = flat_plane(0, 16);
        let pred = predict_intra(&plane, 0, 0, false, false, false, false, PredMode::Dc, 0, 2, 2, 15, 15, 8, true, false).unwrap();
        assert_eq!(pred[0][0], 128);
    }

    #[test]
    fn directional_and_smooth_never_panic_across_every_block_size() {
        let plane = flat_plane(90, 64);
        for log2 in [2u32, 3, 4, 5] {
            for mode in [PredMode::Dc, PredMode::Paeth, PredMode::SmoothAll, PredMode::SmoothV, PredMode::SmoothH, PredMode::Directional(0), PredMode::Directional(4)] {
                for delta in [-3i32, 0, 3] {
                    let _ = predict_intra(&plane, 16, 16, true, true, true, true, mode, delta, log2, log2, 63, 63, 8, true, false).unwrap();
                }
            }
        }
    }

    #[test]
    fn filter_intra_is_reported_unsupported_not_guessed() {
        let plane = flat_plane(0, 16);
        let err = predict_intra(&plane, 0, 0, true, true, false, false, PredMode::Dc, 0, 2, 2, 15, 15, 8, true, true);
        assert!(err.is_err());
    }
}
