//! Running a [`Plan`] over real pixels.
//!
//! # Bands, not frames
//!
//! Work is done in horizontal bands of the destination. A band's output is a
//! pure function of the source rows its vertical filters reach, so:
//!
//! * intermediates are band-sized rather than frame-sized — a 4K
//!   `yuv420p -> rgb24` never materialises a 100 MB `i32` picture;
//! * bands are independent, so parallelism is `chunks_mut` on the destination
//!   and the borrow checker proves disjointness with no runtime mechanism;
//! * **thread count cannot change the output**, because a band never looks at
//!   another band's intermediates. That is asserted by a test, not assumed.
//!
//! Band height is a multiple of the destination's vertical chroma decimation,
//! so every band tiles every plane exactly and no plane has a half row.
//!
//! # The four passes
//!
//! ```text
//!   read + expand depth  ->  filter H (up)   ->  filter V (up)
//!                        ->  colour matrix
//!                        ->  filter V (down) ->  filter H (down)
//!                        ->  quantise + dither + write
//! ```
//!
//! Any pass whose bank turned out to be an identity is skipped, which is why an
//! unscaled `yuv420p -> nv12` reduces to read, expand-by-nothing, write.

use rayon::prelude::*;
use vaco_core::{Error, Result};
use vaco_limits::{Budget, Limits};

use crate::colour::{Affine, ColorStage, FloatTransform};
use crate::dither::{bayer_threshold, expand_depth, reduce_depth, reduce_depth_dithered};
use crate::filter::{COEFF_SHIFT, FilterBank};

/// Fractional bits carried between a horizontal and a vertical filter pass.
///
/// Seven, so an 8-bit picture travels between the two passes as 15-bit — which
/// is both what gives the second pass something to work with and what the
/// reference's intermediates measure as.
const EXTRA_BITS: u8 = 7;
use crate::geometry::{MAX_COMPS, ceil_shr};
use crate::options::DitherKind;
use crate::plan::{ChannelPlan, General, Plan, PlanKind, Synthetic};
use crate::rowio::{read_row, write_row};

/// One source plane: its bytes and its row stride.
#[derive(Debug, Clone, Copy)]
pub struct SrcPlane<'a> {
    /// Plane bytes, padding rows included.
    pub data: &'a [u8],
    /// Bytes between consecutive rows.
    pub stride: usize,
}

/// One destination plane.
#[derive(Debug)]
pub struct DstPlane<'a> {
    /// Plane bytes.
    pub data: &'a mut [u8],
    /// Bytes between consecutive rows.
    pub stride: usize,
}

