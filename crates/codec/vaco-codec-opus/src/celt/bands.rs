//! CELT's per-band shape decode: `quant_band`'s recursive split between
//! plain PVQ, further mono resolution splitting, and the stereo mid/side
//! split, plus the frame-level orchestration (`quant_all_bands`).
//! RFC 6716 §4.3.4-§4.3.5, from `celt/bands.c`.
//!
//! This works on owned `Vec<f32>` buffers rather than mirroring the
//! reference's in-place pointer aliasing between a band's coefficients and
//! the persistent "norm" history used for spectral folding — the two
//! regions are always disjoint by construction (see [`quant_all_bands`]'s
//! doc), but expressing that to the borrow checker without `unsafe`
//! (forbidden workspace-wide) is more naturally done by copying the small
//! per-band history slice in and the `lowband_out` slice back out than by
//! threading raw sub-slices through the recursion. Bands top out at a few
//! hundred samples, so the copies are not a real cost.
//!
//! **Known gap**: the N=2 stereo special case (a band split down to a
//! single stereo sample pair, RFC 6716 4.3.4.3) and the transient
//! recombine/time-divide bit-interleaving corners are transliterated but
//! received less differential scrutiny than the main per-band path — see
//! `docs/codec/vaco-codec-opus.md`.

use crate::celt::pvq::{
    alg_unquant, bitexact_cos, bitexact_log2tan, compute_qn, deinterleave_hadamard, haar1,
    interleave_hadamard, renormalise_vector, stereo_merge,
};
use crate::celt::rate::{bits2pulses, cache_row_pub, get_pulses, pulses2bits};
use crate::celt::tables::{BIT_DEINTERLEAVE_TABLE, BIT_INTERLEAVE_TABLE, EBANDS, LOG_N, NB_EBANDS};
use crate::range::{BITRES, RangeDecoder};

const QTHETA_OFFSET: i32 = 4;
const QTHETA_OFFSET_TWOPHASE: i32 = 16;

fn celt_lcg_rand(seed: u32) -> u32 {
    1_664_525u32.wrapping_mul(seed).wrapping_add(1_013_904_223)
}

fn isqrt32(x: u32) -> u32 {
    if x == 0 {
        return 0;
    }
    let mut r = f64::from(x).sqrt() as u32;
    // Correct the float sqrt to the exact integer floor, since the split-angle
    // triangular pdf's decode must land the same side of the boundary the
    // encoder used.
    while r > 0 && r.saturating_mul(r) > x {
        r -= 1;
    }
    while (r + 1).saturating_mul(r + 1) <= x {
        r += 1;
    }
    r
}

/// The parameters that stay fixed across one `quant_band` recursion tree.
struct BandCtx<'a, 'b> {
    dec: &'a mut RangeDecoder<'b>,
    band: usize,
    intensity: i32,
    seed: &'a mut u32,
}

/// The outcome of one [`quant_band`] call.
struct BandResult {
    x: Vec<f32>,
    y: Option<Vec<f32>>,
    collapse_mask: u32,
    lowband_out: Option<Vec<f32>>,
}

