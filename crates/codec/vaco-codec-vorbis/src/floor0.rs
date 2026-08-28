//! Floor type 0: LSP/LPC spectral envelope (spec section 6).
//!
//! Real encoders in this environment (`ffmpeg`'s native Vorbis encoder) only
//! ever emit Floor 1 — see this crate's differential test notes — so this
//! path is exercised by unit tests built from the spec's own formulas rather
//! than by a captured real-world stream.
//!
//! `Vaco-Spec-Ref: vorbis-i sections 6.2 and 9.2.1`

use vaco_core::{Error, Result};
use vaco_limits::Budget;

use crate::bitreader::{BitReaderLsb, ilog};
use crate::codebook::Codebook;

#[derive(Debug, Clone)]
pub(crate) struct Floor0Config {
    pub(crate) order: u32,
    pub(crate) rate: u32,
    pub(crate) bark_map_size: u32,
    pub(crate) amplitude_bits: u32,
    pub(crate) amplitude_offset: u32,
    pub(crate) book_list: Vec<u8>,
}

impl Floor0Config {
    pub(crate) fn parse_header(
        r: &mut BitReaderLsb<'_>,
        budget: &mut Budget,
        max_codebook: u32,
    ) -> Result<Self> {
        let order = r.get(8);
        let rate = r.get(16);
        let bark_map_size = r.get(16);
        let amplitude_bits = r.get(6);
        let amplitude_offset = r.get(8);
        let number_of_books = r.get(4).saturating_add(1);
        let mut book_list: Vec<u8> = budget.alloc(number_of_books as usize)?;
        for slot in &mut book_list {
            let b = r.get(8);
            if b > max_codebook {
                return Err(Error::InvalidData(
                    "vorbis: floor0 book number out of range",
                ));
            }
            *slot = b as u8;
        }
        if r.overran() {
            return Err(Error::InvalidData("vorbis: eop decoding floor0 header"));
        }
        Ok(Self {
            order,
            rate,
            bark_map_size,
            amplitude_bits,
            amplitude_offset,
            book_list,
        })
    }
}

/// Decoded per-packet floor0 state, or `Unused` (spec: amplitude of zero, or
/// end-of-packet during decode).
pub(crate) enum Floor0Decoded {
    Unused,
    Used {
        amplitude: u32,
        coefficients: Vec<f32>,
    },
}

/// Packet decode (spec section 6.2.2).
pub(crate) fn decode_packet(
    cfg: &Floor0Config,
    r: &mut BitReaderLsb<'_>,
    codebooks: &[Codebook],
    budget: &mut Budget,
) -> Result<Floor0Decoded> {
    let amplitude = r.get(cfg.amplitude_bits);
    if amplitude == 0 || r.overran() {
        return Ok(Floor0Decoded::Unused);
    }
    let mut coefficients: Vec<f32> = Vec::new();
    let book_bits = ilog(i64::from(cfg.book_list.len().saturating_sub(1) as u32));
    loop {
        let booknumber = r.get(book_bits.max(1));
        let Some(book) = cfg
            .book_list
            .get(booknumber as usize)
            .and_then(|&b| codebooks.get(b as usize))
        else {
            return Ok(Floor0Decoded::Unused);
        };
        if !book.has_lookup() {
            return Ok(Floor0Decoded::Unused);
        }
        let Some(temp_vector) = book.decode_vector(r) else {
            return Ok(Floor0Decoded::Unused);
        };
        budget.consume_fuel(u64::try_from(temp_vector.len()).unwrap_or(u64::MAX))?;
        let mut last = coefficients.last().copied().unwrap_or(0.0);
        for &v in temp_vector {
            last += v;
            coefficients.push(last);
            budget.charge(4)?;
        }
        if r.overran() {
            return Ok(Floor0Decoded::Unused);
        }
        if coefficients.len() as u32 >= cfg.order {
            break;
        }
    }
    Ok(Floor0Decoded::Used {
        amplitude,
        coefficients,
    })
}

/// `bark(x)` (spec section 6.2.3, as corrected by errata 20150227).
fn bark(x: f64) -> f64 {
    13.1 * (0.00074 * x).atan() + 2.24 * (0.000_000_018_5 * x * x).atan() + 0.0001 * x
}