/// Run `plan`, converting `src` into `dst`.
///
/// `pool` selects the worker pool; `None` runs on the calling thread.
///
/// # Errors
///
/// [`Error::InvalidData`] when the plane count or a plane's length does not
/// match what the plan expects, and [`Error::LimitExceeded`] when scratch
/// buffers would not fit the budget.
pub fn run(
    plan: &Plan,
    budget: &Budget,
    src: &[SrcPlane<'_>],
    dst: &mut [DstPlane<'_>],
    pool: Option<&rayon::ThreadPool>,
) -> Result<()> {
    if src.len() < plan.src_layout.planes || dst.len() < plan.dst_layout.planes {
        return Err(Error::InvalidData(
            "plane count does not match pixel format",
        ));
    }
    check_planes(plan, src, dst)?;
    match &plan.kind {
        PlanKind::Copy => copy_planes(plan, src, dst),
        PlanKind::General(g) => run_general(plan, g, budget.limits(), src, dst, pool),
    }
}

fn plane_geometry(plan: &Plan, i: usize, source: bool) -> (usize, usize) {
    let spec = if source { &plan.src } else { &plan.dst };
    let rows = spec.format.plane_height(spec.height, i as u8) as usize;
    let bytes = spec.format.min_stride(spec.width, i as u8);
    (rows, bytes)
}

fn check_planes(plan: &Plan, src: &[SrcPlane<'_>], dst: &mut [DstPlane<'_>]) -> Result<()> {
    for (i, p) in src.iter().enumerate().take(plan.src_layout.planes) {
        let (rows, need) = plane_geometry(plan, i, true);
        if p.stride < need
            || rows
                .saturating_sub(1)
                .saturating_mul(p.stride)
                .saturating_add(need)
                > p.data.len()
        {
            return Err(Error::InvalidData("source plane is too small"));
        }
    }
    for (i, p) in dst.iter().enumerate().take(plan.dst_layout.planes) {
        let (rows, need) = plane_geometry(plan, i, false);
        if p.stride < need
            || rows
                .saturating_sub(1)
                .saturating_mul(p.stride)
                .saturating_add(need)
                > p.data.len()
        {
            return Err(Error::InvalidData("destination plane is too small"));
        }
    }
    Ok(())
}

fn copy_planes(plan: &Plan, src: &[SrcPlane<'_>], dst: &mut [DstPlane<'_>]) -> Result<()> {
    for i in 0..plan.dst_layout.planes {
        let (Some(s), Some(d)) = (src.get(i), dst.get_mut(i)) else {
            return Err(Error::InvalidData("plane index out of range"));
        };
        let (rows, bytes) = plane_geometry(plan, i, false);
        for y in 0..rows {
            let so = y.saturating_mul(s.stride);
            let dof = y.saturating_mul(d.stride);
            let (Some(sr), Some(dr)) = (
                s.data.get(so..so.saturating_add(bytes)),
                d.data.get_mut(dof..dof.saturating_add(bytes)),
            ) else {
                return Err(Error::InvalidData("plane row out of range"));
            };
            dr.copy_from_slice(sr);
        }
    }
    Ok(())
}

/// A rectangular block of `i32` samples covering absolute rows `y0..y0 + rows`.
#[derive(Debug, Clone)]
pub(crate) struct Grid {
    w: usize,
    y0: usize,
    rows: usize,
    data: Vec<i32>,
}

impl Grid {
    fn new(budget: &mut Budget, w: usize, y0: usize, rows: usize) -> Result<Self> {
        let n = w.checked_mul(rows).ok_or(Error::LimitExceeded {
            limit: "scale_scratch",
            requested: u64::MAX,
            cap: usize::MAX as u64,
        })?;
        Ok(Self {
            w,
            y0,
            rows,
            data: budget.alloc::<i32>(n)?,
        })
    }

    pub(crate) fn row(&self, y: usize) -> Option<&[i32]> {
        let i = y.checked_sub(self.y0)?;
        if i >= self.rows {
            return None;
        }
        let start = i.checked_mul(self.w)?;
        self.data.get(start..start.checked_add(self.w)?)
    }

    fn row_mut(&mut self, y: usize) -> Option<&mut [i32]> {
        let i = y.checked_sub(self.y0)?;
        if i >= self.rows {
            return None;
        }
        let start = i.checked_mul(self.w)?;
        let end = start.checked_add(self.w)?;
        self.data.get_mut(start..end)
    }
}

/// The rows of a bank's input that producing `d0..d1` of its output touches.
fn source_span(bank: &FilterBank, d0: usize, d1: usize) -> (usize, usize) {
    let mut lo = usize::MAX;
    let mut hi = 0usize;
    for d in d0..d1 {
        let Some(&o) = bank.offsets.get(d) else {
            continue;
        };
        let o = o as usize;
        lo = lo.min(o);
        hi = hi.max(o.saturating_add(bank.taps));
    }
    if lo == usize::MAX {
        let at = d0.min(bank.src_len);
        (at, at)
    } else {
        (lo, hi.min(bank.src_len))
    }
}

/// One band's view of one destination plane.
#[derive(Debug)]
struct PlaneBand<'a> {
    data: &'a mut [u8],
    stride: usize,
    /// First row of the plane this slice starts at.
    first_row: usize,
}

/// The destination slices one band owns.
#[derive(Debug, Default)]
struct BandDst<'a> {
    planes: [Option<PlaneBand<'a>>; MAX_COMPS],
}

fn run_general(
    plan: &Plan,
    g: &General,
    limits: &Limits,
    src: &[SrcPlane<'_>],
    dst: &mut [DstPlane<'_>],
    pool: Option<&rayon::ThreadPool>,
) -> Result<()> {
    let band_rows = plan.band_rows.max(1);
    let nbands = plan.dst.height.div_ceil(band_rows) as usize;
    if nbands == 0 {
        return Ok(());
    }

    // Split every destination plane into per-band, non-overlapping slices. The
    // compiler proves the bands cannot alias, so no runtime mechanism is needed.
    let mut bands: Vec<BandDst<'_>> = (0..nbands).map(|_| BandDst::default()).collect();
    for (pi, p) in dst.iter_mut().enumerate().take(plan.dst_layout.planes) {
        let sub = plane_vsub(plan, pi);
        let rows_per_band = (band_rows >> sub).max(1) as usize;
        let chunk = rows_per_band.saturating_mul(p.stride).max(1);
        let stride = p.stride;
        for (bi, slice) in p.data.chunks_mut(chunk).enumerate() {
            if let Some(band) = bands.get_mut(bi)
                && let Some(slot) = band.planes.get_mut(pi)
            {
                *slot = Some(PlaneBand {
                    data: slice,
                    stride,
                    first_row: bi.saturating_mul(rows_per_band),
                });
            }
        }
    }

    match pool {
        None => bands
            .iter_mut()
            .enumerate()
            .try_for_each(|(bi, band)| run_band(plan, g, limits, src, band, bi)),
        Some(pool) => pool.install(|| {
            bands
                .par_iter_mut()
                .enumerate()
                .try_for_each(|(bi, band)| run_band(plan, g, limits, src, band, bi))
        }),
    }
}

/// log2 of a destination plane's vertical decimation.
fn plane_vsub(plan: &Plan, plane: usize) -> u8 {
    let full = plan.dst.format.plane_height(plan.dst.height, plane as u8);
    if full < plan.dst.height {
        plan.dst_layout.log2_h
    } else {
        0
    }
}

/// Destination rows of the band, for channel `c`.
fn band_range(plan: &Plan, c: usize, y0: u32, y1: u32) -> (usize, usize) {
    let s = if c == 1 || c == 2 {
        plan.dst_layout.log2_h
    } else {
        0
    };
    (ceil_shr(y0, s) as usize, ceil_shr(y1, s) as usize)
}

#[allow(
    clippy::too_many_lines,
    reason = "the band pipeline reads as one sequence; splitting it hides the data flow"
)]
fn run_band(
    plan: &Plan,
    g: &General,
    limits: &Limits,
    src: &[SrcPlane<'_>],
    band: &mut BandDst<'_>,
    band_index: usize,
) -> Result<()> {
    let mut budget = Budget::new(limits.clone());
    let band_rows = plan.band_rows.max(1);
    let y0 = (band_index as u32).saturating_mul(band_rows);
    if y0 >= plan.dst.height {
        return Ok(());
    }
    let y1 = y0.saturating_add(band_rows).min(plan.dst.height);

    // 1. Destination rows, then the mid rows they need, then their union.
    let mut d_range = [(0usize, 0usize); MAX_COMPS];
    let mut m_range = [(0usize, 0usize); MAX_COMPS];
    let mut union = (usize::MAX, 0usize);
    for c in 0..g.live {
        let Some(p) = g.ch.get(c) else { continue };
        if !p.written {
            continue;
        }
        let (d0, d1) = band_range(plan, c, y0, y1);
        let cap = p.dst.1 as usize;
        let (d0, d1) = (d0.min(cap), d1.min(cap));
        if let Some(slot) = d_range.get_mut(c) {
            *slot = (d0, d1);
        }
        let (m0, m1) = match p.down.v.as_deref() {
            Some(b) => source_span(b, d0, d1),
            None => (d0, d1),
        };
        if let Some(slot) = m_range.get_mut(c) {
            *slot = (m0, m1);
        }
        union.0 = union.0.min(m0);
        union.1 = union.1.max(m1);
    }
    if union.0 == usize::MAX || union.1 <= union.0 {
        return Ok(());
    }
    if g.colour.needs_common_resolution() {
        for c in 0..g.live {
            if let Some(slot) = m_range.get_mut(c) {
                *slot = union;
            }
        }
    }

    // 2. Build each channel's mid rows.
    let mut mid: [Option<Grid>; MAX_COMPS] = [None, None, None, None];
    let mut acc: Vec<i64> = Vec::new();
    for c in 0..g.live {
        let Some(p) = g.ch.get(c) else { continue };
        let (m0, m1) = m_range.get(c).copied().unwrap_or((0, 0));
        if m1 <= m0 {
            continue;
        }
        let grid = build_mid(plan, g, p, c, &mut budget, src, m0, m1, &mut acc)?;
        if let Some(slot) = mid.get_mut(c) {
            *slot = Some(grid);
        }
    }

    // 3. Colour, in place, across channels. Destructuring the array is what
    //    gives three disjoint `&mut` without any runtime aliasing check.
    if let ColorStage::Affine(a) = &g.colour {
        let [m0, m1, m2, _] = &mut mid;
        if let (Some(g0), Some(g1), Some(g2)) = (m0.as_mut(), m1.as_mut(), m2.as_mut()) {
            for y in union.0..union.1 {
                let (Some(r0), Some(r1), Some(r2)) = (g0.row_mut(y), g1.row_mut(y), g2.row_mut(y))
                else {
                    continue;
                };
                (g.kernels.affine_row)(a, r0, r1, r2);
            }
        }
    }
    if let ColorStage::Float(a) = &g.colour {
        let [m0, m1, m2, _] = &mut mid;
        if let (Some(g0), Some(g1), Some(g2)) = (m0.as_mut(), m1.as_mut(), m2.as_mut()) {
            for y in union.0..union.1 {
                let (Some(r0), Some(r1), Some(r2)) = (g0.row_mut(y), g1.row_mut(y), g2.row_mut(y))
                else {
                    continue;
                };
                apply_float(a, r0, r1, r2);
            }
        }
    }

    // 4. Down-resample, quantise, pack.
    for c in 0..g.live {
        let Some(p) = g.ch.get(c) else { continue };
        if !p.written {
            continue;
        }
        let (d0, d1) = d_range.get(c).copied().unwrap_or((0, 0));
        if d1 <= d0 {
            continue;
        }
        let Some(m) = mid.get(c).and_then(Option::as_ref) else {
            continue;
        };
        let Some(comp) = plan.dst_layout.comp(c) else {
            continue;
        };
        let Some(Some(pb)) = band.planes.get_mut(usize::from(comp.plane)) else {
            continue;
        };

        let mut vrow = budget.alloc::<i32>(p.mid.0 as usize)?;
        let mut orow = budget.alloc::<i32>(p.dst.0 as usize)?;
        let extra = if arithmetic(p.down.h.as_deref()) && arithmetic(p.down.v.as_deref()) {
            EXTRA_BITS
        } else {
            0
        };
        let vshift = COEFF_SHIFT - extra;
        let hshift = COEFF_SHIFT + extra;
        let sh = g.work_depth.saturating_sub(p.dst_depth);
        for d in d0..d1 {
            match p.down.v.as_deref() {
                Some(b) => filter_v(b, m, d, &mut vrow, &mut acc, vshift),
                None => {
                    if let Some(r) = m.row(d) {
                        copy_row(r, &mut vrow);
                    }
                }
            }
            match p.down.h.as_deref() {
                Some(b) => filter_h(b, &vrow, &mut orow, hshift),
                None => copy_row(&vrow, &mut orow),
            }
            quantise_row(&mut orow, sh, g.work_depth, p.dst_depth, g.dither, d);
            let local = d.saturating_sub(pb.first_row);
            let start = local.saturating_mul(pb.stride);
            let Some(row) = pb.data.get_mut(start..) else {
                continue;
            };
            write_row(row, comp, &orow);
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    clippy::many_single_char_names,
    reason = "one channel's up pass; every argument is a distinct part of it"
)]
fn build_mid(
    plan: &Plan,
    g: &General,
    p: &ChannelPlan,
    c: usize,
    budget: &mut Budget,
    src: &[SrcPlane<'_>],
    m0: usize,
    m1: usize,
    acc: &mut Vec<i64>,
) -> Result<Grid> {
    let mid_w = p.mid.0 as usize;
    // Keep EXTRA fractional bits between a horizontal and a vertical pass:
    // rounding to integer code values in between throws away a third of the
    // filter's accuracy, and it is the difference between "close" and "1 LSB"
    // on a bicubic resize. Costs nothing — the accumulator is `i64` either way.
    let extra = if arithmetic(p.up.h.as_deref()) && arithmetic(p.up.v.as_deref()) {
        EXTRA_BITS
    } else {
        0
    };
    let hshift = COEFF_SHIFT - extra;
    let vshift = COEFF_SHIFT + extra;

    if let Synthetic::Constant(v) = p.synthetic {
        let mut grid = Grid::new(budget, mid_w, m0, m1.saturating_sub(m0))?;
        grid.data.fill(v);
        return Ok(grid);
    }

    let Some(comp) = plan.src_layout.comp(c) else {
        return Grid::new(budget, mid_w, m0, m1.saturating_sub(m0));
    };
    let Some(plane) = src.get(usize::from(comp.plane)) else {
        return Err(Error::InvalidData("source plane index out of range"));
    };

    let (s0, s1) = if let Some(b) = p.up.v.as_deref() {
        source_span(b, m0, m1)
    } else {
        let cap = p.src.1 as usize;
        (m0.min(cap), m1.min(cap))
    };
    let mut h = Grid::new(budget, mid_w, s0, s1.saturating_sub(s0))?;
    let mut raw = budget.alloc::<i32>(p.src.0 as usize)?;
    for y in s0..s1 {
        let start = y.saturating_mul(plane.stride);
        let row = plane.data.get(start..).unwrap_or(&[]);
        let n = read_row(row, comp, &mut raw);
        for v in raw.iter_mut().take(n) {
            *v = expand_depth(*v, p.src_depth, g.work_depth);
        }
        // A truncated source row repeats its last sample, which is the same edge
        // rule the coefficient banks use.
        if let Some(&last) = raw.get(n.wrapping_sub(1)) {
            for v in raw.iter_mut().skip(n) {
                *v = last;
            }
        }
        let Some(out) = h.row_mut(y) else { continue };
        if let Some(b) = p.up.h.as_deref() {
            filter_h(b, &raw, out, hshift);
        } else {
            copy_row(&raw, out);
        }
    }

    // With no vertical bank the horizontal result already *is* the mid grid, so
    // there is nothing to copy it into.
    let Some(bank) = p.up.v.as_deref() else {
        return Ok(h);
    };
    let mut grid = Grid::new(budget, mid_w, m0, m1.saturating_sub(m0))?;
    for y in m0..m1 {
        let Some(out) = grid.row_mut(y) else { continue };
        filter_v(bank, &h, y, out, acc, vshift);
    }
    Ok(grid)
}

/// The colour matrix on three rows at once.
///
/// The scalar reference: `Affine::apply` is the definition and this is the row
/// form of it, so a SIMD kernel has something to be differentially tested
/// against without reimplementing the semantics.
#[inline]
pub(crate) fn apply_affine(a: &Affine, r0: &mut [i32], r1: &mut [i32], r2: &mut [i32]) {
    for ((v0, v1), v2) in r0.iter_mut().zip(r1.iter_mut()).zip(r2.iter_mut()) {
        let [o0, o1, o2] = a.apply([*v0, *v1, *v2]);
        *v0 = o0;
        *v1 = o1;
        *v2 = o2;
    }
}

/// Apply the nonlinear transfer/primaries path on three logical channels.
#[inline]
pub(crate) fn apply_float(a: &FloatTransform, r0: &mut [i32], r1: &mut [i32], r2: &mut [i32]) {
    for ((v0, v1), v2) in r0.iter_mut().zip(r1.iter_mut()).zip(r2.iter_mut()) {
        let [o0, o1, o2] = a.apply([*v0, *v1, *v2]);
        *v0 = o0;
        *v1 = o1;
        *v2 = o2;
    }
}

fn copy_row(src: &[i32], dst: &mut [i32]) {
    let n = src.len().min(dst.len());
    if let (Some(s), Some(d)) = (src.get(..n), dst.get_mut(..n)) {
        d.copy_from_slice(s);
    }
    if let Some(&last) = src.get(n.wrapping_sub(1)) {
        for v in dst.iter_mut().skip(n) {
            *v = last;
        }
    }
}

fn quantise_row(row: &mut [i32], shift: u8, from: u8, to: u8, dither: DitherKind, y: usize) {
    if shift == 0 {
        let max = (1i32 << to) - 1;
        for v in row.iter_mut() {
            *v = (*v).clamp(0, max);
        }
        return;
    }
    match dither {
        DitherKind::Bayer => {
            for (x, v) in row.iter_mut().enumerate() {
                *v = reduce_depth_dithered(*v, from, to, bayer_threshold(x, y, shift));
            }
        }
        DitherKind::None | DitherKind::Auto => {
            for v in row.iter_mut() {
                *v = reduce_depth(*v, from, to);
            }
        }
    }
}

/// Whether a bank does arithmetic rather than being a pure gather.
fn arithmetic(bank: Option<&FilterBank>) -> bool {
    bank.is_some_and(|b| !b.gather)
}

/// Horizontal filter: one input row to one output row.
///
/// Dispatches to a fixed-tap-count body for the common kernel widths
/// (bilinear's 2, an unscaled cubic's 4, an unscaled `a=3` lanczos's 6, a 2x
/// bicubic downscale's 8 — see `docs/signal/vaco-scale.md` §8's profiling
/// note) and falls back to [`filter_h_generic`] — the scalar reference every
/// one of these must agree with — for any other tap count. `bank.taps` is a
/// runtime field, so the un-dispatched loop's trip count is invisible to the
/// optimiser at compile time; converting the coefficient and window slices to
/// `&[i32; N]` before the inner loop gives it back, which is what lets the
/// tap loop unroll instead of paying `Iterator`/bounds-check overhead on every
/// one of a handful of taps. Measured in `docs/signal/vaco-scale.md` §8.
#[inline]
pub(crate) fn filter_h(bank: &FilterBank, src: &[i32], dst: &mut [i32], shift: u8) {
    if bank.gather {
        for (d, out) in dst.iter_mut().enumerate().take(bank.dst_len) {
            let Some(&off) = bank.offsets.get(d) else {
                break;
            };
            let Some(&v) = src.get(off as usize) else {
                break;
            };
            *out = v;
        }
        return;
    }
    let round = 1i64 << (shift - 1);
    match bank.taps {
        2 => filter_h_fixed::<2>(bank, src, dst, round, shift),
        4 => filter_h_fixed::<4>(bank, src, dst, round, shift),
        6 => filter_h_fixed::<6>(bank, src, dst, round, shift),
        8 => filter_h_fixed::<8>(bank, src, dst, round, shift),
        _ => filter_h_generic(bank, src, dst, round, shift),
    }
}

/// The scalar reference: a plain loop over `bank.taps` elements, whatever
/// that count is. [`filter_h_fixed`] must agree with this for every tap
/// count, and `kernels_agree`-style tests in this module pin that.
fn filter_h_generic(bank: &FilterBank, src: &[i32], dst: &mut [i32], round: i64, shift: u8) {
    let taps = bank.taps;
    for (d, out) in dst.iter_mut().enumerate().take(bank.dst_len) {
        let (Some(&off), Some(base)) = (bank.offsets.get(d), d.checked_mul(taps)) else {
            break;
        };
        let off = off as usize;
        let (Some(coeffs), Some(window)) = (
            bank.coeffs.get(base..base.saturating_add(taps)),
            src.get(off..off.saturating_add(taps)),
        ) else {
            break;
        };
        let mut acc = round;
        for (c, s) in coeffs.iter().zip(window.iter()) {
            acc += i64::from(*c) * i64::from(*s);
        }
        *out = (acc >> shift) as i32;
    }
}

/// Same computation as [`filter_h_generic`], specialised to a compile-time
/// tap count. Converting the two `bank.taps`-wide slices to `&[i32; N]` (one
/// bounds check, via `try_into`) rather than indexing them for `N` more
/// iterations is what carries the constant trip count into the accumulation
/// loop below.
#[inline(always)]
#[allow(
    clippy::inline_always,
    reason = "the constant N must reach codegen at every call site for the tap loop below to unroll; a normal #[inline] leaves that to the optimiser's discretion"
)]
fn filter_h_fixed<const N: usize>(
    bank: &FilterBank,
    src: &[i32],
    dst: &mut [i32],
    round: i64,
    shift: u8,
) {
    for (d, out) in dst.iter_mut().enumerate().take(bank.dst_len) {
        let (Some(&off), Some(base)) = (bank.offsets.get(d), d.checked_mul(N)) else {
            break;
        };
        let off = off as usize;
        let (Some(coeffs), Some(window)) = (
            bank.coeffs.get(base..base.saturating_add(N)),
            src.get(off..off.saturating_add(N)),
        ) else {
            break;
        };
        let (Ok(coeffs), Ok(window)): (Result<&[i32; N], _>, Result<&[i32; N], _>) =
            (coeffs.try_into(), window.try_into())
        else {
            break;
        };
        let mut acc = round;
        for (c, s) in coeffs.iter().zip(window.iter()) {
            acc += i64::from(*c) * i64::from(*s);
        }
        *out = (acc >> shift) as i32;
    }
}

/// Vertical filter: a window of input rows to one output row. Common tap
/// counts use an output-major fixed-width dot product; unusual widths and
/// incomplete source windows retain [`filter_v_generic`]'s exact behaviour.
#[inline]
pub(crate) fn filter_v(
    bank: &FilterBank,
    src: &Grid,
    d: usize,
    dst: &mut [i32],
    acc: &mut Vec<i64>,
    shift: u8,
) {
    if !bank.gather {
        let complete = match bank.taps {
            2 => filter_v_fixed::<2>(bank, src, d, dst, shift),
            4 => filter_v_fixed::<4>(bank, src, d, dst, shift),
            6 => filter_v_fixed::<6>(bank, src, d, dst, shift),
            8 => filter_v_fixed::<8>(bank, src, d, dst, shift),
            _ => false,
        };
        if complete {
            return;
        }
    }
    filter_v_generic(bank, src, d, dst, acc, shift);
}

/// Tap-major scalar reference. `acc` is the caller's scratch so this fallback
/// allocates at most once per band even when its tap count is not specialised.
fn filter_v_generic(
    bank: &FilterBank,
    src: &Grid,
    d: usize,
    dst: &mut [i32],
    acc: &mut Vec<i64>,
    shift: u8,
) {
    if bank.gather {
        let Some(&off) = bank.offsets.get(d) else {
            return;
        };
        if let Some(row) = src.row(off as usize) {
            copy_row(row, dst);
        }
        return;
    }
    let taps = bank.taps;
    let round = 1i64 << (shift - 1);
    let (Some(&off), Some(base)) = (bank.offsets.get(d), d.checked_mul(taps)) else {
        return;
    };
    let Some(coeffs) = bank.coeffs.get(base..base.saturating_add(taps)) else {
        return;
    };
    let n = dst.len().min(src.w);
    acc.clear();
    acc.resize(n, round);
    for (t, &c) in coeffs.iter().enumerate() {
        let Some(row) = src.row((off as usize).saturating_add(t)) else {
            continue;
        };
        let c = i64::from(c);
        for (a, s) in acc.iter_mut().zip(row.iter()) {
            *a += c * i64::from(*s);
        }
    }
    for (o, a) in dst.iter_mut().zip(acc.iter()) {
        *o = (*a >> shift) as i32;
    }
}

/// Output-major vertical dot product. All row bounds are proved before the
/// output loop, so the hot path reads each destination accumulator only in a
/// register instead of round-tripping an `i64` scratch row once per tap.
#[inline(always)]
#[allow(
    clippy::indexing_slicing,
    clippy::inline_always,
    reason = "every row is sliced to n before x iterates over 0..n; N must reach codegen so the dot product unrolls"
)]
fn filter_v_fixed<const N: usize>(
    bank: &FilterBank,
    src: &Grid,
    d: usize,
    dst: &mut [i32],
    shift: u8,
) -> bool {
    let (Some(&off), Some(base)) = (bank.offsets.get(d), d.checked_mul(N)) else {
        return false;
    };
    let Some(coeffs) = bank.coeffs.get(base..base.saturating_add(N)) else {
        return false;
    };
    let Ok(coeffs): Result<&[i32; N], _> = coeffs.try_into() else {
        return false;
    };
    let n = dst.len().min(src.w);
    let mut rows = [&[][..]; N];
    for (t, slot) in rows.iter_mut().enumerate() {
        let Some(row) = src
            .row((off as usize).saturating_add(t))
            .and_then(|row| row.get(..n))
        else {
            return false;
        };
        *slot = row;
    }

    let round = 1i64 << (shift - 1);
    for (x, out) in dst.iter_mut().take(n).enumerate() {
        let mut sum = round;
        for (&coefficient, row) in coeffs.iter().zip(rows.iter()) {
            sum += i64::from(coefficient) * i64::from(row[x]);
        }
        *out = (sum >> shift) as i32;
    }
    true
}