/// One call of `celt/bands.c`'s `quant_band` (decode side; `resynth` is
/// unconditionally true, matching the reference decoder build).
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors celt/bands.c's quant_band; the parameters are not independently groupable"
)]
fn quant_band(
    ctx: &mut BandCtx<'_, '_>,
    mut x: Vec<f32>,
    y_in: Option<Vec<f32>>,
    stereo: bool,
    n_in: usize,
    mut b: i32,
    spread: i32,
    mut block_count: usize,
    tf_change: i32,
    lowband: Option<Vec<f32>>,
    remaining_bits: &mut i32,
    mut lm: i32,
    want_lowband_out: bool,
    level: i32,
    gain: f32,
    mut fill: u32,
) -> BandResult {
    let n0 = n_in;
    let mut lowband = lowband;
    let entry_block_count = block_count.max(1);
    let mut n_b = n_in / entry_block_count;
    // `celt/bands.c`'s `quant_band` sets `longBlocks = B0==1` from the
    // *entry* block count, before recombining/time-dividing touch it.
    let long_blocks = entry_block_count == 1;
    let mut recombine = 0i32;
    let mut time_divide = 0i32;

    // Special case: a single-sample band only ever needs a sign bit.
    if n_in == 1 {
        // `celt/bands.c`'s `quant_band_n1` decodes X's and (if stereo) Y's
        // sign as two *independent* bits, each separately gated on and
        // debiting `remaining_bits` — reusing X's sign for Y here skipped
        // Y's bit entirely, desyncing every stereo N=1 band (RFC 6716's
        // narrowest bands, e.g. LM=0's band 0) by one bit.
        let mut sign = false;
        if *remaining_bits >= 1 << BITRES {
            sign = ctx.dec.dec_bits(1) != 0;
            *remaining_bits -= 1 << BITRES;
        }
        x[0] = if sign { -1.0 } else { 1.0 };
        let mut y_out = y_in;
        if let Some(yv) = y_out.as_mut() {
            let mut y_sign = false;
            if *remaining_bits >= 1 << BITRES {
                y_sign = ctx.dec.dec_bits(1) != 0;
                *remaining_bits -= 1 << BITRES;
            }
            yv[0] = if y_sign { -1.0 } else { 1.0 };
        }
        let lowband_out = want_lowband_out.then(|| vec![x[0] / 16.0]);
        return BandResult {
            x,
            y: y_out,
            collapse_mask: 1,
            lowband_out,
        };
    }

    // Band recombining / time-frequency reshaping (mono, top-level bands only).
    if !stereo && level == 0 {
        if tf_change > 0 {
            recombine = tf_change;
        }
        for k in 0..recombine {
            haar1(&mut x, n_in >> k, 1 << k);
            if let Some(lb) = lowband.as_mut() {
                haar1(lb, n_in >> k, 1 << k);
            }
            let a = usize::from(BIT_INTERLEAVE_TABLE[(fill & 0xF) as usize]);
            let b2 = usize::from(BIT_INTERLEAVE_TABLE[((fill >> 4) & 0xF) as usize]);
            fill = (a as u32) | ((b2 as u32) << 2);
        }
        block_count >>= recombine.max(0);
        n_b <<= recombine.max(0);

        let mut tf = tf_change;
        while n_b.is_multiple_of(2) && tf < 0 {
            haar1(&mut x, n_b, block_count);
            if let Some(lb) = lowband.as_mut() {
                haar1(lb, n_b, block_count);
            }
            fill |= fill << block_count;
            block_count <<= 1;
            n_b >>= 1;
            time_divide += 1;
            tf += 1;
        }

        if block_count > 1 {
            deinterleave_hadamard(
                &mut x,
                n_b >> recombine.max(0),
                block_count << recombine.max(0),
                long_blocks,
            );
            if let Some(lb) = lowband.as_mut() {
                deinterleave_hadamard(
                    lb,
                    n_b >> recombine.max(0),
                    block_count << recombine.max(0),
                    long_blocks,
                );
            }
        }
    }

    // `celt/bands.c`'s `quant_band` reassigns `B0=B; N_B0=N_B;` here, i.e.
    // *after* recombining/time-dividing — this is the block count the
    // top-level split (`quant_partition`'s own `B0`) actually splits from,
    // not the pre-preprocessing entry value. Using the entry value here
    // made every top-level split of a non-transient band (`block_count==1`
    // on entry, but `>1` after time-dividing for a negative `tf_change`)
    // wrongly skip the low-energy delta-boost gate (`B0>1`) and pick the
    // triangular- vs uniform-pdf itheta decode on the wrong side.
    let b0 = block_count.max(1);
    let n_b0 = n_b;

    // Decide whether to split for extra resolution (mono self-split) or
    // because the caller passed a genuine second channel.
    let mut n = n_in;
    let mut y = y_in;
    if y.is_none() && !stereo && lm != -1 {
        let cache = cache_row_pub((lm + 1).max(0) as usize, ctx.band);
        let cap = i32::from(cache.first().copied().unwrap_or(0));
        let cache_top = i32::from(cache.get(cap as usize).copied().unwrap_or(0));
        if b > cache_top + 12 && n > 2 {
            n >>= 1;
            let tail = x.split_off(n);
            y = Some(tail);
            lm -= 1;
            if block_count == 1 {
                fill = (fill & 1) | (fill << 1);
            }
            block_count = (block_count + 1) >> 1;
        }
    }

    let (mut out_x, mut out_y, mut collapse_mask, mut lowband_out);

    if let Some(mut yv) = y {
        // --- theta-coded split: real stereo, or a synthetic mono split ---
        let pulse_cap = i32::from(LOG_N.get(ctx.band).copied().unwrap_or(0)) + lm * (1 << BITRES);
        let offset = (pulse_cap >> 1)
            - if stereo && n == 2 {
                QTHETA_OFFSET_TWOPHASE
            } else {
                QTHETA_OFFSET
            };
        let mut qn = compute_qn(n as i32, b, offset, pulse_cap, stereo);
        if stereo && ctx.band as i32 >= ctx.intensity {
            qn = 1;
        }
        let tell = ctx.dec.tell_frac();
        let mut itheta;
        let mut inv = false;
        if qn != 1 {
            if stereo && n > 2 {
                let p0 = 3i32;
                let x0 = qn / 2;
                let ft = (p0 * (x0 + 1) + x0).max(1);
                let fs = ctx.dec.decode_raw(ft as u32) as i32;
                let x_val = if fs < (x0 + 1) * p0 {
                    fs / p0
                } else {
                    x0 + 1 + (fs - (x0 + 1) * p0)
                };
                let (fl, fh) = if x_val <= x0 {
                    (p0 * x_val, p0 * (x_val + 1))
                } else {
                    (
                        (x_val - 1 - x0) + (x0 + 1) * p0,
                        (x_val - x0) + (x0 + 1) * p0,
                    )
                };
                ctx.dec.update_raw(fl as u32, fh as u32, ft as u32);
                itheta = x_val;
            } else if b0 > 1 || stereo {
                itheta = ctx.dec.dec_uint((qn + 1).max(2) as u32).unwrap_or(0) as i32;
            } else {
                let ft = (((qn >> 1) + 1) * ((qn >> 1) + 1)).max(1);
                let fm = ctx.dec.decode_raw(ft as u32) as i32;
                let half = (qn >> 1) * ((qn >> 1) + 1) / 2;
                if fm < half {
                    let it = ((isqrt32(8u32.saturating_mul(fm.max(0) as u32) + 1) as i32) - 1) >> 1;
                    let fs = it + 1;
                    let fl = it * (it + 1) / 2;
                    ctx.dec.update_raw(fl as u32, (fl + fs) as u32, ft as u32);
                    itheta = it;
                } else {
                    let rem = (ft - fm - 1).max(0) as u32;
                    let it = (2 * (qn + 1) - isqrt32(8u32.saturating_mul(rem) + 1) as i32) >> 1;
                    let fs = qn + 1 - it;
                    let fl = ft - ((qn + 1 - it) * (qn + 2 - it) / 2);
                    ctx.dec.update_raw(fl as u32, (fl + fs) as u32, ft as u32);
                    itheta = it;
                }
            }
            itheta = itheta * 16384 / qn.max(1);
        } else if stereo {
            inv = if b > 2 << BITRES && *remaining_bits > 2 << BITRES {
                ctx.dec.bit_logp(2)
            } else {
                false
            };
            if inv {
                for v in &mut yv {
                    *v = -*v;
                }
            }
            itheta = 0;
        } else {
            itheta = 0;
        }

        let qalloc = ctx.dec.tell_frac() - tell;
        b -= qalloc;
        let orig_fill = fill;

        let (imid, iside, delta_raw);
        if itheta == 0 {
            imid = 32767i32;
            iside = 0i32;
            fill &= (1u32 << block_count) - 1;
            delta_raw = -16384i32;
        } else if itheta == 16384 {
            imid = 0;
            iside = 32767;
            fill &= ((1u32 << block_count) - 1) << block_count;
            delta_raw = 16384;
        } else {
            imid = i32::from(bitexact_cos(itheta as i16));
            iside = i32::from(bitexact_cos((16384 - itheta) as i16));
            // `celt/bands.c`'s `compute_theta` uses `FRAC_MUL16((N-1)<<7,
            // bitexact_log2tan(iside,imid))`, i.e. a Q15 fixed-point
            // multiply-and-round (`(16384 + a*b) >> 15`), not a Q14 one —
            // using `>> 14` here doubled `delta`, which cascaded into wildly
            // wrong `mbits`/`sbits` splits for any band that both self-splits
            // and needs the low-energy bit-boost adjustment.
            delta_raw = (((n as i32 - 1) << 7) * bitexact_log2tan(iside, imid) + 16384) >> 15;
        }
        let mid = f32::from(imid as i16) / 32768.0;
        let side = f32::from(iside as i16) / 32768.0;

        if n == 2 && stereo {
            let mut mbits = b;
            let mut sbits = 0i32;
            if itheta != 0 && itheta != 16384 {
                sbits = 1 << BITRES;
            }
            mbits -= sbits;
            // `itheta > 8192` means the side carries more energy than the
            // mid, so the primary (directly PVQ-decoded) channel is `Y`
            // rather than `X` — but the final `mid`/`side` scaling always
            // applies to `X`/`Y` by their *original* channel identity, not
            // to whichever one was primary. `celt/bands.c`'s N=2 special
            // case aliases `x2`/`y2` to `X`/`Y` and then scales `X`/`Y`
            // directly; this reconstructs that by remembering which is
            // which instead of aliasing.
            let primary_is_y = itheta > 8192;
            *remaining_bits -= qalloc + sbits;
            let (mut primary, mut secondary) = if primary_is_y { (yv, x) } else { (x, yv) };
            let sign = if sbits != 0 {
                ctx.dec.dec_bits(1) != 0
            } else {
                false
            };
            let sign_val = if sign { -1.0f32 } else { 1.0 };
            let inner = quant_band(
                ctx,
                primary,
                None,
                false,
                n,
                mbits,
                spread,
                block_count,
                tf_change,
                lowband,
                remaining_bits,
                lm,
                false,
                level,
                gain,
                orig_fill,
            );
            primary = inner.x;
            secondary[0] = -sign_val * primary[1];
            secondary[1] = sign_val * primary[0];
            let (mut ox, mut oy) = if primary_is_y {
                (secondary, primary)
            } else {
                (primary, secondary)
            };
            ox[0] *= mid;
            ox[1] *= mid;
            oy[0] *= side;
            oy[1] *= side;
            let t0 = ox[0];
            ox[0] = t0 - oy[0];
            oy[0] += t0;
            let t1 = ox[1];
            ox[1] = t1 - oy[1];
            oy[1] += t1;
            out_x = ox;
            out_y = Some(oy);
            collapse_mask = inner.collapse_mask;
            lowband_out = None;
        } else {
            let mut delta = delta_raw;
            if b0 > 1 && !stereo && (itheta & 0x3fff) != 0 {
                if itheta > 8192 {
                    delta -= delta >> (4 - lm).clamp(0, 30);
                } else {
                    delta = 0.min(delta + ((n as i32) << BITRES >> (5 - lm).clamp(0, 30)));
                }
            }
            let mut mbits = 0.max(b.min((b - delta) / 2));
            let mut sbits = b - mbits;
            *remaining_bits -= qalloc;

            let next_level = if stereo { 0 } else { level + 1 };
            let rebalance0 = *remaining_bits;
            let mid_gain = if stereo { 1.0 } else { gain * mid };
            let side_gain = gain * side;

            let (rx, ry, cmx, cmy, lbo);
            if mbits >= sbits {
                let a = quant_band(
                    ctx,
                    x,
                    None,
                    false,
                    n,
                    mbits,
                    spread,
                    block_count,
                    tf_change,
                    lowband,
                    remaining_bits,
                    lm,
                    stereo || want_lowband_out,
                    next_level,
                    mid_gain,
                    fill,
                );
                let rebalance = mbits - (rebalance0 - *remaining_bits);
                if rebalance > 3 << BITRES && itheta != 0 {
                    sbits += rebalance - (3 << BITRES);
                }
                let shift = if stereo { 0 } else { b0 >> 1 };
                let b_ = quant_band(
                    ctx,
                    yv,
                    None,
                    false,
                    n,
                    sbits,
                    spread,
                    block_count,
                    tf_change,
                    None,
                    remaining_bits,
                    lm,
                    false,
                    next_level,
                    side_gain,
                    fill >> block_count,
                );
                rx = a.x;
                lbo = a.lowband_out;
                cmx = a.collapse_mask;
                ry = b_.x;
                cmy = b_.collapse_mask << shift;
            } else {
                let shift = if stereo { 0 } else { b0 >> 1 };
                let b_ = quant_band(
                    ctx,
                    yv,
                    None,
                    false,
                    n,
                    sbits,
                    spread,
                    block_count,
                    tf_change,
                    None,
                    remaining_bits,
                    lm,
                    false,
                    next_level,
                    side_gain,
                    fill >> block_count,
                );
                let rebalance = sbits - (rebalance0 - *remaining_bits);
                if rebalance > 3 << BITRES && itheta != 16384 {
                    mbits += rebalance - (3 << BITRES);
                }
                let a = quant_band(
                    ctx,
                    x,
                    None,
                    false,
                    n,
                    mbits,
                    spread,
                    block_count,
                    tf_change,
                    lowband,
                    remaining_bits,
                    lm,
                    stereo || want_lowband_out,
                    next_level,
                    mid_gain,
                    fill,
                );
                rx = a.x;
                lbo = a.lowband_out;
                cmx = a.collapse_mask;
                ry = b_.x;
                cmy = b_.collapse_mask << shift;
            }
            collapse_mask = cmx | cmy;
            lowband_out = lbo;
            if stereo {
                out_x = rx;
                out_y = Some(ry);
            } else {
                // `celt/bands.c` aliases `Y = X+N` into the *same* buffer
                // `X` occupies, so writing each recursive half in place
                // reassembles the whole band automatically -- a mono
                // self-split's two halves are contiguous pieces of ONE
                // band, not a second channel. This owned-`Vec` design's
                // equivalent of that aliasing is to concatenate them
                // explicitly; returning them as separate `x`/`y` here
                // silently dropped every mono self-split's second half
                // whenever the caller had no real second channel to hand
                // it to (any band that self-splits for extra resolution
                // inside a channel that isn't itself part of a genuine
                // stereo pair -- i.e. essentially always, since real
                // stereo bands hit the dedicated stereo branches instead).
                let mut combined = rx;
                combined.extend(ry);
                out_x = combined;
                out_y = None;
            }
        }

        if stereo {
            if n != 2
                && let Some(fy) = out_y.as_mut()
            {
                stereo_merge(&mut out_x, fy, mid, n);
            }
            if inv && let Some(fy) = out_y.as_mut() {
                for v in fy.iter_mut() {
                    *v = -*v;
                }
            }
        } else if level == 0 {
            undo_time_freq(
                &mut out_x,
                n0,
                b0,
                recombine,
                time_divide,
                n_b0,
                long_blocks,
                &mut collapse_mask,
            );
            if want_lowband_out {
                let scale = (n0 as f32 * 4_194_304.0).sqrt() / 2048.0;
                lowband_out = Some(out_x.iter().map(|v| v * scale).collect());
            }
        }
        return BandResult {
            x: out_x,
            y: out_y,
            collapse_mask,
            lowband_out,
        };
    }

    // --- no-split base case: direct PVQ (or noise/fold fill) ---
    // `celt/rate.h`'s `bits2pulses`/`pulses2bits` take the raw `LM` (which
    // is legitimately `-1` for the smallest split leaves) and do `LM++`
    // internally to address the cache row — clamping to 0 here before the
    // call shifted every `LM==-1` leaf onto the next cache row, roughly
    // halving `K` for the deepest-recursion leaves in wide bands.
    let mut q = bits2pulses(lm, ctx.band, b);
    let mut curr_bits = pulses2bits(lm, ctx.band, q);
    *remaining_bits -= curr_bits;
    while *remaining_bits < 0 && q > 0 {
        *remaining_bits += curr_bits;
        q -= 1;
        curr_bits = pulses2bits(lm, ctx.band, q);
        *remaining_bits -= curr_bits;
    }

    if q != 0 {
        let k = get_pulses(q);
        collapse_mask = alg_unquant(ctx.dec, &mut x, n, k, spread, block_count, gain);
    } else {
        let cm_mask: u32 = if block_count >= 32 {
            u32::MAX
        } else {
            (1u32 << block_count) - 1
        };
        fill &= cm_mask;
        if fill == 0 {
            x.fill(0.0);
            collapse_mask = 0;
        } else if let Some(lb) = lowband.as_ref() {
            for j in 0..n {
                *ctx.seed = celt_lcg_rand(*ctx.seed);
                let tmp = if *ctx.seed & 0x8000 != 0 {
                    1.0 / 256.0
                } else {
                    -1.0 / 256.0
                };
                if let Some(slot) = x.get_mut(j) {
                    *slot = lb.get(j).copied().unwrap_or(0.0) + tmp;
                }
            }
            collapse_mask = fill;
            renormalise_vector(&mut x, gain);
        } else {
            // `celt/bands.c` calls `renormalise_vector(X, N, gain, ...)`
            // unconditionally after filling `X`, whether that fill came
            // from the lowband-fold branch above or this pure-noise one --
            // the raw `seed>>20` values have no defined scale on their own
            // (a 32-bit LCG output right-shifted by 20 is nowhere near the
            // ~unit-norm PVQ vectors this band's `gain` expects). Missing
            // the call here left every zero-pulse band with no folding
            // source at a magnitude in the thousands instead of ~`gain`.
            for v in &mut x {
                *ctx.seed = celt_lcg_rand(*ctx.seed);
                *v = ((*ctx.seed as i32) >> 20) as f32;
            }
            collapse_mask = cm_mask;
            renormalise_vector(&mut x, gain);
        }
    }

    out_x = x;
    out_y = None;
    lowband_out = None;
    if level == 0 {
        undo_time_freq(
            &mut out_x,
            n0,
            b0,
            recombine,
            time_divide,
            n_b0,
            long_blocks,
            &mut collapse_mask,
        );
        if want_lowband_out {
            let scale = (n0 as f32 * 4_194_304.0).sqrt() / 2048.0;
            lowband_out = Some(out_x.iter().map(|v| v * scale).collect());
        }
    }
    BandResult {
        x: out_x,
        y: out_y,
        collapse_mask,
        lowband_out,
    }
}

