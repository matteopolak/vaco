//! CELT's bit allocator: turning a per-frame bit budget into a pulse count
//! per band. RFC 6716 §4.3.3, transliterated from `celt/rate.c`'s
//! `compute_allocation`/`interp_bits2pulses` (the decode side, `encode =
//! false`, is the only path implemented — this crate never encodes).
//!
//! Every quantity in this module is pure bit-budget bookkeeping (`1/8`-bit
//! fixed-point integers), not signal amplitude, so unlike CELT's DSP this
//! stays exact integer arithmetic on both the reference's float and
//! fixed-point builds — it must, since a one-bit-per-band difference here
//! desyncs the entropy coder for the rest of the frame.

use crate::celt::tables::{BAND_ALLOCATION, CACHE_BITS50, CACHE_CAPS50, CACHE_INDEX50, LOG2_FRAC_TABLE, NB_EBANDS};
use crate::range::{BITRES, RangeDecoder};

const ALLOC_STEPS: i32 = 6;
const MAX_FINE_BITS: i32 = 8;
const FINE_OFFSET: i32 = 21;

/// `rate.h`'s `get_pulses`: pseudo-pulse-count `i` to the real pulse count
/// `K` the cache tables are addressed by.
#[must_use]
pub const fn get_pulses(i: i32) -> i32 {
    if i < 8 { i } else { (8 + (i & 7)) << ((i >> 3) - 1) }
}

/// The raw PVQ-cost cache row for a `(LM+1, band)` pair — `row[0]` is the
/// maximum pseudo-pulse count, `row[1..]` its per-count costs. Exposed for
/// [`crate::celt::bands`]'s own split-vs-no-split decision, which needs the
/// same row `quant_band` and `bits2pulses`/`pulses2bits` address.
#[must_use]
pub fn cache_row_pub(lm_plus_one: usize, band: usize) -> &'static [u8] {
    cache_row(lm_plus_one, band)
}

fn cache_row(lm_plus_one: usize, band: usize) -> &'static [u8] {
    let Some(&idx) = CACHE_INDEX50.get(lm_plus_one * NB_EBANDS + band) else {
        return &[0];
    };
    let start = usize::try_from(idx).unwrap_or(0);
    CACHE_BITS50.get(start..).unwrap_or(&[0])
}

