//! Exponent strategies and grouped differential exponent decode.
//! ATSC A/52:2018 §7.3.

use vaco_bitstream::BitReader;

use crate::tables::{dexp, group_size};

/// Per-block exponent strategy. `Reuse` means "same as the previous block
/// carrying this channel" — the caller supplies the previous exponents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpStrategy {
    Reuse,
    D15,
    D25,
    D45,
}

impl ExpStrategy {
    #[must_use]
    pub const fn from_bits(code: u32) -> Self {
        match code {
            1 => Self::D15,
            2 => Self::D25,
            3 => Self::D45,
            _ => Self::Reuse,
        }
    }
}

/// Decode one channel's exponents for a block, given how many coefficients
/// it covers (`n`) and the strategy. `absexp_bits` is 4 for main/coupling
/// channels; the LFE channel's own `nlfegrps` is fixed at 2 groups of 4
/// coefficients (§7.3.3) and is handled by the caller passing `n = 7`
/// (`LFE_COEFFS`), which this function treats no differently.
///
/// Returns one exponent (0..=24) per coefficient in `0..n`, and how many
/// bits were consumed.
#[must_use]
pub fn decode(r: &mut BitReader<'_>, n: usize, strategy: ExpStrategy) -> (Vec<u8>, u32) {
    let group = group_size(match strategy {
        // Caller does not call `decode` for `Reuse`; `D15`'s code is what
        // `group_size` treats as the "otherwise" default too.
        ExpStrategy::Reuse | ExpStrategy::D15 => 1,
        ExpStrategy::D25 => 2,
        ExpStrategy::D45 => 3,
    });
    let ngrps = n.div_ceil(group as usize);
    let start_pos = r.bit_pos();

    let absexp = u8::try_from(r.get(4)).unwrap_or(0);
    let mut group_exps = Vec::new();
    group_exps.push(absexp);
    let mut i = 0usize;
    while i < ngrps {
        let code = u8::try_from(r.get(7)).unwrap_or(0);
        let [d0, d1, d2] = dexp(code);
        for d in [d0, d1, d2] {
            if i >= ngrps {
                break;
            }
            let prev = *group_exps.last().unwrap_or(&0);
            let next = (i64::from(prev) + i64::from(d)).clamp(0, 24);
            group_exps.push(u8::try_from(next).unwrap_or(0));
            i += 1;
        }
    }
    group_exps.remove(0);

    let mut out = vec![0u8; n];
    for (g, &e) in group_exps.iter().enumerate() {
        let base = g * group as usize;
        for k in 0..group as usize {
            if let Some(slot) = out.get_mut(base + k) {
                *slot = e;
            }
        }
    }
    let bits = u32::try_from(r.bit_pos().saturating_sub(start_pos)).unwrap_or(0);
    (out, bits)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code"
)]
mod tests {
    use super::*;

    #[test]
    fn a_flat_exponent_run_decodes_to_a_constant_array() {
        // absexp=0 (4 bits = 0b0000), then dexp code for delta {0,0,0} is
        // (0+2)+(0+2)*5+(0+2)*25 = 2+10+50 = 62.
        let mut bits = vec![false; 4];
        let code = 62u32;
        for b in (0..7).rev() {
            bits.push((code >> b) & 1 != 0);
        }
        // pad plenty more zero groups
        for _ in 0..(7 * 10) {
            let d2 = 2u32;
            for b in (0..7).rev() {
                bits.push((d2 >> b) & 1 != 0);
            }
        }
        let mut buf = vec![0u8; bits.len().div_ceil(8)];
        for (i, &b) in bits.iter().enumerate() {
            if b {
                buf[i / 8] |= 0x80 >> (i % 8);
            }
        }
        let mut r = BitReader::new(&buf);
        let (out, _bits) = decode(&mut r, 8, ExpStrategy::D15);
        assert!(out.iter().all(|&e| e == 0));
    }

    #[test]
    fn never_panics_on_a_truncated_buffer() {
        let mut r = BitReader::new(&[0u8; 1]);
        let _ = decode(&mut r, 40, ExpStrategy::D45);
    }
}