/// Curve computation (spec section 6.2.3): synthesize an `n`-element linear
/// spectral envelope from the decoded LSP coefficients.
#[allow(
    clippy::many_single_char_names,
    clippy::integer_division,
    clippy::manual_midpoint,
    reason = "matches the spec's own p/q/w notation and its exact-floor-division loop bounds (section 6.2.3, odd/even order cases)"
)]
pub(crate) fn compute_curve(
    cfg: &Floor0Config,
    amplitude: u32,
    coefficients: &[f32],
    n: usize,
) -> Vec<f32> {
    let mut out = vec![0f32; n];
    if amplitude == 0 || cfg.order == 0 {
        return out;
    }
    let order = cfg.order as usize;
    let bark_max = bark(0.5 * f64::from(cfg.rate));
    let map: Vec<u32> = (0..n)
        .map(|i| {
            let foobar = (bark((f64::from(cfg.rate) * i as f64) / (2.0 * n as f64))
                * f64::from(cfg.bark_map_size)
                / bark_max.max(f64::MIN_POSITIVE)) as u32;
            foobar.min(cfg.bark_map_size.saturating_sub(1))
        })
        .collect();

    let coeff: Vec<f64> = coefficients
        .iter()
        .take(order)
        .map(|&v| f64::from(v))
        .collect();
    let max_amp_value = (1u64 << cfg.amplitude_bits.min(31))
        .saturating_sub(1)
        .max(1) as f64;

    let mut i = 0usize;
    while i < n {
        let map_i = *map.get(i).unwrap_or(&0);
        let omega = std::f64::consts::PI * f64::from(map_i) / f64::from(cfg.bark_map_size.max(1));
        let cos_w = omega.cos();

        let (p, q) = if order % 2 == 1 {
            let half = (order.saturating_sub(3)) / 2;
            let mut p = 1.0 - cos_w * cos_w;
            for j in 0..=half {
                let c = coeff.get(2 * j + 1).copied().unwrap_or(0.0);
                let d = c.cos() - cos_w;
                p *= 4.0 * d * d;
            }
            let mut q = 0.25;
            for j in 0..(order.saturating_sub(1)) / 2 {
                let c = coeff.get(2 * j).copied().unwrap_or(0.0);
                let d = c.cos() - cos_w;
                q *= 4.0 * d * d;
            }
            (p, q)
        } else {
            let mut p = (1.0 - cos_w) / 2.0;
            for j in 0..order.saturating_sub(2) / 2 {
                let c = coeff.get(2 * j + 1).copied().unwrap_or(0.0);
                let d = c.cos() - cos_w;
                p *= 4.0 * d * d;
            }
            let mut q = (1.0 + cos_w) / 2.0;
            for j in 0..order.saturating_sub(2) / 2 {
                let c = coeff.get(2 * j).copied().unwrap_or(0.0);
                let d = c.cos() - cos_w;
                q *= 4.0 * d * d;
            }
            (p, q)
        };

        let denom = (p + q).max(f64::MIN_POSITIVE);
        let inner =
            f64::from(amplitude) * f64::from(cfg.amplitude_offset) / (max_amp_value * denom.sqrt());
        let linear_floor_value = (0.115_129_25 * inner).exp();

        let iteration_condition = map_i;
        loop {
            if let Some(o) = out.get_mut(i) {
                *o = linear_floor_value as f32;
            }
            i += 1;
            if i >= n || *map.get(i).unwrap_or(&u32::MAX) != iteration_condition {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn zero_amplitude_produces_a_silent_curve() {
        let cfg = Floor0Config {
            order: 4,
            rate: 44100,
            bark_map_size: 64,
            amplitude_bits: 6,
            amplitude_offset: 0,
            book_list: vec![],
        };
        let curve = compute_curve(&cfg, 0, &[], 32);
        assert!(curve.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn nonzero_amplitude_produces_a_finite_positive_curve() {
        let cfg = Floor0Config {
            order: 4,
            rate: 44100,
            bark_map_size: 64,
            amplitude_bits: 6,
            amplitude_offset: 20,
            book_list: vec![],
        };
        let coefficients = [0.3f32, 0.6, 1.0, 1.4];
        let curve = compute_curve(&cfg, 30, &coefficients, 64);
        assert_eq!(curve.len(), 64);
        for &v in &curve {
            assert!(v.is_finite());
            assert!(v > 0.0);
        }
    }
}
