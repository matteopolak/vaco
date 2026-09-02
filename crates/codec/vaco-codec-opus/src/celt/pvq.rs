//! CELT's pyramid vector quantizer (PVQ) shape decode and the small
//! per-sample DSP primitives (spreading, Hadamard folding, stereo
//! split/merge) that sit around it. RFC 6716 §4.3.4 / §4.3.5, transliterated
//! from `celt/cwrs.c`, `celt/vq.c` and `celt/bands.c`.
//!
//! The combinatorics in [`decode_pulses`] always use the generic O(N*K) row
//! recurrence (`cwrs.c`'s `_n<=6` branch of `ncwrs_urow`, generalised to any
//! `N`) rather than the reference's N=2/3/4 closed-form fast paths — CELT's
//! bands are small enough that the difference is not worth two more
//! hand-transcribed special cases.

use crate::range::RangeDecoder;

/// `cwrs.c`'s `unext`: advance a `U(n,*)` row to `U(n+1,*)` in place.
/// Wrapping arithmetic matches the reference's `UADD32`/`USUB32`, which are
/// plain modular 32-bit ops relied on not to actually wrap for valid inputs.
fn unext(u: &mut [u32], ui0_init: u32) {
    let mut ui0 = ui0_init;
    for j in 1..u.len() {
        let ui1 = u[j].wrapping_add(u[j - 1]).wrapping_add(ui0);
        u[j - 1] = ui0;
        ui0 = ui1;
    }
    if let Some(last) = u.last_mut() {
        *last = ui0;
    }
}

/// `cwrs.c`'s `uprev`: the inverse of [`unext`], row `N` back to row `N-1`.
fn uprev(u: &mut [u32], ui0_init: u32) {
    let mut ui0 = ui0_init;
    for j in 1..u.len() {
        let ui1 = u[j].wrapping_sub(u[j - 1]).wrapping_sub(ui0);
        u[j - 1] = ui0;
        ui0 = ui1;
    }
    if let Some(last) = u.last_mut() {
        *last = ui0;
    }
}

/// `cwrs.c`'s `ncwrs_urow`, generic-`n` branch: fills `u[0..=k+1]` with
/// `U(n, 0..=k+1)` and returns `V(n, k) = u[k] + u[k+1]`.
///
/// `u` must have length exactly `k + 2`. Starts from the `U(2, *)` row
/// (`U(2, i) = 2i - 1`) and advances it to `U(n, *)` with `n - 2` calls to
/// [`unext`], exactly as the reference's `_n<=6` branch does — generalised
/// to any `n` since CELT's bands never need the `n>6` fast-path tables.
fn ncwrs_urow(n: usize, k: usize, u: &mut [u32]) {
    let len = k + 2;
    if u.len() < len || len < 2 {
        return;
    }
    u[0] = 0;
    u[1] = 1;
    for (idx, slot) in u.iter_mut().enumerate().take(len).skip(2) {
        *slot = ((idx as u32) << 1) - 1;
    }
    for _ in 2..n {
        // unext(u + 1, k + 1, 1): advance U(m, *) to U(m+1, *) in place.
        unext(&mut u[1..len], 1);
    }
}

fn v_and_u_row(n: usize, k: usize) -> Vec<u32> {
    let mut u = vec![0u32; k + 2];
    ncwrs_urow(n, k, &mut u);
    u
}

/// `V(n, k)`: the size of the codebook of `n`-dimensional pulse vectors with
/// `k` pulses (including sign). Used to size the range-coded index.
#[must_use]
pub fn ncwrs(n: usize, k: usize) -> u32 {
    if k == 0 {
        return 1;
    }
    let u = v_and_u_row(n, k);
    u.get(k)
        .copied()
        .unwrap_or(0)
        .wrapping_add(u.get(k + 1).copied().unwrap_or(0))
}

/// `cwrs.c`'s `cwrsi`: recover the `n`-dimensional pulse vector (each entry
/// signed, `sum(|y|) == k`) that the range-coded index `i` addresses.
fn cwrsi(n: usize, mut k: usize, mut i: u32, y: &mut [i32], u: &mut [u32]) {
    for slot in y.iter_mut().take(n) {
        let p = u.get(k + 1).copied().unwrap_or(0);
        let neg = i >= p;
        if neg {
            i -= p;
        }
        let mut yj = k as i32;
        let mut p2 = u.get(k).copied().unwrap_or(0);
        while p2 > i {
            k = k.saturating_sub(1);
            p2 = u.get(k).copied().unwrap_or(0);
        }
        i -= p2;
        yj -= k as i32;
        *slot = if neg { -yj } else { yj };
        let width = (k + 2).min(u.len());
        uprev(&mut u[..width], 0);
    }
}

