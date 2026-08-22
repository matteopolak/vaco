//! The encoder side: the inverse of every mapping in [`crate::read`].
//!
//! An extension trait on [`BitWriter`], with `put_` names so it never collides
//! with `BitWriter`'s own inherent `ue`/`se`. Where `BitWriter` already has the
//! code, these forward to it — there is no reason for two implementations of
//! `ue(v)` encoding in one workspace, and the round-trip property tests hold the
//! two crates together.

use vaco_bitstream::BitWriter;

use crate::map;
use crate::tables::{ChromaArrayType, MbPartPredMode, code_num_from_cbp};

/// Exp-Golomb writes, the inverse of [`GolombDecode`](crate::GolombDecode).
///
/// # Domain
///
/// Every method documents what it does with a value it cannot encode. The rule
/// is the same everywhere: **clamp and debug-assert, never panic**, because an
/// encoder handed a bad value by a buggy caller should produce a wrong
/// bitstream in release rather than take the process down.
pub trait GolombEncode {
    /// Write `value` as `ue(v)`.
    ///
    /// # Domain
    ///
    /// `0 ..= u32::MAX − 1`. `u32::MAX` needs a 32-zero prefix, which no reader
    /// in this workspace accepts; it clamps and debug-asserts.
    fn put_ue_v(&mut self, value: u32);

    /// Write `value` as `se(v)`.
    ///
    /// # Domain
    ///
    /// `−(2^31 − 1) ..= 2^31 − 1`. [`i32::MIN`] clamps and debug-asserts.
    fn put_se_v(&mut self, value: i32);

    /// Write `value` as `te(v)` with ceiling `c_max`.
    ///
    /// With `c_max <= 1` this writes one bit — the *inverse* of the value, per
    /// clause 9.1.1 — and a value above 1 clamps.
    fn put_te_v(&mut self, c_max: u32, value: u32);

    /// Write `coded_block_pattern` as `me(v)`, clause 9.1.2.
    ///
    /// Returns `false` and writes nothing if `cbp` has no Table 9-4 row for the
    /// given `chroma`/`pred` combination, which is the only way to say "that is
    /// not an encodable coded block pattern" without panicking.
    fn put_me_v(&mut self, chroma: ChromaArrayType, pred: MbPartPredMode, cbp: u32) -> bool;

    /// Write `value` as order-`k` Exp-Golomb. Order 0 is
    /// [`put_ue_v`](GolombEncode::put_ue_v).
    ///
    /// # Domain
    ///
    /// `k <= 31`, and a `value` whose codeword the matching reader would accept
    /// — that is, `ue_k_bit_len(value, k) <= 63`. Anything else clamps and
    /// debug-asserts.
    fn put_ue_k(&mut self, k: u32, value: u32);

    /// Write `value` as signed order-`k` Exp-Golomb.
    fn put_se_k(&mut self, k: u32, value: i32);

    /// Write `value` as a 64-bit `ue(v)`.
    ///
    /// # Domain
    ///
    /// `0 ..= u64::MAX − 1`; `u64::MAX` clamps and debug-asserts.
    fn put_ue_v64(&mut self, value: u64);
}

impl GolombEncode for BitWriter {
    #[inline]
    fn put_ue_v(&mut self, value: u32) {
        self.ue(value);
    }

    #[inline]
    fn put_se_v(&mut self, value: i32) {
        self.se(value);
    }

    #[inline]
    fn put_te_v(&mut self, c_max: u32, value: u32) {
        if c_max > 1 {
            self.ue(value.min(c_max));
        } else {
            self.put(1, 1 - value.min(1));
        }
    }

    fn put_me_v(&mut self, chroma: ChromaArrayType, pred: MbPartPredMode, cbp: u32) -> bool {
        match code_num_from_cbp(cbp, chroma, pred) {
            Some(code_num) => {
                self.ue(code_num);
                true
            }
            None => false,
        }
    }

    fn put_ue_k(&mut self, k: u32, value: u32) {
        if k == 0 {
            self.ue(value);
            return;
        }
        debug_assert!(k <= 31, "GolombEncode::put_ue_k: k must be <= 31, got {k}");
        if k > 31 {
            return;
        }
        // The prefix length: `bitlen(value + 2^k) − 1 − k`, the same derivation
        // `map::ue_k_bit_len` documents.
        let shifted = u64::from(value) + (1u64 << k);
        let lz = shifted.ilog2() - k;
        debug_assert!(
            lz <= 31 && lz + k <= 32,
            "GolombEncode::put_ue_k: value {value} is not decodable at order {k}"
        );
        if lz > 31 || lz + k > 32 {
            return;
        }
        // `value + 2^k` is `2^(lz+k) + suffix`; dropping its leading one bit and
        // writing `lz` zeros in front of it *is* the codeword.
        self.put_zeros(lz);
        self.put_long(lz + k + 1, shifted);
    }

    #[inline]
    fn put_se_k(&mut self, k: u32, value: i32) {
        self.put_ue_k(k, map::se_code_num(value));
    }

    fn put_ue_v64(&mut self, value: u64) {
        debug_assert!(
            value != u64::MAX,
            "GolombEncode::put_ue_v64: u64::MAX is not representable"
        );
        let code = value.min(u64::MAX - 1) + 1;
        let bits = 64 - code.leading_zeros();
        self.put_zeros(bits - 1);
        self.put_long(bits, code);
    }
}

/// Cost in bits of a whole run of `ue(v)` values, without writing them.
///
/// Re-exported from [`crate::map`] here because "how many bits would this cost"
/// is an encoder question and this is the encoder module.
#[must_use]
pub fn ue_v_cost(values: &[u32]) -> u64 {
    map::ue_bits_total(values)
}
