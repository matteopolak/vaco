//! A small, dependency-free, deterministic PRNG for [`crate::anoisesrc`]'s
//! `seed` option.
//!
//! Not an attempt to reproduce the reference's own bit stream — see
//! `vaco-filter-temporal::rng` and `vaco-filter-source::rng`'s identical
//! divergence, for the identical reason (D7/D17: matching an undocumented
//! `av_lfg` stream from black-box probing alone is not tractable, and
//! reproducibility, not bit-identity, is what a `seed` option is for).
//! `SplitMix64` (Vigna, public domain / CC0) again, for the same reasons.
#![allow(
    clippy::unreadable_literal,
    reason = "the SplitMix64 constants are the published magic numbers"
)]

#[derive(Debug, Clone, Copy)]
pub(crate) struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub(crate) const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    /// A uniform `f64` in `[-1, 1)`, for noise sample generation.
    pub(crate) fn next_bipolar(&mut self) -> f64 {
        let bits = self.next_u64() >> 11;
        #[allow(
            clippy::cast_precision_loss,
            reason = "53 significant bits fit exactly in an f64 mantissa"
        )]
        let unit = (bits as f64) * (1.0 / ((1u64 << 53) as f64));
        unit.mul_add(2.0, -1.0)
    }
}

pub(crate) const fn resolve_seed(seed: i64, fallback: u64) -> u64 {
    if seed < 0 {
        fallback
    } else {
        #[allow(clippy::cast_sign_loss, reason = "seed >= 0 was just checked")]
        {
            seed as u64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_reproduces_the_same_sequence() {
        let mut a = SplitMix64::new(42);
        let mut b = SplitMix64::new(42);
        let seq_a: Vec<u64> = (0..16).map(|_| a.next_u64()).collect();
        let seq_b: Vec<u64> = (0..16).map(|_| b.next_u64()).collect();
        assert_eq!(seq_a, seq_b);
    }

    #[test]
    fn next_bipolar_stays_in_range() {
        let mut r = SplitMix64::new(9);
        for _ in 0..1000 {
            let v = r.next_bipolar();
            assert!((-1.0..1.0).contains(&v), "{v}");
        }
    }

    #[test]
    fn resolve_seed_negative_uses_fallback() {
        assert_eq!(resolve_seed(-1, 99), 99);
        assert_eq!(resolve_seed(5, 99), 5);
    }
}