/// `cwrs.c`'s `decode_pulses`: read the combinatorial index for an
/// `n`-dimensional, `k`-pulse shape and expand it to `y`.
pub fn decode_pulses(dec: &mut RangeDecoder<'_>, n: usize, k: usize, y: &mut [i32]) {
    if k == 0 {
        for slot in y.iter_mut().take(n) {
            *slot = 0;
        }
        return;
    }
    let mut u = v_and_u_row(n, k);
    let total = u
        .get(k)
        .copied()
        .unwrap_or(1)
        .wrapping_add(u.get(k + 1).copied().unwrap_or(0))
        .max(1);
    let idx = dec.dec_uint(total).unwrap_or(0);
    cwrsi(n, k, idx, y, &mut u);
}

/// `bands.c`'s `bitexact_cos`: a deterministic fixed-point cosine
/// approximation used for the mid/side split gains. Kept bit-exact to the
/// reference regardless of this crate's otherwise-float DSP because it also
/// governs `bitexact_log2tan`'s bit-allocation split, not just amplitude.
#[must_use]
pub fn bitexact_cos(x: i16) -> i16 {
    let tmp = (4096i32 + i32::from(x) * i32::from(x)) >> 13;
    let x2 = tmp as i16;
    let frac_mul16 = |a: i32, b: i32| (16384 + a * b) >> 15;
    let inner = frac_mul16(-626, i32::from(x2));
    let inner = frac_mul16(i32::from(x2), 8277 + inner);
    let inner = frac_mul16(i32::from(x2), -7651 + inner);
    let x2b = (i32::from(32767 - x2) + inner) as i16;
    1i16.wrapping_add(x2b)
}

/// `bands.c`'s `bitexact_log2tan`.
#[must_use]
pub fn bitexact_log2tan(isin: i32, icos: i32) -> i32 {
    let lc = 32 - icos.leading_zeros() as i32;
    let ls = 32 - isin.leading_zeros() as i32;
    let icos = icos << (15 - lc);
    let isin = isin << (15 - ls);
    let frac_mul16 = |a: i32, b: i32| (16384 + a * b) >> 15;
    (ls - lc) * (1 << 11) + frac_mul16(isin, frac_mul16(isin, -2597) + 7932)
        - frac_mul16(icos, frac_mul16(icos, -2597) + 7932)
}

/// `vq.c`'s `exp_rotation1`.
fn exp_rotation1(x: &mut [f32], len: usize, stride: usize, c: f32, s: f32) {
    if stride >= len {
        return;
    }
    for i in 0..len - stride {
        let x1 = x[i];
        let x2 = x[i + stride];
        x[i + stride] = c * x2 + s * x1;
        x[i] = c * x1 - s * x2;
    }
    let mut i = len.wrapping_sub(2 * stride + 1);
    loop {
        if i >= x.len() || i + stride >= x.len() {
            break;
        }
        let x1 = x[i];
        let x2 = x[i + stride];
        x[i + stride] = c * x2 + s * x1;
        x[i] = c * x1 - s * x2;
        if i == 0 {
            break;
        }
        i -= 1;
    }
}

/// `vq.c`'s `exp_rotation`: PVQ spreading, applied before quantisation on the
/// encoder side and undone (`dir = -1`) after shape decode here.
pub fn exp_rotation(x: &mut [f32], len: usize, dir: i32, stride: usize, k: i32, spread: i32) {
    const SPREAD_FACTOR: [i32; 3] = [15, 10, 5];
    if spread == 0 || 2 * k >= len as i32 {
        return;
    }
    let factor = SPREAD_FACTOR[(spread - 1) as usize];
    let gain = len as f32 / (len as f32 + (factor * k) as f32);
    let theta = 0.5 * gain * gain;
    let c = (0.5 * std::f32::consts::PI * theta).cos();
    let s = (0.5 * std::f32::consts::PI * (1.0 - theta)).cos();

    let mut stride2 = 0usize;
    if len >= 8 * stride {
        stride2 = 1;
        while (stride2 * stride2 + stride2) * stride + (stride >> 2) < len {
            stride2 += 1;
        }
    }
    let sub_len = len / stride;
    for i in 0..stride {
        let Some(slice) = x.get_mut(i * sub_len..(i + 1) * sub_len) else {
            continue;
        };
        if dir < 0 {
            if stride2 != 0 {
                exp_rotation1(slice, sub_len, stride2, s, c);
            }
            exp_rotation1(slice, sub_len, 1, c, s);
        } else {
            exp_rotation1(slice, sub_len, 1, c, -s);
            if stride2 != 0 {
                exp_rotation1(slice, sub_len, stride2, s, -c);
            }
        }
    }
}

