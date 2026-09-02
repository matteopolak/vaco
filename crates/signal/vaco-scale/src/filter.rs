//! Resampling kernels and the coefficient banks built from them.
//!
//! There are **no coefficient tables in this file**. A kernel is a formula and a
//! support radius; a bank is what the generator produces for one
//! `(kernel, src_len, dst_len, phase)` tuple. That is both a clean-room
//! requirement (a table is authorial, a formula is mathematics — D7/D15) and the
//! only way the error target is auditable.
//!
//! # The generator, in five steps
//!
//! For destination sample `d` of `dst_len`, over a source of `src_len`:
//!
//! 1. **Centre.** `centre = (d + 0.5 + p_dst)·ratio − 0.5 − p_src`, with
//!    `ratio = src_len / dst_len`. The `p` terms are chroma-siting phases in
//!    *component sample* units; both are zero by default, which is what the
//!    reference does (see `docs/signal/vaco-scale.md`, "chroma siting").
//! 2. **Stretch when downscaling.** `xscale = min(1, dst_len/src_len)`, and the
//!    kernel is evaluated at `x·xscale` over a radius of `support/xscale`. Without
//!    this a downscale filters at the *source* Nyquist and aliases, which is the
//!    single most common way to get resampling visibly wrong.
//! 3. **Edge replication.** Taps landing outside `0..src_len` fold their weight
//!    onto the boundary sample. Chosen over zero-padding (darkens edges) and
//!    reflection (mirrors detail).
//! 4. **Uniform tap count.** Every destination position reads the same number of
//!    source samples, which is what makes the inner loop a fixed trip count and
//!    therefore vectorisable at all.
//! 5. **Normalise, then quantise.** Weights sum to exactly 1, and their 14-bit
//!    fixed-point images sum to exactly `1 << 14` — the residual goes to the
//!    largest-magnitude tap. That exactness is what makes a constant image
//!    survive scaling unchanged, which is the crate's single most valuable
//!    property test.
//!
//! # Which bicubic
//!
//! `Kernel::Bicubic` is Mitchell–Netravali with `(B, C)`. The default is
//! **`(0, 0.6)`**, not Catmull–Rom `(0, 0.5)`: `(0, 0.6)` is what the reference
//! binary measurably uses, recovered by scaling an impulse to 16 bits and
//! solving for `(B, C)` from four taps. Plan 17 §A.7.1 says Catmull–Rom and is
//! wrong. The probe is recorded in the crate doc file.

use vaco_core::{Error, Result};
use vaco_limits::Budget;

/// Fixed-point shift for bank coefficients. Fourteen bits leaves an `i32`
/// accumulator room for 8-bit input at up to 16 taps, and the generic path
/// accumulates in `i64` so deeper formats cost accuracy nowhere.
pub const COEFF_SHIFT: u8 = 14;
/// `1 << COEFF_SHIFT`.
pub const COEFF_ONE: i32 = 1 << COEFF_SHIFT;

/// Default cap on the tap count. Above it the kernel is narrowed rather than
/// two-stage decimation being used; see the crate docs for the deferral.
pub const DEFAULT_MAX_TAPS: usize = 64;

/// A continuous reconstruction kernel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kernel {
    /// Nearest neighbour.
    Point,
    /// Triangle, support 1.
    Bilinear,
    /// Mitchell–Netravali cubic with parameters `(b, c)`.
    Bicubic { b: f64, c: f64 },
    /// `sinc(x)·sinc(x/a)`, support `a`.
    Lanczos { a: f64 },
    /// `exp(-x²/2σ²)`, support `3σ`.
    Gaussian { sigma: f64 },
    /// Box of width `max(1, src/dst)`; an exact area average when downscaling.
    Area,
}

impl Kernel {
    /// The default bicubic, as measured against the reference.
    #[must_use]
    pub const fn bicubic_default() -> Self {
        Self::Bicubic { b: 0.0, c: 0.6 }
    }

    /// Half-width of the kernel's support, in source samples at `xscale = 1`.
    #[must_use]
    pub fn support(self) -> f64 {
        match self {
            Self::Point | Self::Area => 0.5,
            Self::Bilinear => 1.0,
            Self::Bicubic { .. } => 2.0,
            Self::Lanczos { a } => a.max(1.0),
            Self::Gaussian { sigma } => (3.0 * sigma).max(1.0),
        }
    }

