//! Rice/Golomb residual coding.
//!
//! Simplified relative to what a maximally-efficient encoder would do:
//! every subframe's residual is written as a single partition (partition
//! order 0), never the escaped/unencoded form. Both are spec-legal choices
//! (RFC 9639 §9.2.7 requires only that a partition order divide the block
//! size evenly and leave more residual samples than the predictor order in
//! the first partition — 0 always satisfies that), just not the most
//! compact one a real encoder would reach for. See the crate-level doc for
//! why: the escape code (`0b1111`) is deliberately never used either,
//! because this project's own decode boundary (Claxon 0.4.3) does not
//! implement it and returns `Error::Unsupported` if it appears.
//!
//! Vaco-Spec-Ref: rfc-9639-flac Section 9.2.7, "Coded Residual"

use vaco_bitstream::BitWriter;

/// The largest Rice parameter this crate will ever choose. One below
/// `0b1111` (15), which is reserved for the escaped/unencoded partition
/// this crate never writes (see the module doc).
const MAX_PARAM: u32 = 14;

/// Signed-to-unsigned "folding" (zigzag): `0, -1, 1, -2, 2, ... -> 0, 1, 2,
/// 3, 4, ...`. Computed in `i64` so the shift can never overflow, then cast
/// down — the result always fits `u32` because `r` itself fits `i32`.
///
/// Vaco-Spec-Ref: rfc-9639-flac Section 9.2.7.2, "Rice Code"
#[must_use]
pub fn fold(r: i32) -> u32 {
    let r = i64::from(r);
    (((r << 1) ^ (r >> 63)) & 0xFFFF_FFFF) as u32
}

/// The number of bits a Rice code with parameter `k` needs for one already-
/// folded value: `k` remainder bits plus `folded >> k` unary zero bits plus
/// the unary stop bit.
fn code_len(folded: u32, k: u32) -> u64 {
    u64::from(folded >> k) + 1 + u64::from(k)
}

/// Total bits `residuals` would need at Rice parameter `k`, folding as it
/// goes.
fn total_bits_at(residuals: &[i32], k: u32) -> u64 {
    let mut total = 0u64;
    for &r in residuals {
        total = total.saturating_add(code_len(fold(r), k));
    }
    total
}

/// The Rice parameter in `0..=14` minimizing the total encoded size of
/// `residuals`, and that size.
///
/// An exhaustive search over the 15 legal values — cheap for one block's
/// worth of residuals, and exact rather than the usual mean-based estimate,
/// which matters because this crate needs to *compare* the result against
/// other subframe encodings (`VERBATIM`, other predictor orders) to pick
/// the smallest, not just pick a merely-good parameter.
#[must_use]
pub fn best_parameter(residuals: &[i32]) -> (u32, u64) {
    let mut best_k = 0u32;
    let mut best_bits = total_bits_at(residuals, 0);
    let mut k = 1u32;
    while k <= MAX_PARAM {
        let bits = total_bits_at(residuals, k);
        if bits < best_bits {
            best_bits = bits;
            best_k = k;
        }
        k += 1;
    }
    (best_k, best_bits)
}

/// Bits a single-partition coded residual of `residuals` needs, including
/// the 2-bit coding-method field, the 4-bit partition order and the 4-bit
/// Rice parameter — i.e. the real total this subframe's residual costs.
#[must_use]
pub fn encoded_len_bits(residuals: &[i32]) -> u64 {
    let (_, bits) = best_parameter(residuals);
    2 + 4 + 4 + bits
}

/// Write `residuals` as a single-partition, 4-bit-parameter coded residual.
pub fn write(bw: &mut BitWriter, residuals: &[i32]) {
    bw.put(2, 0); // Coding method: 4-bit Rice parameters.
    bw.put(4, 0); // Partition order 0: exactly one partition.
    let (k, _) = best_parameter(residuals);
    bw.put(4, k);
    for &r in residuals {
        let folded = fold(r);
        let quotient = folded >> k;
        bw.put_zeros(quotient);
        bw.put(1, 1);
        if k > 0 {
            bw.put(k, folded);
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_wrap,
    clippy::integer_division,
    reason = "test code: hand-rolled reference decoder and boundary values"
)]
mod tests {
    use super::{fold, write};
    use vaco_bitstream::{BitReader, BitWriter};

    #[test]
    fn fold_matches_the_spec_table() {
        assert_eq!(fold(0), 0);
        assert_eq!(fold(-1), 1);
        assert_eq!(fold(1), 2);
        assert_eq!(fold(-2), 3);
        assert_eq!(fold(2), 4);
    }

    #[test]
    fn fold_handles_the_i32_extremes_without_panicking() {
        assert_eq!(fold(i32::MAX), u32::MAX - 1);
        // i32::MIN itself is excluded from real residuals by the encoder
        // (RFC 9639 §9.2.7.3), but `fold` must still not panic on it.
        assert_eq!(fold(i32::MIN), u32::MAX);
    }

    /// Decode one Rice-coded value by hand (unary quotient, `k`-bit
    /// remainder, un-fold), independently of [`write`]'s own `fold`, to
    /// check the bitstream it produces is actually readable rather than
    /// merely self-consistent.
    fn read_one(r: &mut BitReader<'_>, k: u32) -> i32 {
        let mut q = 0u32;
        while r.get(1) == 0 {
            q += 1;
        }
        let rem = if k > 0 { r.get(k) } else { 0 };
        let folded = (q << k) | rem;
        if folded & 1 == 0 {
            (folded >> 1) as i32
        } else {
            -1 - ((folded >> 1) as i32)
        }
    }

    #[test]
    fn write_then_hand_decode_round_trips() {
        let residuals = [0, 1, -1, 2, -2, 100, -100, 12345, -12345];
        let mut bw = BitWriter::new();
        write(&mut bw, &residuals);
        let bytes = bw.finish();
        let mut r = BitReader::new(&bytes);
        let _method = r.get(2);
        let _order = r.get(4);
        let k = r.get(4);
        let mut got = Vec::new();
        for _ in 0..residuals.len() {
            got.push(read_one(&mut r, k));
        }
        assert_eq!(got, residuals);
    }

    #[test]
    fn never_emits_the_reserved_escape_parameter() {
        // Even a residual so large that no small k is efficient must not
        // pick parameter 15 (Claxon's decode boundary treats that as
        // `Error::Unsupported`, see the module doc).
        let residuals = [i32::MAX / 2, i32::MIN / 2 + 1];
        let (k, _) = super::best_parameter(&residuals);
        assert!(k <= 14);
    }
}