/// Tooling-only access to the private vertical-filter implementations.
#[cfg(feature = "checkasm")]
#[doc(hidden)]
pub mod checkasm {
    use super::{Grid, filter_v_fixed, filter_v_generic};
    use crate::filter::{COEFF_ONE, COEFF_SHIFT, FilterBank, FilterSpec, Kernel};
    use vaco_limits::{Budget, Limits};

    /// Opaque, deterministic input for comparing the generic and fixed
    /// vertical-filter implementations.
    #[derive(Debug, Clone)]
    pub struct FilterVCase {
        bank: FilterBank,
        src: Grid,
        shift: u8,
        output_len: usize,
    }

    impl FilterVCase {
        /// Build a valid non-gather bank and matching source grid.
        ///
        /// Returns `None` for unsupported tap counts or dimensions whose
        /// storage arithmetic does not fit.
        #[must_use]
        pub fn synthetic(taps: usize, width: usize, dst_rows: usize) -> Option<Self> {
            let kernel = match taps {
                2 => Kernel::Bilinear,
                4 => Kernel::Bicubic { b: 0.0, c: 0.6 },
                6 => Kernel::Lanczos { a: 3.0 },
                8 => Kernel::Lanczos { a: 4.0 },
                _ => return None,
            };
            let src_rows = dst_rows.checked_add(taps.checked_sub(1)?)?;
            let sample_count = width.checked_mul(src_rows)?;
            let output_len = width.checked_mul(dst_rows)?.checked_add(1)?;
            let coefficient_count = dst_rows.checked_mul(taps)?;

            let mut budget = Budget::new(Limits::strict());
            let mut offsets = budget.alloc::<u32>(dst_rows).ok()?;
            for (d, offset) in offsets.iter_mut().enumerate() {
                *offset = u32::try_from(d).ok()?;
            }
            let mut coeffs = budget.alloc::<i32>(coefficient_count).ok()?;
            let base = COEFF_ONE.div_euclid(i32::try_from(taps).ok()?);
            for (d, row) in coeffs.chunks_exact_mut(taps).enumerate() {
                let (last, prefix) = row.split_last_mut()?;
                let mut sum = 0i32;
                for (t, coefficient) in prefix.iter_mut().enumerate() {
                    let phase = d.wrapping_mul(11).wrapping_add(t.wrapping_mul(7)) % 9;
                    let jitter = i32::try_from(phase).ok()?.checked_sub(4)?;
                    *coefficient = base.checked_add(jitter)?;
                    sum = sum.checked_add(*coefficient)?;
                }
                *last = COEFF_ONE.checked_sub(sum)?;
            }
            let abs_sum = coeffs
                .chunks(taps)
                .map(|row| row.iter().map(|&c| i64::from(c).abs()).sum::<i64>())
                .max()
                .unwrap_or(0);
            let mut data = budget.alloc::<i32>(sample_count).ok()?;
            if width > 0 {
                for (y, row) in data.chunks_exact_mut(width).enumerate() {
                    for (x, sample) in row.iter_mut().enumerate() {
                        let patterned = x.wrapping_mul(131).wrapping_add(y.wrapping_mul(17)) % 4096;
                        *sample = i32::try_from(patterned).ok()?.checked_sub(2048)?;
                    }
                }
            }

            Some(Self {
                bank: FilterBank {
                    src_len: src_rows,
                    dst_len: dst_rows,
                    taps,
                    offsets,
                    coeffs,
                    gather: false,
                    abs_sum,
                    spec: FilterSpec {
                        kernel,
                        src_len: src_rows,
                        dst_len: dst_rows,
                        phase_src: 0.0,
                        phase_dst: 0.0,
                        max_taps: taps,
                    },
                },
                src: Grid {
                    w: width,
                    y0: 0,
                    rows: src_rows,
                    data,
                },
                shift: COEFF_SHIFT,
                output_len,
            })
        }