    /// Evaluate at distance `x` from the filter centre.
    #[must_use]
    pub fn eval(self, x: f64) -> f64 {
        let ax = x.abs();
        match self {
            // Half-open on purpose: a sample exactly half a step away is
            // included on one side only, so a tie never produces a filter with
            // no taps at all.
            Self::Point | Self::Area => {
                if (-0.5..0.5).contains(&x) {
                    1.0
                } else {
                    0.0
                }
            }
            Self::Bilinear => (1.0 - ax).max(0.0),
            Self::Bicubic { b, c } => mitchell(ax, b, c),
            Self::Lanczos { a } => {
                let a = a.max(1.0);
                if ax >= a {
                    0.0
                } else {
                    sinc(ax) * sinc(ax / a)
                }
            }
            Self::Gaussian { sigma } => {
                let s = sigma.max(1e-3);
                if ax >= 3.0 * s {
                    0.0
                } else {
                    (-(x * x) / (2.0 * s * s)).exp()
                }
            }
        }
    }
}

/// Mitchell & Netravali, SIGGRAPH 1988, "Reconstruction Filters in Computer
/// Graphics", equation (8). Written out rather than tabulated.
fn mitchell(ax: f64, b: f64, c: f64) -> f64 {
    if ax < 1.0 {
        let x2 = ax * ax;
        ((12.0 - 9.0 * b - 6.0 * c) * x2 * ax + (-18.0 + 12.0 * b + 6.0 * c) * x2 + (6.0 - 2.0 * b))
            / 6.0
    } else if ax < 2.0 {
        let x2 = ax * ax;
        ((-b - 6.0 * c) * x2 * ax
            + (6.0 * b + 30.0 * c) * x2
            + (-12.0 * b - 48.0 * c) * ax
            + (8.0 * b + 24.0 * c))
            / 6.0
    } else {
        0.0
    }
}

/// `sin(πx)/(πx)`, with the removable singularity filled in.
fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        let p = std::f64::consts::PI * x;
        p.sin() / p
    }
}

/// What a bank was built from. Kept so `explain()` and the tests can name it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FilterSpec {
    /// The kernel.
    pub kernel: Kernel,
    /// Source sample count along this axis.
    pub src_len: usize,
    /// Destination sample count along this axis.
    pub dst_len: usize,
    /// Source siting phase, in source-sample units.
    pub phase_src: f64,
    /// Destination siting phase, in destination-sample units.
    pub phase_dst: f64,
    /// Cap on the tap count.
    pub max_taps: usize,
}

/// One axis of resampling, precomputed.
///
/// `coeffs` is row-major: destination `d`'s tap `t` is `coeffs[d * taps + t]`,
/// applied to source sample `offsets[d] + t`. Every offset satisfies
/// `0 <= offsets[d]` and `offsets[d] + taps <= src_len`, so no execution path
/// needs a bounds check that could fail.
#[derive(Debug, Clone)]
pub struct FilterBank {
    /// Source sample count.
    pub src_len: usize,
    /// Destination sample count.
    pub dst_len: usize,
    /// Taps per destination sample; uniform by construction.
    pub taps: usize,
    /// First source index per destination sample. `dst_len` entries.
    pub offsets: Vec<u32>,
    /// `dst_len * taps` coefficients, summing to `1 << COEFF_SHIFT` per row.
    pub coeffs: Vec<i32>,
    /// Every row is a single unit tap, so applying the bank is a gather and not
    /// an arithmetic pass at all. Chroma replication produces one of these for
    /// every `Y'CbCr` to `R'G'B'` conversion, which is why it is worth a flag rather
    /// than a branch per pixel.
    pub gather: bool,
    /// The largest `Sum |c|` over all rows. An accumulator wider than
    /// `abs_sum x max_sample` cannot overflow, which is what lets the execution
    /// engine choose `i32` over `i64` provably rather than hopefully.
    pub abs_sum: i64,
    /// What produced this bank.
    pub spec: FilterSpec,
}

impl FilterBank {
    /// True when this bank copies its input unchanged.
    ///
    /// Checked structurally rather than by comparing the spec: a bilinear
    /// 64 -> 64 bank *is* an identity — its taps are `[0, 1, 0]` — and a spec
    /// comparison would not say so, leaving a three-tap filter in the chain for
    /// a conversion that resamples nothing.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        if self.src_len != self.dst_len {
            return false;
        }
        (0..self.dst_len).all(|d| {
            let (Some(&off), Some(row)) = (self.offsets.get(d), self.row(d)) else {
                return false;
            };
            row.iter().enumerate().all(|(t, &c)| {
                let is_centre = (off as usize).saturating_add(t) == d;
                c == if is_centre { COEFF_ONE } else { 0 }
            })
        })
    }

    /// The coefficients for destination sample `d`.
    #[must_use]
    pub fn row(&self, d: usize) -> Option<&[i32]> {
        let start = d.checked_mul(self.taps)?;
        self.coeffs.get(start..start.checked_add(self.taps)?)
    }
}