/// Undo the band-recombine and time-divide reshaping done at the top of
/// [`quant_band`], and fold the fine-grained collapse mask back down.
/// `celt/bands.c`'s tail of `quant_band`, `resynth` branch, mono case.
fn undo_time_freq(
    x: &mut [f32],
    n0: usize,
    b0: usize,
    recombine: i32,
    time_divide: i32,
    n_b0: usize,
    long_blocks: bool,
    collapse_mask: &mut u32,
) {
    if b0 > 1 {
        interleave_hadamard(
            x,
            n_b0 >> recombine.max(0),
            b0 << recombine.max(0),
            long_blocks,
        );
    }
    let mut n_b = n_b0;
    let mut block_count = b0;
    for _ in 0..time_divide {
        block_count >>= 1;
        n_b <<= 1;
        *collapse_mask |= *collapse_mask >> block_count;
        haar1(x, n_b, block_count);
    }
    for k in 0..recombine {
        let idx = (*collapse_mask & 0xF) as usize;
        *collapse_mask = u32::from(BIT_DEINTERLEAVE_TABLE.get(idx).copied().unwrap_or(0));
        haar1(x, n0 >> k, 1 << k);
    }
    let shift = block_count << recombine.max(0);
    if shift < 32 {
        *collapse_mask &= (1u32 << shift) - 1;
    }
}