/// `vq.c`'s `normalise_residual`, folded into [`alg_unquant`]: scale the
/// integer pulse vector `iy` so `||gain * iy / sqrt(Ryy)|| == gain`.
fn normalise_residual(iy: &[i32], x: &mut [f32], ryy: f32, gain: f32) {
    let scale = gain / ryy.sqrt().max(1e-30);
    for (dst, &v) in x.iter_mut().zip(iy.iter()) {
        *dst = scale * v as f32;
    }
}

/// `vq.c`'s `renormalise_vector`: rescale `x` to norm `gain` in place.
pub fn renormalise_vector(x: &mut [f32], gain: f32) {
    let e: f32 = x.iter().map(|v| v * v).sum::<f32>() + 1e-15;
    let g = gain / e.sqrt();
    for v in x.iter_mut() {
        *v *= g;
    }
}

/// `vq.c`'s `extract_collapse_mask`.
#[must_use]
pub fn extract_collapse_mask(iy: &[i32], n: usize, b: usize) -> u32 {
    if b <= 1 {
        return 1;
    }
    let n0 = n / b;
    let mut mask = 0u32;
    for i in 0..b {
        for j in 0..n0 {
            if iy.get(i * n0 + j).copied().unwrap_or(0) != 0 {
                mask |= 1 << i;
            }
        }
    }
    mask
}

/// `vq.c`'s `alg_unquant`: decode the PVQ shape for one (sub-)band and mix it
/// in at `gain`. Returns the collapse mask.
pub fn alg_unquant(
    dec: &mut RangeDecoder<'_>,
    x: &mut [f32],
    n: usize,
    k: i32,
    spread: i32,
    b: usize,
    gain: f32,
) -> u32 {
    if k <= 0 || n < 2 {
        for v in x.iter_mut().take(n) {
            *v = 0.0;
        }
        return 0;
    }
    let mut iy = vec![0i32; n];
    decode_pulses(dec, n, k as usize, &mut iy);
    let ryy: f32 = iy.iter().map(|&v| (v * v) as f32).sum();
    normalise_residual(&iy, x, ryy.max(1.0), gain);
    exp_rotation(x, n, -1, b, k, spread);
    extract_collapse_mask(&iy, n, b)
}

/// `bands.c`'s `haar1`: the in-place Haar butterfly CELT uses both for
/// band-recombining and for switching between frequency- and time-ordered
/// samples.
pub fn haar1(x: &mut [f32], n0: usize, stride: usize) {
    const INV_SQRT2: f32 = std::f32::consts::FRAC_1_SQRT_2;
    let half = n0 / 2;
    for i in 0..stride {
        for j in 0..half {
            let Some(&a) = x.get(stride * 2 * j + i) else {
                continue;
            };
            let Some(&b) = x.get(stride * (2 * j + 1) + i) else {
                continue;
            };
            let t1 = INV_SQRT2 * a;
            let t2 = INV_SQRT2 * b;
            if let Some(slot) = x.get_mut(stride * 2 * j + i) {
                *slot = t1 + t2;
            }
            if let Some(slot) = x.get_mut(stride * (2 * j + 1) + i) {
                *slot = t1 - t2;
            }
        }
    }
}

/// `bands.c`'s `deinterleave_hadamard`.
pub fn deinterleave_hadamard(x: &mut [f32], n0: usize, stride: usize, hadamard: bool) {
    let n = n0 * stride;
    let Some(region) = x.get_mut(..n) else { return };
    let mut tmp = vec![0.0f32; n];
    if hadamard && stride >= 2 {
        let ordery = crate::celt::tables::ordery_table(stride);
        for i in 0..stride {
            for j in 0..n0 {
                let dst = usize::from(*ordery.get(i).unwrap_or(&0)) * n0 + j;
                if let (Some(&v), Some(slot)) = (region.get(j * stride + i), tmp.get_mut(dst)) {
                    *slot = v;
                }
            }
        }
    } else {
        for i in 0..stride {
            for j in 0..n0 {
                if let (Some(&v), Some(slot)) =
                    (region.get(j * stride + i), tmp.get_mut(i * n0 + j))
                {
                    *slot = v;
                }
            }
        }
    }
    region.copy_from_slice(&tmp);
}