/// Build the bank described by `spec`.
///
/// # Errors
///
/// [`Error::InvalidData`] for a zero-length axis and [`Error::LimitExceeded`]
/// when the bank would not fit the budget.
pub fn build_bank(budget: &mut Budget, spec: &FilterSpec) -> Result<FilterBank> {
    // Nudge the support inwards so a tap whose weight is exactly zero at the
    // boundary — every kernel here has two of them — does not become a tap.
    const EPS: f64 = 1e-9;

    let (src_len, dst_len) = (spec.src_len, spec.dst_len);
    if src_len == 0 || dst_len == 0 {
        return Err(Error::InvalidData("resampling axis of length zero"));
    }
    let ratio = src_len as f64 / dst_len as f64;
    // Stretching a kernel's support by `1/xscale` on downscale is correct
    // band-limiting for every *smooth* kernel here, `Area`'s own box
    // included (its support is deliberately the destination footprint, so
    // it needs no special casing) -- but `Point` is not a band-limiting
    // kernel at all, it is a hard nearest-sample pick, and stretching its
    // already-minimal 0.5 support on downscale widens its window to cover
    // two source samples instead of one. Both then land inside `eval`'s
    // scaled `(-0.5, 0.5)` box and get equal weight 1.0, quantising to a
    // 50/50 blend rather than a single tap -- measured: an 8-wide ramp
    // downscaled 2:1 produced an average of samples 0 and 1 for output 0
    // instead of ffmpeg's own verified `2*d+1` nearest-sample rule. `Point`
    // keeps `xscale = 1` unconditionally so its support and its per-tap
    // `eval` distance below are never stretched by the decimation ratio.
    let xscale = if matches!(spec.kernel, Kernel::Point) {
        1.0
    } else if ratio > 1.0 {
        1.0 / ratio
    } else {
        1.0
    };
    let max_taps = spec.max_taps.clamp(1, 1024);
    let raw_radius = spec.kernel.support() / xscale;
    // Narrow the kernel rather than let the tap count explode. Two-stage
    // decimation (plan 17 §A.7.3) is the better answer and is deferred.
    let radius = raw_radius.min(((max_taps as f64) - 1.0) / 2.0).max(0.5);

    // Nudge the support inwards so a tap whose weight is exactly zero at the
    // boundary — every kernel here has two of them — does not become a tap.
    let centre = |d: usize| (d as f64 + 0.5 + spec.phase_dst) * ratio - 0.5 - spec.phase_src;
    let window = |c: f64| {
        let first = (c - radius + EPS).ceil();
        let last = (c + radius - EPS).floor();
        if last < first {
            let at = (c + 0.5).floor();
            (at, at)
        } else {
            (first, last)
        }
    };

    // Step 4: one tap count for every destination position.
    let mut taps = 1usize;
    for d in 0..dst_len {
        let c = centre(d);
        let (first, last) = window(c);
        let w = (last - first + 1.0).max(1.0);
        let w = if w.is_finite() { w as usize } else { 1 };
        taps = taps.max(w);
    }
    let mut taps = taps.clamp(1, max_taps).min(src_len);

    budget.check(
        (dst_len as u64)
            .saturating_mul(taps as u64)
            .saturating_mul(4),
    )?;
    let mut coeffs = budget.alloc::<i32>(dst_len.saturating_mul(taps))?;
    let mut offsets = budget.alloc::<u32>(dst_len)?;
    let mut acc = budget.alloc::<f64>(taps)?;

    for d in 0..dst_len {
        let c = centre(d);
        let (first_ideal, last_ideal) = window(c);
        let first_ideal = clamp_index(first_ideal, src_len);
        let last_ideal = clamp_index(last_ideal, src_len).max(first_ideal);
        // Window start, clamped so the whole uniform window is inside the row.
        let head = i64::try_from(src_len.saturating_sub(taps)).unwrap_or(i64::MAX);
        let base = usize::try_from(first_ideal.clamp(0, head)).unwrap_or(0);
        let base = base.min(src_len - taps);

        acc.fill(0.0);
        let mut i = first_ideal;
        while i <= last_ideal {
            let w = spec.kernel.eval(((i as f64) - c) * xscale);
            let last = i64::try_from(src_len.saturating_sub(1)).unwrap_or(i64::MAX);
            let idx = usize::try_from(i.clamp(0, last)).unwrap_or(0);
            let slot = idx.saturating_sub(base).min(taps - 1);
            if let Some(a) = acc.get_mut(slot) {
                *a += w;
            }
            i += 1;
        }
        quantise(&mut acc, &mut coeffs, d, taps);
        if let Some(o) = offsets.get_mut(d) {
            *o = base as u32;
        }
    }

    // Trim taps that are zero for *every* destination position. A bilinear
    // 64 -> 64 bank comes out of the generator with three taps of which two are
    // always zero; trimming turns it into the identity the optimiser can delete.
    let (lead, trail) = zero_margins(&coeffs, taps);
    if lead + trail > 0 && taps > lead + trail {
        let new_taps = taps - lead - trail;
        let mut packed = budget.alloc::<i32>(dst_len.saturating_mul(new_taps))?;
        for d in 0..dst_len {
            let from = d.saturating_mul(taps).saturating_add(lead);
            let to = d.saturating_mul(new_taps);
            let (Some(src), Some(dstr)) = (
                coeffs.get(from..from.saturating_add(new_taps)),
                packed.get_mut(to..to.saturating_add(new_taps)),
            ) else {
                continue;
            };
            dstr.copy_from_slice(src);
            if let Some(o) = offsets.get_mut(d) {
                *o = o.saturating_add(lead as u32);
            }
        }
        coeffs = packed;
        taps = new_taps;
    }

    let abs_sum = coeffs
        .chunks(taps.max(1))
        // `i64::from(c).abs()`, not `c.abs()`: a degenerate kernel can put
        // `i32::MIN` in a coefficient slot, and `i32::MIN.abs()` overflows.
        // Found by the fuzz target at exec 31.
        .map(|row| row.iter().map(|c| i64::from(*c).abs()).sum::<i64>())
        .max()
        .unwrap_or(i64::from(COEFF_ONE));

    let gather = taps == 1 && coeffs.iter().all(|&c| c == COEFF_ONE);

    Ok(FilterBank {
        src_len,
        dst_len,
        taps,
        offsets,
        coeffs,
        gather,
        abs_sum,
        spec: *spec,
    })
}