/// One frame's worth of band decode. `celt/bands.c`'s `quant_all_bands`,
/// decode side.
///
/// # Why folding never reads the band it is about to write
///
/// `effective_lowband` is always computed as
/// `max(eBands[start], eBands[lowband_offset] - N)` with `lowband_offset`
/// tracking only *already-decoded* bands, so the read range
/// `[effective_lowband, effective_lowband + N)` ends at or before
/// `eBands[lowband_offset] <= eBands[i]` — strictly before the current
/// band's own write range. That invariant (not enforced by the type system
/// here, since both live in the same logical spectrum) is what this
/// module's own doc leans on to justify copying the fold source out before
/// the call instead of threading a shared mutable buffer through it.
#[expect(
    clippy::too_many_arguments,
    reason = "mirrors celt/bands.c's quant_all_bands"
)]
pub fn quant_all_bands(
    dec: &mut RangeDecoder<'_>,
    start: usize,
    end: usize,
    x_all: &mut [f32],
    mut y_all: Option<&mut [f32]>,
    collapse_masks: &mut [u8],
    pulses: &[i32],
    short_blocks: usize,
    spread: i32,
    mut dual_stereo: bool,
    intensity: i32,
    tf_res: &[i32],
    total_bits: i32,
    mut balance: i32,
    lm: i32,
    coded_bands: i32,
    seed: &mut u32,
) {
    let m = 1usize << lm;
    let block_count = if short_blocks != 0 { short_blocks } else { 1 };
    let channels = if y_all.is_some() { 2 } else { 1 };
    let total_len = m * usize::from(EBANDS[NB_EBANDS] as u16);
    let mut norm = vec![0.0f32; total_len];
    let mut norm2 = vec![0.0f32; total_len];

    let mut lowband_offset = 0usize;
    let mut update_lowband = true;
    let start_bin = m * usize::from(EBANDS[start] as u16);

    for i in start..end {
        let band_start = m * usize::from(EBANDS[i] as u16);
        let band_end_bin = m * usize::from(EBANDS[i + 1] as u16);
        let n = band_end_bin - band_start;

        let tell = dec.tell_frac();
        if i != start {
            balance -= tell;
        }
        let remaining_bits_start = total_bits - tell - 1;
        let b = if (i as i32) < coded_bands {
            let curr_balance = balance / 3.min((coded_bands - i as i32).max(1));
            0.max(
                (remaining_bits_start + 1)
                    .min(16383)
                    .min(pulses.get(i).copied().unwrap_or(0) + curr_balance),
            )
        } else {
            0
        };

        // `celt/bands.c`'s `quant_all_bands` sets `lowband_offset = i` when
        // `M*eBands[i]-N >= M*eBands[start]` OR `i == start+1` — the second
        // arm unconditionally seeds folding from the band right after
        // `start` even when it's too small to satisfy the size check, and
        // was missing here.
        if (band_start >= start_bin + n || i == start + 1)
            && (update_lowband || lowband_offset == 0)
        {
            lowband_offset = i;
        }
        let tf_change = tf_res.get(i).copied().unwrap_or(0);

        let mut x_cm;
        let mut y_cm;
        let mut effective_lowband: Option<usize> = None;
        if lowband_offset != 0 && (spread != 3 || block_count > 1 || tf_change < 0) {
            let lowband_bin = m * usize::from(EBANDS[lowband_offset] as u16);
            let eff = start_bin.max(lowband_bin.saturating_sub(n));
            let mut fold_start = lowband_offset;
            while fold_start > 0 && m * usize::from(EBANDS[fold_start - 1] as u16) > eff {
                fold_start -= 1;
            }
            let mut fold_end = lowband_offset.saturating_sub(1);
            while fold_end + 1 < NB_EBANDS && m * usize::from(EBANDS[fold_end + 1] as u16) < eff + n
            {
                fold_end += 1;
            }
            let mut xcm = 0u32;
            let mut ycm = 0u32;
            for fi in fold_start..fold_end.max(fold_start) {
                xcm |= u32::from(collapse_masks.get(fi * channels).copied().unwrap_or(0));
                ycm |= u32::from(
                    collapse_masks
                        .get(fi * channels + channels - 1)
                        .copied()
                        .unwrap_or(0),
                );
            }
            x_cm = xcm;
            y_cm = ycm;
            effective_lowband = Some(eff);
        } else {
            let mask = if block_count >= 32 {
                u32::MAX
            } else {
                (1u32 << block_count) - 1
            };
            x_cm = mask;
            y_cm = mask;
        }

        if dual_stereo && i as i32 == intensity {
            dual_stereo = false;
            for j in start_bin..band_start {
                if let (Some(&a), Some(&bv)) = (norm.get(j), norm2.get(j))
                    && let Some(slot) = norm.get_mut(j)
                {
                    *slot = 0.5 * (a + bv);
                }
            }
        }

        let x_slice: Vec<f32> = x_all
            .get(band_start..band_end_bin)
            .map(<[f32]>::to_vec)
            .unwrap_or_default();
        let lowband_src =
            effective_lowband.and_then(|eff| norm.get(eff..eff + n).map(<[f32]>::to_vec));

        let mut ctx = BandCtx {
            dec,
            band: i,
            intensity,
            seed,
        };

        if dual_stereo {
            // `celt/bands.c`'s per-band `ctx.remaining_bits` is a single
            // field shared by both `quant_band` calls here — X's recursive
            // splits can drive it negative or leave it high, and that
            // *carries into* Y's decode (it feeds the leaf bit-reduction
            // loop and split rebalancing), not just X's own. Giving each
            // channel its own fresh copy of `remaining_bits_start` made Y's
            // allocation decisions independent of what X actually spent.
            let mut remaining = remaining_bits_start;
            let rx = quant_band(
                &mut ctx,
                x_slice,
                None,
                false,
                n,
                b / 2,
                spread,
                block_count,
                tf_change,
                lowband_src.clone(),
                &mut remaining,
                lm,
                true,
                0,
                1.0,
                x_cm,
            );
            copy_back(&mut norm, band_start, rx.lowband_out.as_deref());
            copy_back(x_all, band_start, Some(&rx.x));
            x_cm = rx.collapse_mask;

            let y_slice: Vec<f32> = y_all
                .as_deref()
                .and_then(|y| y.get(band_start..band_end_bin))
                .map(<[f32]>::to_vec)
                .unwrap_or_default();
            let ry = quant_band(
                &mut ctx,
                y_slice,
                None,
                false,
                n,
                b / 2,
                spread,
                block_count,
                tf_change,
                lowband_src,
                &mut remaining,
                lm,
                true,
                0,
                1.0,
                y_cm,
            );
            copy_back(&mut norm2, band_start, ry.lowband_out.as_deref());
            if let Some(y) = y_all.as_deref_mut() {
                copy_back(y, band_start, Some(&ry.x));
            }
            y_cm = ry.collapse_mask;
        } else {
            let y_slice: Option<Vec<f32>> = y_all
                .as_deref()
                .and_then(|y| y.get(band_start..band_end_bin))
                .map(<[f32]>::to_vec);
            let stereo = y_slice.is_some();
            let mut remaining = remaining_bits_start;
            let combined_fill = x_cm | y_cm;
            let r = quant_band(
                &mut ctx,
                x_slice,
                y_slice,
                stereo,
                n,
                b,
                spread,
                block_count,
                tf_change,
                lowband_src,
                &mut remaining,
                lm,
                true,
                0,
                1.0,
                combined_fill,
            );
            copy_back(&mut norm, band_start, r.lowband_out.as_deref());
            if stereo {
                copy_back(&mut norm2, band_start, r.lowband_out.as_deref());
            }
            copy_back(x_all, band_start, Some(&r.x));
            if let (Some(ry), Some(y)) = (r.y.as_ref(), y_all.as_deref_mut()) {
                copy_back(y, band_start, Some(ry));
            }
            x_cm = r.collapse_mask;
            y_cm = r.collapse_mask;
        }

        if let Some(slot) = collapse_masks.get_mut(i * channels) {
            *slot = x_cm as u8;
        }
        if channels == 2
            && let Some(slot) = collapse_masks.get_mut(i * channels + 1)
        {
            *slot = y_cm as u8;
        }
        balance += pulses.get(i).copied().unwrap_or(0) + tell;
        update_lowband = b > (n as i32) << BITRES;
    }
}

fn copy_back(dst: &mut [f32], offset: usize, src: Option<&[f32]>) {
    let Some(src) = src else { return };
    let Some(slot) = dst.get_mut(offset..offset + src.len()) else {
        return;
    };
    slot.copy_from_slice(src);
}