        /// Fixed tap count carried by this case.
        #[must_use]
        pub fn taps(&self) -> usize {
            self.bank.taps
        }

        /// Number of comparable lanes returned by either runner.
        #[must_use]
        pub fn output_len(&self) -> usize {
            self.output_len
        }
    }

    /// Run the private generic production callee over every destination row.
    #[must_use]
    pub fn run_generic(case: &FilterVCase) -> Vec<i32> {
        let Some((mut output, mut scratch)) = runner_buffers(case) else {
            return Vec::new();
        };
        if let Some(completion) = output.first_mut() {
            *completion = 1;
        }
        for d in 0..case.bank.dst_len {
            let start = 1usize.saturating_add(d.saturating_mul(case.src.w));
            let end = start.saturating_add(case.src.w);
            if let Some(dst) = output.get_mut(start..end) {
                filter_v_generic(&case.bank, &case.src, d, dst, &mut scratch, case.shift);
            }
        }
        output
    }

    /// Run the matching private fixed-width production callee directly over
    /// every destination row.
    #[must_use]
    pub fn run_fixed(case: &FilterVCase) -> Vec<i32> {
        let Some((mut output, scratch)) = runner_buffers(case) else {
            return Vec::new();
        };
        std::hint::black_box(&scratch);
        let mut complete = true;
        for d in 0..case.bank.dst_len {
            let start = 1usize.saturating_add(d.saturating_mul(case.src.w));
            let end = start.saturating_add(case.src.w);
            let row_complete = output
                .get_mut(start..end)
                .is_some_and(|dst| match case.bank.taps {
                    2 => filter_v_fixed::<2>(&case.bank, &case.src, d, dst, case.shift),
                    4 => filter_v_fixed::<4>(&case.bank, &case.src, d, dst, case.shift),
                    6 => filter_v_fixed::<6>(&case.bank, &case.src, d, dst, case.shift),
                    8 => filter_v_fixed::<8>(&case.bank, &case.src, d, dst, case.shift),
                    _ => false,
                });
            complete &= row_complete;
        }
        if let Some(completion) = output.first_mut() {
            *completion = i32::from(complete);
        }
        output
    }