/// Compose a bank with a nearest-neighbour bank applied after it.
///
/// `point` maps `inner.dst_len` positions onto its own `dst_len`, one tap each,
/// so the composition is a permutation of `inner`'s rows: destination `d` takes
/// whichever row of `inner` the nearest-neighbour step selected. No coefficients
/// are combined and no tap count grows, so this is exact.
///
/// This is what "resample chroma to the destination's chroma grid, then
/// replicate it onto the luma grid" is, expressed as one bank — which is what
/// the reference does for every `Y'CbCr` to `R'G'B'` conversion (see
/// `docs/signal/vaco-scale.md`, "chroma upsampling").
///
/// # Errors
///
/// [`Error::InvalidData`] if the two banks do not compose, and
/// [`Error::LimitExceeded`] if the result does not fit the budget.
pub fn compose_after_point(
    budget: &mut Budget,
    inner: &FilterBank,
    point: &FilterBank,
) -> Result<FilterBank> {
    if point.taps != 1 || point.src_len != inner.dst_len {
        return Err(Error::InvalidData("filter banks do not compose"));
    }
    let dst_len = point.dst_len;
    let taps = inner.taps;
    let mut coeffs = budget.alloc::<i32>(dst_len.saturating_mul(taps))?;
    let mut offsets = budget.alloc::<u32>(dst_len)?;
    for d in 0..dst_len {
        let pick = point.offsets.get(d).copied().unwrap_or(0) as usize;
        let (Some(row), Some(off)) = (inner.row(pick), inner.offsets.get(pick)) else {
            continue;
        };
        let to = d.saturating_mul(taps);
        if let Some(slot) = coeffs.get_mut(to..to.saturating_add(taps)) {
            slot.copy_from_slice(row);
        }
        if let Some(slot) = offsets.get_mut(d) {
            *slot = *off;
        }
    }
    let gather = taps == 1 && coeffs.iter().all(|&c| c == COEFF_ONE);
    Ok(FilterBank {
        src_len: inner.src_len,
        dst_len,
        taps,
        offsets,
        coeffs,
        gather,
        abs_sum: inner.abs_sum,
        spec: inner.spec,
    })
}