/// `rate.c`'s `bits2pulses`: the largest pseudo-pulse-count whose cost fits
/// in `bits` (`1/8`-bit units), by binary search over the per-band cache row.
#[must_use]
pub fn bits2pulses(lm: i32, band: usize, bits: i32) -> i32 {
    let cache = cache_row((lm + 1) as usize, band);
    let cap = i32::from(cache.first().copied().unwrap_or(0));
    let mut lo = 0i32;
    let mut hi = cap;
    let bits = bits - 1;
    for _ in 0..6 {
        let mid = (lo + hi + 1) >> 1;
        let v = i32::from(cache.get(mid as usize).copied().unwrap_or(255));
        if v >= bits {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let lo_cost = if lo == 0 { -1 } else { i32::from(cache.get(lo as usize).copied().unwrap_or(255)) };
    let hi_cost = i32::from(cache.get(hi as usize).copied().unwrap_or(255));
    if bits - lo_cost <= hi_cost - bits { lo } else { hi }
}

/// `rate.c`'s `pulses2bits`: the bit cost (`1/8`-bit units) of a
/// pseudo-pulse-count.
#[must_use]
pub fn pulses2bits(lm: i32, band: usize, pulses: i32) -> i32 {
    if pulses == 0 {
        return 0;
    }
    let cache = cache_row((lm + 1) as usize, band);
    i32::from(cache.get(pulses as usize).copied().unwrap_or(255)) + 1
}

/// `celt.c`'s `init_caps`: the maximum reliable bits per band for this
/// `(LM, channel count)`, scaled to actual bit units for this frame's band
/// widths.
#[must_use]
pub fn init_caps(ebands: &[i16], lm: i32, channels: i32) -> Vec<i32> {
    let row = ((2 * lm + channels - 1) as usize) * NB_EBANDS;
    (0..NB_EBANDS)
        .map(|i| {
            let n = i32::from(ebands[i + 1] - ebands[i]) << lm;
            let cap = i32::from(CACHE_CAPS50.get(row + i).copied().unwrap_or(0));
            ((cap + 64) * channels * n) >> 2
        })
        .collect()
}

/// The result of [`compute_allocation`].
#[derive(Debug)]
pub struct Allocation {
    pub pulses: Vec<i32>,
    pub fine_energy: Vec<i32>,
    pub fine_priority: Vec<bool>,
    pub intensity: i32,
    pub dual_stereo: bool,
    pub balance: i32,
    pub coded_bands: i32,
}

/// `rate.c`'s `compute_allocation` + `interp_bits2pulses`, decode side only.
pub fn compute_allocation(
    dec: &mut RangeDecoder<'_>,
    ebands: &[i16],
    start: usize,
    end: usize,
    offsets: &[i32],
    cap: &[i32],
    alloc_trim: i32,
    total_in: i32,
    channels: i32,
    lm: i32,
) -> Allocation {
    let len = NB_EBANDS;
    let mut total = total_in.max(0);
    let mut skip_start = start as i32;
    let skip_rsv = if total >= 1 << BITRES { 1 << BITRES } else { 0 };
    total -= skip_rsv;

    let mut intensity_rsv = 0i32;
    let mut dual_stereo_rsv = 0i32;
    if channels == 2 {
        intensity_rsv = i32::from(LOG2_FRAC_TABLE.get(end - start).copied().unwrap_or(0));
        if intensity_rsv > total {
            intensity_rsv = 0;
        } else {
            total -= intensity_rsv;
            dual_stereo_rsv = if total >= 1 << BITRES { 1 << BITRES } else { 0 };
            total -= dual_stereo_rsv;
        }
    }

    let mut thresh = vec![0i32; len];
    let mut trim_offset = vec![0i32; len];
    for j in start..end {
        let n = i32::from(ebands[j + 1] - ebands[j]);
        thresh[j] = (channels << BITRES).max(((3 * n) << lm << BITRES) >> 4);
        trim_offset[j] = (channels * n * (alloc_trim - 5 - lm) * (end as i32 - j as i32 - 1) * (1 << (lm + BITRES as i32))) >> 6;
        if n << lm == 1 {
            trim_offset[j] -= channels << BITRES;
        }
    }

    let n_alloc_vectors = BAND_ALLOCATION.len() as i32;
    let mut lo = 1i32;
    let mut hi = n_alloc_vectors - 1;
    while lo <= hi {
        let mid = (lo + hi) >> 1;
        let mut psum = 0i32;
        let mut done = false;
        for j in (start..end).rev() {
            let n = i32::from(ebands[j + 1] - ebands[j]);
            let mut bitsj = ((channels * n * i32::from(BAND_ALLOCATION[mid as usize][j])) << lm) >> 2;
            if bitsj > 0 {
                bitsj = (bitsj + trim_offset[j]).max(0);
            }
            bitsj += offsets[j];
            if bitsj >= thresh[j] || done {
                done = true;
                psum += bitsj.min(cap[j]);
            } else if bitsj >= channels << BITRES {
                psum += channels << BITRES;
            }
        }
        if psum > total {
            hi = mid - 1;
        } else {
            lo = mid + 1;
        }
    }
    hi = lo;
    lo -= 1;

    let mut bits1 = vec![0i32; len];
    let mut bits2 = vec![0i32; len];
    for j in start..end {
        let n = i32::from(ebands[j + 1] - ebands[j]);
        let mut bits1j = ((channels * n * i32::from(BAND_ALLOCATION[lo as usize][j])) << lm) >> 2;
        let mut bits2j = if hi >= n_alloc_vectors {
            cap[j]
        } else {
            ((channels * n * i32::from(BAND_ALLOCATION[hi as usize][j])) << lm) >> 2
        };
        if bits1j > 0 {
            bits1j = (bits1j + trim_offset[j]).max(0);
        }
        if bits2j > 0 {
            bits2j = (bits2j + trim_offset[j]).max(0);
        }
        if lo > 0 {
            bits1j += offsets[j];
        }
        bits2j += offsets[j];
        if offsets[j] > 0 {
            skip_start = j as i32;
        }
        bits2j = (bits2j - bits1j).max(0);
        bits1[j] = bits1j;
        bits2[j] = bits2j;
    }

    // interp_bits2pulses
    let alloc_floor = channels << BITRES;
    let logm = lm << BITRES;
    let mut lo2 = 0i32;
    let mut hi2 = 1 << ALLOC_STEPS;
    for _ in 0..ALLOC_STEPS {
        let mid = (lo2 + hi2) >> 1;
        let mut psum = 0i32;
        let mut done = false;
        for j in (start..end).rev() {
            let tmp = bits1[j] + ((mid * bits2[j]) >> ALLOC_STEPS);
            if tmp >= thresh[j] || done {
                done = true;
                psum += tmp.min(cap[j]);
            } else if tmp >= alloc_floor {
                psum += alloc_floor;
            }
        }
        if psum > total {
            hi2 = mid;
        } else {
            lo2 = mid;
        }
    }
    let mut psum = 0i32;
    let mut done = false;
    let mut bits = vec![0i32; len];
    for j in (start..end).rev() {
        let mut tmp = bits1[j] + ((lo2 * bits2[j]) >> ALLOC_STEPS);
        if tmp < thresh[j] && !done {
            tmp = if tmp >= alloc_floor { alloc_floor } else { 0 };
        } else {
            done = true;
        }
        tmp = tmp.min(cap[j]);
        bits[j] = tmp;
        psum += tmp;
    }

    let mut coded_bands = end as i32;
    let mut intensity_rsv_mut = intensity_rsv;
    loop {
        let j = coded_bands - 1;
        if j <= skip_start {
            total += skip_rsv;
            break;
        }
        let left = total - psum;
        let percoeff = left / i32::from(ebands[coded_bands as usize] - ebands[start]);
        let left = left - i32::from(ebands[coded_bands as usize] - ebands[start]) * percoeff;
        let rem = (left - i32::from(ebands[j as usize] - ebands[start])).max(0);
        let band_width = i32::from(ebands[coded_bands as usize] - ebands[j as usize]);
        let mut band_bits = bits[j as usize] + percoeff * band_width + rem;
        if band_bits >= thresh[j as usize].max(alloc_floor + (1 << BITRES)) {
            if dec.bit_logp(1) {
                break;
            }
            psum += 1 << BITRES;
            band_bits -= 1 << BITRES;
        }
        psum -= bits[j as usize] + intensity_rsv_mut;
        if intensity_rsv_mut > 0 {
            intensity_rsv_mut = i32::from(LOG2_FRAC_TABLE.get((j - start as i32) as usize).copied().unwrap_or(0));
        }
        psum += intensity_rsv_mut;
        if band_bits >= alloc_floor {
            psum += alloc_floor;
            bits[j as usize] = alloc_floor;
        } else {
            bits[j as usize] = 0;
        }
        coded_bands -= 1;
    }
    let intensity_rsv = intensity_rsv_mut;

    let mut intensity = 0i32;
    if intensity_rsv > 0 {
        intensity = start as i32 + dec.dec_uint((coded_bands + 1 - start as i32).max(1) as u32).unwrap_or(0) as i32;
    }
    if intensity <= start as i32 {
        total += dual_stereo_rsv;
        dual_stereo_rsv = 0;
    }
    let dual_stereo = if dual_stereo_rsv > 0 { dec.bit_logp(1) } else { false };

    let mut left = total - psum;
    let percoeff = left / i32::from(ebands[coded_bands as usize] - ebands[start]);
    left -= i32::from(ebands[coded_bands as usize] - ebands[start]) * percoeff;
    for j in start..coded_bands as usize {
        bits[j] += percoeff * i32::from(ebands[j + 1] - ebands[j]);
    }
    for j in start..coded_bands as usize {
        let tmp = left.min(i32::from(ebands[j + 1] - ebands[j]));
        bits[j] += tmp;
        left -= tmp;
    }

    let mut balance = 0i32;
    let mut fine_energy = vec![0i32; len];
    let mut fine_priority = vec![false; len];
    for j in start..coded_bands as usize {
        let n0 = i32::from(ebands[j + 1] - ebands[j]);
        let n = n0 << lm;
        bits[j] += balance;
        let excess;
        if n > 1 {
            excess = (bits[j] - cap[j]).max(0);
            bits[j] -= excess;
            let den = channels * n + i32::from(channels == 2 && n > 2 && !dual_stereo && (j as i32) < intensity);
            let nclogn = den * (i32::from(crate::celt::tables::LOG_N[j]) + logm);
            let mut offset = (nclogn >> 1) - den * FINE_OFFSET;
            if n == 2 {
                offset += (den << BITRES) >> 2;
            }
            if bits[j] + offset < (den * 2) << BITRES {
                offset += nclogn >> 2;
            } else if bits[j] + offset < (den * 3) << BITRES {
                offset += nclogn >> 3;
            }
            let mut ebits = 0.max((bits[j] + offset + (den << (BITRES - 1))) / (den << BITRES));
            if channels * ebits > (bits[j] >> BITRES) {
                ebits = (bits[j] >> i32::from(channels == 2)) >> BITRES;
            }
            ebits = ebits.min(MAX_FINE_BITS);
            fine_priority[j] = ebits * (den << BITRES) >= bits[j] + offset;
            bits[j] -= (channels * ebits) << BITRES;
            fine_energy[j] = ebits;
        } else {
            excess = (bits[j] - (channels << BITRES)).max(0);
            bits[j] -= excess;
            fine_energy[j] = 0;
            fine_priority[j] = true;
        }
        if excess > 0 {
            let stereo_shift = i32::from(channels == 2);
            let extra_fine = (excess >> (stereo_shift + BITRES as i32)).min(MAX_FINE_BITS - fine_energy[j]);
            fine_energy[j] += extra_fine;
            let extra_bits = (extra_fine * channels) << BITRES;
            fine_priority[j] = extra_bits >= excess - balance;
            balance = excess - extra_bits;
        } else {
            balance = excess;
        }
    }
    for j in coded_bands as usize..end {
        let stereo_shift = i32::from(channels == 2);
        fine_energy[j] = (bits[j] >> stereo_shift) >> BITRES;
        bits[j] = 0;
        fine_priority[j] = fine_energy[j] < 1;
    }

    Allocation {
        pulses: bits,
        fine_energy,
        fine_priority,
        intensity,
        dual_stereo,
        balance,
        coded_bands,
    }
}