    fn runner_buffers(case: &FilterVCase) -> Option<(Vec<i32>, Vec<i64>)> {
        let mut budget = Budget::new(Limits::strict());
        let output = budget.alloc::<i32>(case.output_len).ok()?;
        let scratch = budget.alloc::<i64>(case.src.w).ok()?;
        Some((output, scratch))
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::many_single_char_names,
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::unreadable_literal,
    reason = "a failing assertion in a test is a failing test"
)]
mod filter_h_fixed_tests {
    use super::*;
    use crate::filter::{FilterBank, FilterSpec, Kernel};

    /// A deterministic, non-normalised bank: `filter_h_fixed`'s only job is
    /// to compute the same sum as `filter_h_generic`, not to be a valid
    /// resampling kernel, so its coefficients need not sum to `COEFF_ONE`.
    fn synthetic_bank(taps: usize, dst_len: usize, src_len: usize, seed: u32) -> FilterBank {
        let next = |i: u32| (i.wrapping_mul(2_654_435_761).wrapping_add(seed)) as i32;
        let max_off = src_len.saturating_sub(taps);
        let offsets: Vec<u32> = (0..dst_len)
            .map(|d| {
                let raw = if max_off == 0 {
                    0
                } else {
                    (d * 3 + seed as usize) % (max_off + 1)
                };
                raw as u32
            })
            .collect();
        let coeffs: Vec<i32> = (0..dst_len * taps)
            .map(|i| (next(i as u32) % 4096) - 2048)
            .collect();
        FilterBank {
            src_len,
            dst_len,
            taps,
            offsets,
            coeffs,
            gather: false,
            abs_sum: i64::from(i32::MAX),
            spec: FilterSpec {
                kernel: Kernel::Point,
                src_len,
                dst_len,
                phase_src: 0.0,
                phase_dst: 0.0,
                max_taps: taps,
            },
        }
    }

