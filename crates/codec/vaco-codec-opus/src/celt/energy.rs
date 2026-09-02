//! CELT band-energy decode: the coarse Laplace-coded delta, the fine
//! uniform bits, and the log-domain-to-linear conversion that turns them
//! into per-band gains. RFC 6716 §4.3.2, from `celt/quant_bands.c`'s decode
//! side (`unquant_coarse_energy`/`unquant_fine_energy`/
//! `unquant_energy_finalise`/`log2Amp`).
//!
//! All of it is float arithmetic here (the reference's `PSHR32`/`SHL32`/
//! `QCONST16` Q-format macros are no-ops on the float build — see
//! `arch.h`), except the Laplace/range-coder calls themselves, which stay
//! exact integer per [`crate::range`].

use crate::celt::tables::{BETA_COEF, BETA_INTRA, E_PROB_MODEL, PRED_COEF, SMALL_ENERGY_ICDF};
use crate::range::{RangeDecoder, laplace_decode};

/// `quant_bands.c`'s `unquant_coarse_energy`: decode the per-band log-energy
/// for one frame, in place. `old_band_e` holds the previous frame's decoded
/// energies on entry (channel-major, `nb_ebands` per channel) and this
/// frame's on exit.
pub fn unquant_coarse_energy(
    dec: &mut RangeDecoder<'_>,
    old_band_e: &mut [f32],
    nb_ebands: usize,
    start: usize,
    end: usize,
    intra: bool,
    channels: usize,
    lm: usize,
) {
    let prob_model = &E_PROB_MODEL[lm.min(3)][usize::from(intra)];
    let (coef, beta) = if intra {
        (0.0, BETA_INTRA)
    } else {
        (PRED_COEF[lm.min(3)], BETA_COEF[lm.min(3)])
    };
    let budget = (dec.storage() as i32).saturating_mul(8);
    let mut prev = [0.0f32; 2];

    for i in start..end {
        for c in 0..channels {
            let tell = dec.tell();
            let qi = if budget - tell >= 15 {
                let pi = 2 * i.min(20);
                let fs0 = u32::from(prob_model.get(pi).copied().unwrap_or(0)) << 7;
                let decay = u32::from(prob_model.get(pi + 1).copied().unwrap_or(0)) << 6;
                laplace_decode(dec, fs0, decay)
            } else if budget - tell >= 2 {
                let qi = dec.icdf(&SMALL_ENERGY_ICDF, 2).unwrap_or(0);
                (qi >> 1) ^ -(qi & 1)
            } else if budget - tell >= 1 {
                -i32::from(dec.bit_logp(1))
            } else {
                -1
            };
            let q = qi as f32;
            let idx = i + c * nb_ebands;
            let Some(prev_e) = prev.get_mut(c) else {
                continue;
            };
            let old = old_band_e.get(idx).copied().unwrap_or(-9.0).max(-9.0);
            // `quant_bands.c` only clamps this to `+-GCONST(28.f)` inside
            // `#ifdef FIXED_POINT`, where it exists to stop the Q-format
            // accumulator overflowing -- a no-op on the reference's own
            // float build, since ordinary content never drives it that far.
            // `laplace_decode`'s escape mechanism can still return an
            // arbitrarily large `qi` given enough (adversarial) input bits,
            // which `exp2` downstream would turn into `inf`/`NaN`; clamping
            // unconditionally keeps that reachable-but-pathological case
            // finite without changing anything for real content.
            let tmp = (coef * old + *prev_e + q).clamp(-28.0, 28.0);
            if let Some(slot) = old_band_e.get_mut(idx) {
                *slot = tmp;
            }
            *prev_e = *prev_e + q - beta * q;
        }
    }
}

/// `quant_bands.c`'s `unquant_fine_energy`: the uniform-probability bits
/// spent per band (`fine_energy[i]` bits each), read straight off the back
/// of the buffer.
pub fn unquant_fine_energy(
    dec: &mut RangeDecoder<'_>,
    old_band_e: &mut [f32],
    nb_ebands: usize,
    start: usize,
    end: usize,
    channels: usize,
    fine_energy: &[i32],
) {
    for i in start..end {
        let bits = fine_energy.get(i).copied().unwrap_or(0);
        if bits <= 0 {
            continue;
        }
        // `quant_bands.c`'s `unquant_fine_energy`: skip a band whose raw
        // bits would run past the frame's own byte budget rather than
        // reading them anyway -- `dec_bits` pulls from the same
        // shared raw-bit region every other read in the frame does, so an
        // extra read here shifts every later one.
        if dec.tell() + (channels as i32) * bits > (dec.storage() as i32) * 8 {
            continue;
        }
        for c in 0..channels {
            let q2 = dec.dec_bits(bits as u32);
            let frac = f32::from(1u16 << bits.min(15));
            let offset = (f32::from(q2 as u16) + 0.5) / frac - 0.5;
            let idx = i + c * nb_ebands;
            if let Some(slot) = old_band_e.get_mut(idx) {
                *slot += offset;
            }
        }
    }
}

/// `quant_bands.c`'s `unquant_energy_finalise`: spend any bits left over
/// after fine energy on one more sign-only refinement per band, highest
/// [`crate::celt::rate::Allocation::fine_priority`] first.
pub fn unquant_energy_finalise(
    dec: &mut RangeDecoder<'_>,
    old_band_e: &mut [f32],
    nb_ebands: usize,
    start: usize,
    end: usize,
    channels: usize,
    fine_energy: &[i32],
    fine_priority: &[bool],
    mut bits_left: i32,
) {
    for prio in 0..2 {
        for i in start..end {
            if bits_left < channels as i32 {
                break;
            }
            let bits = fine_energy.get(i).copied().unwrap_or(0);
            let is_prio = fine_priority.get(i).copied().unwrap_or(false);
            if bits >= 8 || is_prio != (prio == 1) {
                continue;
            }
            for c in 0..channels {
                let q2 = dec.dec_bits(1);
                let offset = (f32::from(q2 as u16) - 0.5) / f32::from(1u16 << (bits + 1).min(15));
                let idx = i + c * nb_ebands;
                if let Some(slot) = old_band_e.get_mut(idx) {
                    *slot += offset;
                }
                bits_left -= 1;
            }
        }
    }
}

/// `quant_bands.c`'s `log2Amp`: log-domain band energy to linear amplitude.
pub fn log2_amp(
    old_band_e: &[f32],
    band_e: &mut [f32],
    nb_ebands: usize,
    start: usize,
    end: usize,
    channels: usize,
) {
    use crate::celt::tables::E_MEANS;
    for c in 0..channels {
        for i in 0..nb_ebands {
            let idx = i + c * nb_ebands;
            let amp = if i < start || i >= end {
                0.0
            } else {
                let lg = old_band_e.get(idx).copied().unwrap_or(0.0)
                    + E_MEANS.get(i).copied().unwrap_or(0.0);
                lg.exp2()
            };
            if let Some(slot) = band_e.get_mut(idx) {
                *slot = amp;
            }
        }
    }
}