/// `bands.c`'s `interleave_hadamard`: the inverse of
/// [`deinterleave_hadamard`].
pub fn interleave_hadamard(x: &mut [f32], n0: usize, stride: usize, hadamard: bool) {
    let n = n0 * stride;
    let Some(region) = x.get_mut(..n) else { return };
    let mut tmp = vec![0.0f32; n];
    if hadamard && stride >= 2 {
        let ordery = crate::celt::tables::ordery_table(stride);
        for i in 0..stride {
            for j in 0..n0 {
                let src = usize::from(*ordery.get(i).unwrap_or(&0)) * n0 + j;
                if let (Some(&v), Some(slot)) = (region.get(src), tmp.get_mut(j * stride + i)) {
                    *slot = v;
                }
            }
        }
    } else {
        for i in 0..stride {
            for j in 0..n0 {
                if let (Some(&v), Some(slot)) =
                    (region.get(i * n0 + j), tmp.get_mut(j * stride + i))
                {
                    *slot = v;
                }
            }
        }
    }
    region.copy_from_slice(&tmp);
}

/// `bands.c`'s `intensity_stereo`: fold the side channel into the mid at a
/// band where dual coding is no longer worth its bits.
pub fn intensity_stereo(
    x: &mut [f32],
    y: &[f32],
    band_energy_left: f32,
    band_energy_right: f32,
    n: usize,
) {
    let norm =
        (1e-15 + band_energy_left * band_energy_left + band_energy_right * band_energy_right)
            .sqrt()
            + 1e-15;
    let a1 = band_energy_left / norm;
    let a2 = band_energy_right / norm;
    for j in 0..n {
        if let (Some(&l), Some(&r)) = (x.get(j), y.get(j))
            && let Some(slot) = x.get_mut(j)
        {
            *slot = a1 * l + a2 * r;
        }
    }
}

/// `bands.c`'s `stereo_split`: rotate `(X, Y)` into `(mid, side)` before a
/// stereo split decision (`itheta == 0`/`16384` special cases skip this).
pub fn stereo_split(x: &mut [f32], y: &mut [f32], n: usize) {
    const INV_SQRT2: f32 = std::f32::consts::FRAC_1_SQRT_2;
    for j in 0..n {
        if let (Some(&xv), Some(&yv)) = (x.get(j), y.get(j)) {
            let l = INV_SQRT2 * xv;
            let r = INV_SQRT2 * yv;
            if let Some(s) = x.get_mut(j) {
                *s = l + r;
            }
            if let Some(s) = y.get_mut(j) {
                *s = r - l;
            }
        }
    }
}

/// `bands.c`'s `stereo_merge`: reconstruct `(left, right)` from a decoded
/// `(mid, side)` pair.
pub fn stereo_merge(x: &mut [f32], y: &mut [f32], mid: f32, n: usize) {
    let mut xp = 0.0f32;
    let mut side = 0.0f32;
    for j in 0..n {
        if let (Some(&xv), Some(&yv)) = (x.get(j), y.get(j)) {
            xp += xv * yv;
            side += yv * yv;
        }
    }
    xp *= mid;
    let mid2 = mid * 0.5;
    let el = mid2 * mid2 + side - 2.0 * xp;
    let er = mid2 * mid2 + side + 2.0 * xp;
    if er < 6e-4 || el < 6e-4 {
        for j in 0..n {
            let v = x.get(j).copied().unwrap_or(0.0);
            if let Some(slot) = y.get_mut(j) {
                *slot = v;
            }
        }
        return;
    }
    let lgain = 1.0 / el.sqrt();
    let rgain = 1.0 / er.sqrt();
    for j in 0..n {
        if let (Some(&xv), Some(&yv)) = (x.get(j), y.get(j)) {
            let l = mid * xv;
            let r = yv;
            if let Some(s) = x.get_mut(j) {
                *s = lgain * (l - r);
            }
            if let Some(s) = y.get_mut(j) {
                *s = rgain * (l + r);
            }
        }
    }
}

/// `bands.c`'s `compute_qn`: the split-angle resolution for a band of size
/// `n` given its bit budget `b`.
#[must_use]
pub fn compute_qn(n: i32, b: i32, offset: i32, pulse_cap: i32, stereo: bool) -> i32 {
    use crate::celt::tables::EXP2_TABLE8;
    use crate::range::BITRES;
    let bitres = BITRES as i32;
    let mut n2 = 2 * n - 1;
    if stereo && n == 2 {
        n2 -= 1;
    }
    let mut qb = (b - pulse_cap - (4 << bitres)).min((b + n2 * offset) / n2);
    qb = qb.min(8 << bitres);
    if qb < (1 << bitres) >> 1 {
        1
    } else {
        let idx = (qb & 0x7) as usize;
        let qn = i32::from(EXP2_TABLE8[idx]) >> (14 - (qb >> bitres));
        ((qn + 1) >> 1) << 1
    }
}
