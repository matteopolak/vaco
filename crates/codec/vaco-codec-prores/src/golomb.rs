//! Golomb-Rice/exponential-Golomb combination codes, RDD 36 SS7.1.1.1/7.1.1.2.
//!
//! `ProRes` entropy coding uses one family of variable-length codes throughout:
//! a combination of a Golomb-Rice code (for small symbols) and an
//! exponential-Golomb code (for larger ones), selected by a codeword's own
//! unary prefix length against a per-context threshold `lastRiceQ`. See the
//! spec text quoted in this crate's top-level doc for the derivation this
//! module transcribes directly.
//!
//! A codeword's unary prefix is unbounded in principle; [`MAX_PREFIX_ZEROS`]
//! caps it so a truncated or adversarial bitstream fails cleanly instead of
//! looping (`BitReader::get` pads with zeros past the end of a buffer, so a
//! loop whose termination depends on seeing a `1` bit must use `try_get`,
//! which this module does throughout).

use vaco_bitstream::BitReader;
use vaco_core::{Error, Result};

/// No legitimate `ProRes` symbol (a 12-bit-range DCT coefficient difference, an
/// AC run bounded by 64 coefficients, or a level bounded by the same) needs a
/// unary prefix anywhere close to this long even at the coarsest codebook.
/// Bounds the counting loop against truncated/adversarial input.
const MAX_PREFIX_ZEROS: u32 = 40;

/// Count the `0` bits preceding the next `1` bit, consuming all of them
/// including the terminating `1`. Bounded by [`MAX_PREFIX_ZEROS`].
fn count_prefix(r: &mut BitReader<'_>) -> Result<u32> {
    let mut q = 0u32;
    loop {
        let bit = r
            .try_get(1)
            .map_err(|_| Error::InvalidData("prores: golomb code truncated"))?;
        if bit == 1 {
            return Ok(q);
        }
        q += 1;
        if q > MAX_PREFIX_ZEROS {
            return Err(Error::InvalidData("prores: golomb unary prefix too long"));
        }
    }
}

/// Decode a Golomb-Rice/exponential-Golomb combination code with parameters
/// `last_rice_q`, `k_rice`, `k_exp` (RDD 36 SS7.1.1.1's `lastRiceQ`, `kRice`,
/// `kExp`).
///
/// # Errors
/// [`Error::InvalidData`] on a truncated or malformed codeword.
pub(crate) fn combo(
    r: &mut BitReader<'_>,
    last_rice_q: u32,
    k_rice: u32,
    k_exp: u32,
) -> Result<u64> {
    let q = count_prefix(r)?;
    if q <= last_rice_q {
        let suffix = u64::from(
            r.try_get(k_rice)
                .map_err(|_| Error::InvalidData("prores: rice suffix truncated"))?,
        );
        let rice_step = 1u64
            .checked_shl(k_rice)
            .ok_or(Error::InvalidData("prores: rice shift overflow"))?;
        Ok(u64::from(q)
            .saturating_mul(rice_step)
            .saturating_add(suffix))
    } else {
        let q_inner = q - (last_rice_q + 1);
        let suffix_bits = q_inner.saturating_add(k_exp);
        if suffix_bits > 63 {
            return Err(Error::InvalidData("prores: exp-golomb suffix too wide"));
        }
        let suffix = u64::from(
            r.try_get(suffix_bits)
                .map_err(|_| Error::InvalidData("prores: exp-golomb suffix truncated"))?,
        );
        let hi = 1u64
            .checked_shl(q_inner.saturating_add(k_exp))
            .ok_or(Error::InvalidData("prores: exp-golomb shift overflow"))?;
        let lo = 1u64
            .checked_shl(k_exp)
            .ok_or(Error::InvalidData("prores: exp-golomb shift overflow"))?;
        let inner = suffix
            .checked_add(hi)
            .and_then(|v| v.checked_sub(lo))
            .ok_or(Error::InvalidData("prores: exp-golomb value overflow"))?;
        let escape = u64::from(last_rice_q + 1)
            .checked_mul(
                1u64.checked_shl(k_rice)
                    .ok_or(Error::InvalidData("prores: rice shift overflow"))?,
            )
            .ok_or(Error::InvalidData("prores: rice escape overflow"))?;
        inner
            .checked_add(escape)
            .ok_or(Error::InvalidData("prores: combo value overflow"))
    }
}

/// A standalone exponential-Golomb code of order `k` — the special case
/// `combo(0, k, k+1)` per the spec's own note in SS7.1.1.1, used directly for
/// `first_dc_coeff` (order 5).
///
/// # Errors
/// [`Error::InvalidData`] on a truncated or malformed codeword.
pub(crate) fn exp_golomb(r: &mut BitReader<'_>, k: u32) -> Result<u64> {
    combo(r, 0, k, k.saturating_add(1))
}

