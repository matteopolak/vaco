//! Mantissa VLC read and dequantisation, driven by the `bap` array
//! [`crate::bitalloc`] computed. ATSC A/52:2018 §7.4.

use vaco_bitstream::BitReader;

use crate::tables::{Quant, quant_for_bap};

/// Dequantise one uniform-quantizer code (bap 3, 5..=15) to a signed
/// fraction in `(-1, 1)`. Two's-complement-shaped: values `0..levels/2` are
/// negative, matching every independent description of AC-3's mantissa
/// coding (the MSB is a sign bit).
#[allow(
    clippy::integer_division,
    reason = "levels is always a power of two (1 << bits), so halving is exact"
)]
fn dequant_uniform(code: u32, bits: u8) -> f32 {
    let levels = 1u32 << bits;
    let half = levels / 2;
    (i64::from(code) - i64::from(half)) as f32 / half as f32
}

/// Dequantise one grouped-quantizer level (bap 1/2/4) to a signed fraction,
/// treating the `levels` values as evenly spaced across `(-1, 1)` — an
/// engineering approximation of the standard's perceptually-optimised
/// non-uniform step sizes for these three small quantizers (see the crate
/// root docs: this is the one dequantisation detail known to diverge from
/// the specification's exact values, even where the bitstream itself is
/// read correctly).
fn dequant_grouped(level: u32, levels: u16) -> f32 {
    let levels = f32::from(levels);
    (2.0 * level as f32 - (levels - 1.0)) / levels
}

/// Read and dequantise `bap.len()` mantissas in order, applying each one's
/// exponent to produce a coefficient. `dither` supplies a caller-chosen
/// pseudo-random value in `(-1, 1)` for `bap == 0` positions when `dithflag`
/// is set (§7.4.5); pass a function returning `0.0` to disable dither.
#[must_use]
pub fn decode(
    r: &mut BitReader<'_>,
    bap: &[u8],
    exps: &[u8],
    dither: bool,
    mut rng: impl FnMut() -> f32,
) -> Vec<f32> {
    let mut out = Vec::new();
    let mut pending_group: Vec<f32> = Vec::new();
    for (i, &b) in bap.iter().enumerate() {
        let exp = exps.get(i).copied().unwrap_or(24);
        let scale = 2f32.powi(-i32::from(exp));

        if let Some(v) = pending_group.pop() {
            out.push(v * scale);
            continue;
        }

        let value = match quant_for_bap(b) {
            Quant::Zero => {
                if dither {
                    rng()
                } else {
                    0.0
                }
            }
            Quant::Uniform { bits } => dequant_uniform(r.get(u32::from(bits)), bits),
            Quant::Grouped {
                levels,
                per_group,
                bits,
            } => {
                let code = r.get(u32::from(bits));
                let mut levels_out = decompose_group(code, levels, per_group);
                let first = levels_out.pop().unwrap_or(0);
                // `decompose_group` returns least-significant value last;
                // reverse so `pending_group.pop()` yields them in bitstream
                // (most-significant-first) order on subsequent iterations.
                levels_out.reverse();
                pending_group = levels_out
                    .into_iter()
                    .map(|lvl| dequant_grouped(lvl, levels))
                    .collect();
                dequant_grouped(first, levels)
            }
        };
        out.push(value * scale);
    }
    out
}

/// Base-`levels` decomposition of a grouped code into `count` digits, most
/// significant first — the inverse of how the encoder packs
/// `sum(digit[i] * levels^i)`.
fn decompose_group(mut code: u32, levels: u16, count: u8) -> Vec<u32> {
    let levels = u32::from(levels);
    let mut digits = Vec::new();
    for _ in 0..count {
        digits.push(code % levels);
        code /= levels;
    }
    digits.reverse();
    digits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_bap_with_no_dither_decodes_to_silence() {
        let mut r = BitReader::new(&[0u8; 4]);
        let out = decode(&mut r, &[0, 0, 0], &[10, 10, 10], false, || 1.0);
        assert!(out.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn grouped_mantissas_recover_the_right_count() {
        let mut r = BitReader::new(&[0u8; 8]);
        let bap = [1u8, 1, 1, 2, 2, 2];
        let exps = [0u8; 6];
        let out = decode(&mut r, &bap, &exps, false, || 0.0);
        assert_eq!(out.len(), 6);
    }

    #[test]
    fn never_panics_on_a_truncated_buffer() {
        let mut r = BitReader::new(&[]);
        let bap = [15u8; 20];
        let exps = [0u8; 20];
        let out = decode(&mut r, &bap, &exps, true, || 0.5);
        assert_eq!(out.len(), 20);
    }
}