    #[test]
    fn fixed_and_generic_agree_at_every_tap_count_and_length() {
        let shift = COEFF_SHIFT;
        for &taps in &[1usize, 2, 3, 4, 5, 6, 7, 8, 9] {
            for src_len in [taps, taps + 1, taps + 7, taps * 4] {
                for dst_len in [0usize, 1, 2, 3, 7, 16, 17] {
                    let bank = synthetic_bank(taps, dst_len, src_len, taps as u32 * 7 + 1);
                    let src: Vec<i32> = (0..src_len).map(|i| (i as i32) * 3 - 5).collect();
                    let round = 1i64 << (shift - 1);

                    let mut generic_out = vec![0i32; dst_len];
                    filter_h_generic(&bank, &src, &mut generic_out, round, shift);

                    let mut dispatched_out = vec![0i32; dst_len];
                    match taps {
                        2 => filter_h_fixed::<2>(&bank, &src, &mut dispatched_out, round, shift),
                        4 => filter_h_fixed::<4>(&bank, &src, &mut dispatched_out, round, shift),
                        6 => filter_h_fixed::<6>(&bank, &src, &mut dispatched_out, round, shift),
                        8 => filter_h_fixed::<8>(&bank, &src, &mut dispatched_out, round, shift),
                        _ => filter_h_generic(&bank, &src, &mut dispatched_out, round, shift),
                    }

                    assert_eq!(
                        generic_out, dispatched_out,
                        "taps={taps} src_len={src_len} dst_len={dst_len}"
                    );

                    // The real dispatcher (`filter_h`) must reach the same
                    // fixed body for the widths it claims to specialise.
                    let mut via_filter_h = vec![0i32; dst_len];
                    filter_h(&bank, &src, &mut via_filter_h, shift);
                    assert_eq!(
                        generic_out, via_filter_h,
                        "filter_h taps={taps} src_len={src_len} dst_len={dst_len}"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    reason = "test code over bounded synthetic grids"
)]
mod filter_v_fixed_tests {
    use super::*;
    use crate::filter::{FilterBank, FilterSpec, Kernel};

    fn synthetic_bank(taps: usize, dst_len: usize, y0: usize, rows: usize) -> FilterBank {
        let max_offset = rows.saturating_sub(taps);
        let offsets = (0..dst_len)
            .map(|d| y0.saturating_add(d.min(max_offset)) as u32)
            .collect();
        let coeffs = (0..dst_len.saturating_mul(taps))
            .map(|i| ((i as i32).wrapping_mul(977) & 4095) - 2048)
            .collect();
        FilterBank {
            src_len: rows,
            dst_len,
            taps,
            offsets,
            coeffs,
            gather: false,
            abs_sum: i64::from(i32::MAX),
            spec: FilterSpec {
                kernel: Kernel::Point,
                src_len: rows,
                dst_len,
                phase_src: 0.0,
                phase_dst: 0.0,
                max_taps: taps,
            },
        }
    }

    fn synthetic_grid(w: usize, y0: usize, rows: usize) -> Grid {
        Grid {
            w,
            y0,
            rows,
            data: (0..w.saturating_mul(rows))
                .map(|i| (i as i32).wrapping_mul(31).wrapping_sub(4000))
                .collect(),
        }
    }

    #[test]
    fn fixed_and_generic_vertical_filters_agree_across_shapes_and_missing_rows() {
        let shift = COEFF_SHIFT;
        for &taps in &[1usize, 2, 3, 4, 5, 6, 7, 8, 9] {
            for width in [0usize, 1, 3, 16, 17, 63] {
                for rows in [taps.saturating_sub(1), taps, taps + 3] {
                    let y0 = 5;
                    let dst_rows = 3;
                    let bank = synthetic_bank(taps, dst_rows, y0, rows);
                    let grid = synthetic_grid(width, y0, rows);
                    for d in 0..dst_rows {
                        for dst_len in [0usize, width.saturating_sub(1), width, width + 3] {
                            let mut generic_out = vec![17; dst_len];
                            let mut generic_acc = Vec::new();
                            filter_v_generic(
                                &bank,
                                &grid,
                                d,
                                &mut generic_out,
                                &mut generic_acc,
                                shift,
                            );

                            let mut dispatched_out = vec![17; dst_len];
                            let mut dispatched_acc = Vec::new();
                            filter_v(
                                &bank,
                                &grid,
                                d,
                                &mut dispatched_out,
                                &mut dispatched_acc,
                                shift,
                            );
                            assert_eq!(
                                generic_out, dispatched_out,
                                "taps={taps} width={width} rows={rows} d={d} dst_len={dst_len}"
                            );

                            if matches!(taps, 2 | 4 | 6 | 8) {
                                let mut fixed_out = vec![17; dst_len];
                                let complete = match taps {
                                    2 => {
                                        filter_v_fixed::<2>(&bank, &grid, d, &mut fixed_out, shift)
                                    }
                                    4 => {
                                        filter_v_fixed::<4>(&bank, &grid, d, &mut fixed_out, shift)
                                    }
                                    6 => {
                                        filter_v_fixed::<6>(&bank, &grid, d, &mut fixed_out, shift)
                                    }
                                    8 => {
                                        filter_v_fixed::<8>(&bank, &grid, d, &mut fixed_out, shift)
                                    }
                                    _ => unreachable!(),
                                };
                                assert_eq!(
                                    complete,
                                    rows >= taps,
                                    "taps={taps} width={width} rows={rows} d={d} dst_len={dst_len}"
                                );
                                if complete {
                                    assert_eq!(
                                        generic_out, fixed_out,
                                        "direct fixed path: taps={taps} width={width} rows={rows} d={d} dst_len={dst_len}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