/// The signed integer-to-symbol mapping `S(n)`, RDD 36 Table 8: `2|n|` for
/// `n >= 0`, `2|n| - 1` for `n < 0`.
///
/// Decode only ever needs the inverse ([`symbol_to_signed`]) — this forward
/// direction exists to build synthetic combo-coded test vectors (both here
/// and in `coeff.rs`'s tests), which is the only reason it is `cfg(test)`
/// rather than dead weight in a decode-only crate.
#[cfg(test)]
pub(crate) fn signed_to_symbol(n: i64) -> u64 {
    if n >= 0 {
        (n as u64).saturating_mul(2)
    } else {
        n.unsigned_abs().saturating_mul(2).saturating_sub(1)
    }
}

/// The inverse of [`signed_to_symbol`]: even symbols are `S/2` (nonnegative),
/// odd symbols are `-((S+1)/2)` (negative).
pub(crate) fn symbol_to_signed(s: u64) -> i64 {
    if s.is_multiple_of(2) {
        #[allow(
            clippy::integer_division,
            reason = "s is even here, so s / 2 is exact per RDD 36 Table 8's own inverse mapping"
        )]
        (s / 2).cast_signed()
    } else {
        -(s.div_ceil(2).cast_signed())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_bitstream::BitWriter;

    fn write_combo(w: &mut BitWriter, last_rice_q: u32, k_rice: u32, k_exp: u32, n: u64) {
        // Encoder mirror of `combo`'s decode, built straight from the same
        // spec clause, used only to generate round-trip test vectors.
        let rice_max = (u64::from(last_rice_q) + 1) * (1u64 << k_rice);
        if n < rice_max {
            let q = n >> k_rice;
            let r = n & ((1u64 << k_rice) - 1);
            for _ in 0..q {
                w.put(1, 0);
            }
            w.put(1, 1);
            w.put(k_rice, r as u32);
        } else {
            for _ in 0..=last_rice_q {
                w.put(1, 0);
            }
            let inner = n - rice_max;
            // order-k_exp exp-golomb of `inner`
            let m = inner + (1u64 << k_exp);
            let bits = 64 - m.leading_zeros();
            let q = bits - 1 - k_exp;
            for _ in 0..q {
                w.put(1, 0);
            }
            for i in (0..bits).rev() {
                w.put(1, ((m >> i) & 1) as u32);
            }
        }
    }

    #[test]
    fn signed_symbol_round_trips() {
        for n in -50i64..50 {
            assert_eq!(symbol_to_signed(signed_to_symbol(n)), n);
        }
    }

    #[test]
    fn signed_symbol_matches_table_8() {
        assert_eq!(signed_to_symbol(0), 0);
        assert_eq!(signed_to_symbol(-1), 1);
        assert_eq!(signed_to_symbol(1), 2);
        assert_eq!(signed_to_symbol(-2), 3);
        assert_eq!(signed_to_symbol(2), 4);
        assert_eq!(signed_to_symbol(-3), 5);
        assert_eq!(signed_to_symbol(3), 6);
    }

    #[test]
    fn exp_golomb_round_trips_many_values() {
        for k in 0..4u32 {
            for n in 0..500u64 {
                let mut w = BitWriter::new();
                write_combo(&mut w, 0, k, k + 1, n);
                w.put(8, 0); // trailing pad so try_get never starves mid-suffix
                let bytes = w.finish();
                let mut r = BitReader::new(&bytes);
                let got = exp_golomb(&mut r, k).unwrap();
                assert_eq!(got, n, "k={k} n={n}");
            }
        }
    }

    #[test]
    fn combo_round_trips_many_values() {
        for (last_rice_q, k_rice, k_exp) in [(2u32, 0u32, 1u32), (1, 2, 3), (1, 1, 2), (2, 0, 2)] {
            for n in 0..1000u64 {
                let mut w = BitWriter::new();
                write_combo(&mut w, last_rice_q, k_rice, k_exp, n);
                w.put(8, 0);
                let bytes = w.finish();
                let mut r = BitReader::new(&bytes);
                let got = combo(&mut r, last_rice_q, k_rice, k_exp).unwrap();
                assert_eq!(
                    got, n,
                    "lastRiceQ={last_rice_q} kRice={k_rice} kExp={k_exp} n={n}"
                );
            }
        }
    }

    #[test]
    fn truncated_input_errors_not_hangs() {
        let bytes = [0u8; 4];
        let mut r = BitReader::new(&bytes);
        assert!(exp_golomb(&mut r, 3).is_err());
    }
}
