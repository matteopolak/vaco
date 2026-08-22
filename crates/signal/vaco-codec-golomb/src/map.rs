//! The mappings themselves, with no bitstream attached.
//!
//! Every function here is a pure `u32`/`i32` transform taken directly from
//! ITU-T H.264 (03/2010) clause 9.1. Keeping them separate from the reads is
//! what makes them testable **against the standard** rather than only against
//! our own writer: clause 9.1.1's Table 9-3 is a list of `(codeNum, synElVal)`
//! pairs, and `se_value` can be checked against it one row at a time with no
//! bits involved.
//!
//! They are also what an encoder needs: `se_code_num` is the inverse mapping,
//! and the `*_bit_len` family answers "how many bits would this cost" without
//! writing anything, which is the question rate-distortion optimisation asks
//! several million times per frame.

/// The signed mapping, H.264 clause 9.1.1 equation 9-3.
///
/// `synElVal = (−1)^(k+1) · Ceil(k ÷ 2)`, which is the odd/even split below.
/// Table 9-3 of the same clause tabulates the first few rows and the unit tests
/// check against it directly.
///
/// # Saturation
///
/// `code_num == u32::MAX` would map to `2^31`, which is not an `i32`. It
/// saturates to [`i32::MAX`]. No `ue(v)` reader in this crate can produce that
/// code number — it needs a 32-zero prefix, which is rejected as malformed — so
/// the saturation is unreachable from a bitstream and exists only so the
/// function is total.
#[must_use]
#[inline]
pub const fn se_value(code_num: u32) -> i32 {
    // `k >> 1` fits an i32 for every u32 k: the maximum is 2^31 − 1.
    let half = (code_num >> 1).cast_signed();
    if code_num & 1 == 1 {
        half.saturating_add(1)
    } else {
        -half
    }
}

/// The inverse of [`se_value`]: the code number that encodes `value`.
///
/// H.264 clause 9.1.1 read backwards — positive values take the odd code
/// numbers, zero and negatives the even ones.
///
/// # Saturation
///
/// [`i32::MIN`] would need code number `2^32`, one past a `u32`. It saturates to
/// [`u32::MAX`], which is itself not encodable; [`crate::GolombEncode`] clamps
/// the input instead, so this is only reachable by calling the mapping directly.
#[must_use]
#[inline]
pub const fn se_code_num(value: i32) -> u32 {
    if value > 0 {
        // value <= 2^31 − 1, so 2·value − 1 <= 2^32 − 3: fits.
        (value as u32).wrapping_mul(2).wrapping_sub(1)
    } else {
        value.unsigned_abs().saturating_mul(2)
    }
}

/// Length in bits of the `ue(v)` codeword for `value`.
///
/// The codeword is `leadingZeroBits` zeros, a one, then `leadingZeroBits` more
/// bits, so it is always odd and equal to `2·bitlen(value + 1) − 1`.
///
/// Values needing a prefix longer than 31 zeros — that is, a length of 65 —
/// are *not* decodable by [`crate::GolombDecode::ue_v`], which rejects them as
/// malformed rather than looping. Only `u32::MAX` is in that position.
#[must_use]
#[inline]
pub const fn ue_bit_len(value: u32) -> u32 {
    // In u64 so that value == u32::MAX does not wrap to zero.
    127 - 2 * ((value as u64 + 1).leading_zeros())
}

/// Length in bits of the `se(v)` codeword for `value`.
#[must_use]
#[inline]
pub const fn se_bit_len(value: i32) -> u32 {
    ue_bit_len(se_code_num(value))
}

/// Length in bits of the order-`k` Exp-Golomb codeword for `value`.
///
/// `2·leadingZeroBits + k + 1`, where `leadingZeroBits = bitlen(value + 2^k) −
/// 1 − k`. Order 0 agrees with [`ue_bit_len`] by construction, which the
/// property tests assert.
///
/// Returns 0 for `k > 31`, which is not a usable order for a `u32` value.
#[must_use]
#[inline]
pub const fn ue_k_bit_len(value: u32, k: u32) -> u32 {
    if k > 31 {
        return 0;
    }
    let t = value as u64 + (1u64 << k);
    // `t >= 2^k`, so `ilog2` is defined and at least `k`.
    let lz = t.ilog2() - k;
    2 * lz + k + 1
}

/// Length in bits of the signed order-`k` codeword for `value`.
#[must_use]
#[inline]
pub const fn se_k_bit_len(value: i32, k: u32) -> u32 {
    ue_k_bit_len(se_code_num(value), k)
}

/// Length in bits of the 64-bit `ue(v)` codeword for `value`.
///
/// Prefixes longer than 63 zeros are rejected by
/// [`crate::GolombDecode::ue_v64`]; `u64::MAX` is the only value in that
/// position and reports 129 here.
#[must_use]
#[inline]
pub const fn ue_v64_bit_len(value: u64) -> u32 {
    match value.checked_add(1) {
        // 2·bitlen(code) − 1, computed in the 128-bit-free way.
        Some(code) => 127 - 2 * code.leading_zeros(),
        // u64::MAX: code is 2^64, bitlen 65.
        None => 129,
    }
}

/// Total `ue(v)` cost of a run of values, in bits.
///
/// The shape rate-distortion loops want: no branches, no lookups, one
/// `leading_zeros` per element and an accumulator, so LLVM vectorises it
/// (`clz` plus an integer add are both lane-wise operations on every target we
/// care about). Measured against the same loop written with a `Vec` of lengths
/// in `benches/golomb.rs`.
#[must_use]
pub fn ue_bits_total(values: &[u32]) -> u64 {
    values
        .iter()
        .map(|&v| u64::from(ue_bit_len(v)))
        .fold(0u64, u64::wrapping_add)
}

/// Per-element `ue(v)` lengths, written into `out`.
///
/// Processes `min(values.len(), out.len())` elements and returns that count, so
/// a short `out` truncates rather than panicking.
pub fn ue_bit_len_batch(values: &[u32], out: &mut [u32]) -> usize {
    let mut n = 0usize;
    for (o, &v) in out.iter_mut().zip(values.iter()) {
        *o = ue_bit_len(v);
        n += 1;
    }
    n
}