/// Leading and trailing tap positions that are zero in every row.
fn zero_margins(coeffs: &[i32], taps: usize) -> (usize, usize) {
    if taps <= 1 {
        return (0, 0);
    }
    let mut lead = taps;
    let mut trail = taps;
    for row in coeffs.chunks(taps) {
        if row.len() < taps {
            break;
        }
        let l = row.iter().take_while(|c| **c == 0).count();
        let t = row.iter().rev().take_while(|c| **c == 0).count();
        lead = lead.min(l);
        trail = trail.min(t);
    }
    if lead.saturating_add(trail) >= taps {
        return (0, 0);
    }
    (lead, trail)
}

/// The ideal tap index, guarded against a non-finite centre.
fn clamp_index(v: f64, src_len: usize) -> i64 {
    if !v.is_finite() {
        return 0;
    }
    let len = i64::try_from(src_len).unwrap_or(1 << 40);
    (v as i64).clamp(-len - 1, len.saturating_mul(2).saturating_add(1))
}

/// Normalise `acc` to sum 1, quantise to `COEFF_SHIFT` bits, and force the
/// quantised row to sum to exactly `COEFF_ONE`.
fn quantise(acc: &mut [f64], coeffs: &mut [i32], d: usize, taps: usize) {
    let sum: f64 = acc.iter().sum();
    let inv = if sum.abs() > 1e-12 { 1.0 / sum } else { 0.0 };
    let Some(row_start) = d.checked_mul(taps) else {
        return;
    };
    let Some(row) = coeffs.get_mut(row_start..row_start.saturating_add(taps)) else {
        return;
    };
    if inv == 0.0 {
        // Degenerate kernel (every weight zero): fall back to a unit impulse on
        // the nearest sample so the output is defined rather than black.
        for (i, slot) in row.iter_mut().enumerate() {
            *slot = if i == 0 { COEFF_ONE } else { 0 };
        }
        return;
    }
    let mut total: i64 = 0;
    let mut best = (0usize, -1.0f64);
    for (i, (slot, &w)) in row.iter_mut().zip(acc.iter()).enumerate() {
        let n = w * inv;
        // Tried truncating `d * (1 << shift) + 0.5` toward zero (a plain C
        // `(int)` cast) instead of rounding, on the hypothesis that the
        // reference's own coefficient quantisation uses that convention and
        // that it would explain the scattered +-1 divergence measured below
        // on `Bicubic`/`Lanczos` (both have negative-lobe taps; `Point`/
        // `Bilinear`/`Area` never do and already match the reference
        // exactly). Measured against `ffmpeg 8.1` on a real 80x60 downscale:
        // it made `Bicubic` worse (1184 -> 3063 differing bytes of 360000)
        // and `Lanczos` only marginally better (1656 -> 1610), so the
        // hypothesis is not confirmed and this keeps plain rounding, the
        // better-measured of the two. See `docs/signal/vaco-scale.md`
        // section 3 for the standing, already-documented divergence this
        // leaves on those two kernels (recorded there before this change,
        // not introduced by it).
        let q = (n * f64::from(COEFF_ONE)).round();
        let q = if q.is_finite() {
            q.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
        } else {
            0
        };
        *slot = q;
        total += i64::from(q);
        if n.abs() > best.1 {
            best = (i, n.abs());
        }
    }
    let residual = i64::from(COEFF_ONE) - total;
    if residual != 0
        && let Some(slot) = row.get_mut(best.0)
    {
        *slot =
            slot.saturating_add(residual.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32);
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::many_single_char_names,
    clippy::float_cmp,
    clippy::integer_division,
    clippy::needless_range_loop,
    clippy::field_reassign_with_default,
    clippy::unreadable_literal,
    clippy::cast_possible_wrap,
    reason = "a failing assertion in a test is a failing test"
)]
mod tests {
    use super::*;
    use vaco_limits::Limits;

    fn bank(kernel: Kernel, src: usize, dst: usize) -> FilterBank {
        let mut b = Budget::new(Limits::permissive());
        build_bank(
            &mut b,
            &FilterSpec {
                kernel,
                src_len: src,
                dst_len: dst,
                phase_src: 0.0,
                phase_dst: 0.0,
                max_taps: DEFAULT_MAX_TAPS,
            },
        )
        .expect("bank builds")
    }

    #[test]
    fn every_row_sums_to_one_in_fixed_point() {
        for kernel in [
            Kernel::Point,
            Kernel::Bilinear,
            Kernel::bicubic_default(),
            Kernel::Lanczos { a: 3.0 },
            Kernel::Gaussian { sigma: 1.0 },
            Kernel::Area,
        ] {
            for (s, d) in [(1, 1), (1, 7), (7, 1), (16, 16), (1920, 640), (100, 301)] {
                let b = bank(kernel, s, d);
                for row in 0..d {
                    let sum: i32 = b.row(row).expect("row").iter().sum();
                    assert_eq!(sum, COEFF_ONE, "{kernel:?} {s}->{d} row {row}");
                }
            }
        }
    }

    #[test]
    fn offsets_never_leave_the_source() {
        for (s, d) in [(1, 1), (2, 9), (9, 2), (33, 32), (5, 5)] {
            let b = bank(Kernel::Lanczos { a: 3.0 }, s, d);
            assert!(b.taps <= s);
            for &o in &b.offsets {
                assert!(
                    o as usize + b.taps <= s,
                    "offset {o} taps {} src {s}",
                    b.taps
                );
            }
        }
    }

    #[test]
    fn point_kernel_picks_a_single_nearest_sample_on_downscale() {
        // Coordinator-verified reference: ffmpeg's `flags=neighbor` on a 2:1
        // downscale takes source pixel 2*d+1 -- an 8-wide ramp
        // 10,20,...,80 becomes 20,40,60,80.
        let b = bank(Kernel::Point, 8, 4);
        for d in 0..4 {
            let row = b.row(d).expect("row");
            assert_eq!(
                row.iter().filter(|&&w| w != 0).count(),
                1,
                "row {d}: {row:?} is not a single tap"
            );
            let (tap_idx, _) = row
                .iter()
                .enumerate()
                .find(|&(_, &w)| w != 0)
                .expect("one nonzero tap");
            let picked = b.offsets[d] as usize + tap_idx;
            assert_eq!(
                picked,
                2 * d + 1,
                "row {d} picked source index {picked}, want {}",
                2 * d + 1
            );
        }
    }

    #[test]
    fn identity_is_detected() {
        for kernel in [Kernel::Point, Kernel::Bilinear, Kernel::bicubic_default()] {
            let b = bank(kernel, 64, 64);
            assert!(b.is_identity(), "{kernel:?} 64->64 should be an identity");
        }
    }

    #[test]
    fn bicubic_matches_the_measured_reference_taps() {
        // The four taps of a 2x upsample, which is what the reference's chroma
        // upsampler applies and what pinned (B, C) = (0, 0.6).
        let k = Kernel::bicubic_default();
        let got = [k.eval(0.25), k.eval(0.75), k.eval(1.25), k.eval(1.75)];
        let want = [0.871_875, 0.240_625, -0.084_375, -0.028_125];
        for (g, w) in got.iter().zip(want.iter()) {
            assert!((g - w).abs() < 1e-12, "{got:?} vs {want:?}");
        }
    }

    /// Regression: a kernel whose weights explode puts `i32::MIN` in a
    /// coefficient slot, and `i32::MIN.abs()` overflows in debug. Found by
    /// `fuzz_targets/scale_convert.rs` at exec 31.
    #[test]
    fn a_degenerate_kernel_does_not_overflow_the_absolute_sum() {
        let mut b = Budget::new(Limits::permissive());
        for kernel in [
            Kernel::Gaussian { sigma: 1e-9 },
            Kernel::Gaussian { sigma: 1e9 },
            Kernel::Lanczos { a: 1e9 },
            Kernel::Bicubic { b: 1e18, c: -1e18 },
            Kernel::Bicubic {
                b: f64::NAN,
                c: f64::NAN,
            },
        ] {
            for (src, dst) in [(1, 1), (2, 9), (64, 3), (3, 64)] {
                let bank = build_bank(
                    &mut b,
                    &FilterSpec {
                        kernel,
                        src_len: src,
                        dst_len: dst,
                        phase_src: 0.0,
                        phase_dst: 0.0,
                        max_taps: DEFAULT_MAX_TAPS,
                    },
                )
                .expect("a degenerate kernel still produces a bank");
                assert!(bank.abs_sum >= 0);
                for &o in &bank.offsets {
                    assert!(o as usize + bank.taps <= src);
                }
            }
        }
    }

    #[test]
    fn area_downscale_is_a_plain_average() {
        let b = bank(Kernel::Area, 8, 2);
        assert_eq!(b.taps, 4);
        for d in 0..2 {
            for &c in b.row(d).expect("row") {
                assert_eq!(c, COEFF_ONE / 4);
            }
        }
    }
}
