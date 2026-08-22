//! The decoder side: `ue(v)`, `se(v)`, `te(v)`, `me(v)` and the order-`k` forms.
//!
//! Implemented as an extension trait on [`BitReader`] so a codec imports exactly
//! the vocabulary it uses, and so the method names match the specification's
//! descriptor syntax (`ue_v` reads what H.264 clause 7 writes as `ue(v)`).
//!
//! # Why these do not simply forward to `vaco-bitstream`
//!
//! `vaco_bitstream::GolombRead` already has `ue`/`se`. This crate's `ue_v` is a
//! different implementation and it is faster: see the module comment on
//! [`GolombDecode::ue_v`] for the mechanism and `benches/golomb.rs` for the
//! measurement. The two agree on every input, which `tests/spec.rs` asserts by
//! differential property test — that agreement is the point, since the parsers
//! already written against `vaco-bitstream` must keep decoding identically.

use vaco_bitstream::{BitReader, BitstreamError, Result};

use crate::map;
use crate::tables::{ChromaArrayType, MbPartPredMode, cbp_code_num_count, cbp_from_code_num};

/// The largest Exp-Golomb prefix that still yields a `u32` code number.
const MAX_PREFIX_32: u32 = 31;
/// The largest prefix that still yields a `u64` code number.
const MAX_PREFIX_64: u32 = 63;
/// Prefix length below which a whole `ue(v)` codeword fits in one 32-bit peek.
///
/// A codeword is `2·lz + 1` bits, so `lz <= 15` gives at most 31 bits.
const INLINE_PREFIX: u32 = 15;

/// Exp-Golomb reads, ITU-T H.264 clause 9.1.
///
/// # The sticky-overrun contract
///
/// These follow [`BitReader`]'s model exactly: nothing returns `Result` unless
/// it is a `*_max`/`*_range` form, past-the-end reads produce zeros, and a
/// structurally impossible codeword flags the reader. One
/// [`BitReader::check`](vaco_bitstream::BitReader::check) at the end of a syntax
/// structure covers the lot.
///
/// **Nothing here loops on input.** Every prefix is bounded by a leading-zero
/// count over a fixed-width word, so an all-zero buffer is rejected in constant
/// time rather than hanging. That is the property the fuzz target exists to
/// keep.
pub trait GolombDecode {
    /// `ue(v)` — unsigned Exp-Golomb, clause 9.1.
    ///
    /// A prefix longer than 31 zeros cannot produce a `u32`, so it flags the
    /// reader and returns 0.
    fn ue_v(&mut self) -> u32;

    /// `se(v)` — signed Exp-Golomb, clause 9.1.1.
    fn se_v(&mut self) -> i32;

    /// `te(v)` — truncated Exp-Golomb, clause 9.1.1.
    ///
    /// With `c_max > 1` this is `ue(v)`. With `c_max <= 1` the codeword is a
    /// single bit whose **inverse** is the value, which is the part everyone
    /// gets backwards the first time.
    fn te_v(&mut self, c_max: u32) -> u32;

    /// `me(v)` — mapped Exp-Golomb, clause 9.1.2, giving
    /// `coded_block_pattern`.
    ///
    /// A code number past the end of the applicable Table 9-4 column flags the
    /// reader and returns 0.
    fn me_v(&mut self, chroma: ChromaArrayType, pred: MbPartPredMode) -> u32;

    /// Order-`k` unsigned Exp-Golomb. Order 0 is [`ue_v`](GolombDecode::ue_v).
    ///
    /// The codeword is `lz` zeros, a one, then `lz + k` bits, and the value is
    /// `(2^lz − 1)·2^k + suffix`. A codeword whose value would not fit a `u32`
    /// flags the reader.
    fn ue_k(&mut self, k: u32) -> u32;

    /// Order-`k` signed Exp-Golomb: [`ue_k`](GolombDecode::ue_k) through the
    /// clause 9.1.1 signed mapping. Order 0 is [`se_v`](GolombDecode::se_v).
    ///
    /// Note this is the *zig-zag* signed form, not the explicit sign bit that
    /// CABAC's `UEGk` suffix uses — see `vaco-codec-cabac` for that one.
    fn se_k(&mut self, k: u32) -> i32;

    /// `ue(v)` widened to 64 bits, for the rare field whose prefix exceeds 31
    /// zeros. Prefixes past 63 zeros are still malformed.
    fn ue_v64(&mut self) -> u64;

    /// `ue(v)` with an inclusive ceiling checked at the read site.
    ///
    /// # Errors
    ///
    /// [`BitstreamError::Overrun`] if the read ran past the end,
    /// [`BitstreamError::Malformed`] on an impossible prefix, and
    /// [`BitstreamError::ValueTooLarge`] if the value exceeds `max`.
    fn ue_v_max(&mut self, max: u32) -> Result<u32>;

    /// `se(v)` with an inclusive range checked at the read site.
    ///
    /// # Errors
    ///
    /// As [`ue_v_max`](GolombDecode::ue_v_max).
    fn se_v_range(&mut self, min: i32, max: i32) -> Result<i32>;

    /// `te(v)` with the ceiling `c_max` also enforced on the decoded value.
    ///
    /// Clause 9.1.1 gives `te(v)` a range by definition, but nothing in the
    /// coding stops a stream from exceeding it, so it has to be checked.
    ///
    /// # Errors
    ///
    /// As [`ue_v_max`](GolombDecode::ue_v_max).
    fn te_v_checked(&mut self, c_max: u32) -> Result<u32>;

    /// `me(v)`, reporting an out-of-table code number as an error rather than
    /// as a flag.
    ///
    /// # Errors
    ///
    /// As [`ue_v_max`](GolombDecode::ue_v_max); a code number past the end of
    /// the Table 9-4 column gives [`BitstreamError::ValueTooLarge`].
    fn me_v_checked(&mut self, chroma: ChromaArrayType, pred: MbPartPredMode) -> Result<u32>;

    /// Order-`k` unsigned Exp-Golomb with an inclusive ceiling.
    ///
    /// # Errors
    ///
    /// As [`ue_v_max`](GolombDecode::ue_v_max).
    fn ue_k_max(&mut self, k: u32, max: u32) -> Result<u32>;
}

/// Run `f`, turning a newly-set reader flag into the right error.
///
/// Written once because getting it wrong the same way in six places is how a
/// truncation ends up reported as a range violation.
#[inline]
fn checked<'a, T, F>(r: &mut BitReader<'a>, f: F) -> Result<T>
where
    F: FnOnce(&mut BitReader<'a>) -> T,
{
    if r.overrun() {
        return Err(BitstreamError::Overrun);
    }
    let v = f(r);
    if r.overrun() {
        return Err(if r.bits_left() == 0 {
            BitstreamError::Overrun
        } else {
            BitstreamError::Malformed
        });
    }
    Ok(v)
}

impl GolombDecode for BitReader<'_> {
    /// # How this is faster than the obvious version, and by how much
    ///
    /// The obvious `ue(v)` — the one `vaco_bitstream::GolombRead::ue` uses — is
    /// `peek` → `leading_zeros` → `skip(lz + 1)` → `get(lz)`: two extractions
    /// from the reader's cache, and a possible second refill between them.
    ///
    /// But a codeword with `lz <= 15` is at most 31 bits long, so it is
    /// **already inside the 32-bit word we peeked**. Take the top `2·lz + 1`
    /// bits of that word — which is `2^lz + suffix`, exactly `codeNum + 1` —
    /// subtract one, and skip the whole codeword in a single step.
    ///
    /// Measured on Apple M5 (`benches/golomb.rs`, min of 300 samples over 4096
    /// codewords, three runs agreeing), against the same two-step shape written
    /// beside it in the same file so the comparison cannot be an artefact of a
    /// crate boundary:
    ///
    /// | Corpus | two-step | one-peek | |
    /// |---|---|---|---|
    /// | realistic (`codeNum` mostly < 2^12) | 7.25 µs | **6.87 µs** | 1.05x |
    /// | uniform to 2^31 (prefixes of 16–31 zeros) | 17.0 µs | **12.1 µs** | 1.40x |
    ///
    /// # `inline(always)`, and why plain `#[inline]` was not enough
    ///
    /// With `#[inline]` this measured **the same as the two-step version** —
    /// LLVM declined to inline it across the crate boundary, and an out-of-line
    /// call forces the reader's cache, position and bit count out of registers,
    /// which costs more than the shape saves. The identical code written inside
    /// the benchmark crate was 1.40x faster at the same moment.
    ///
    /// That gap is the whole measurement. `#[inline(always)]` is not a
    /// superstition here; it is the difference between the two rows above, and
    /// `benches/golomb.rs` keeps both shapes so a future toolchain that changes
    /// the answer is visible rather than silent.
    #[inline(always)]
    #[allow(
        clippy::inline_always,
        reason = "measured: with plain #[inline] this is 1.40x slower on the \
                  wide corpus, because the out-of-line call spills the reader's \
                  cache and position out of registers. See benches/golomb.rs."
    )]
    fn ue_v(&mut self) -> u32 {
        let word = self.peek(32);
        let lz = word.leading_zeros();
        if lz <= INLINE_PREFIX {
            // 2·lz + 1 <= 31 < 32 <= cache_bits after the peek, so the skip
            // takes its cache-only branch and cannot refill.
            self.skip(2 * lz + 1);
            // The top 2·lz + 1 bits of `word`. The leading one bit is at
            // position `lz`, so this is `2^lz + suffix == codeNum + 1 >= 1`.
            (word >> (31 - 2 * lz)) - 1
        } else if lz <= MAX_PREFIX_32 {
            // The two-step form: the codeword is longer than the peeked word.
            self.skip(lz + 1);
            // lz <= 31, so `1 << lz` fits and the sum is at most 2^32 − 2.
            ((1u32 << lz) - 1).wrapping_add(self.get(lz))
        } else {
            self.flag_malformed();
            0
        }
    }

    /// `#[inline(always)]` for the same reason as
    /// [`ue_v`](GolombDecode::ue_v): it is a one-instruction wrapper, and an
    /// out-of-line call to it would undo that function's whole measured gain.
    #[inline(always)]
    #[allow(
        clippy::inline_always,
        reason = "a one-instruction wrapper over ue_v; see that method's note"
    )]
    fn se_v(&mut self) -> i32 {
        map::se_value(self.ue_v())
    }

    #[inline]
    fn te_v(&mut self, c_max: u32) -> u32 {
        if c_max > 1 {
            self.ue_v()
        } else {
            // Clause 9.1.1: the value is the *inverse* of the single bit read.
            1 - self.get_bit()
        }
    }

    fn me_v(&mut self, chroma: ChromaArrayType, pred: MbPartPredMode) -> u32 {
        let code_num = self.ue_v();
        if let Some(v) = cbp_from_code_num(code_num, chroma, pred) {
            v
        } else {
            self.flag_malformed();
            0
        }
    }

    #[inline]
    fn ue_k(&mut self, k: u32) -> u32 {
        if k == 0 {
            return self.ue_v();
        }
        let word = self.peek(32);
        let lz = word.leading_zeros();
        // `lz + k <= 32` keeps the suffix within one `get`; the u64 arithmetic
        // below then decides whether the *value* fits a u32.
        if lz > MAX_PREFIX_32 || lz.saturating_add(k) > 32 {
            self.flag_malformed();
            return 0;
        }
        self.skip(lz + 1);
        let suffix = u64::from(self.get(lz + k));
        let value = (((1u64 << lz) - 1) << k) + suffix;
        if let Ok(v) = u32::try_from(value) {
            v
        } else {
            self.flag_malformed();
            0
        }
    }

    #[inline]
    fn se_k(&mut self, k: u32) -> i32 {
        map::se_value(self.ue_k(k))
    }

    fn ue_v64(&mut self) -> u64 {
        // At most two iterations: 64 zeros already exceeds the ceiling, so this
        // is a bounded loop written as a loop rather than an unbounded one.
        let mut lz = 0u32;
        loop {
            let z = self.peek(32).leading_zeros();
            if z < 32 {
                lz += z;
                if lz > MAX_PREFIX_64 {
                    self.flag_malformed();
                    return 0;
                }
                self.skip(z + 1);
                break;
            }
            lz += 32;
            if lz > MAX_PREFIX_64 {
                self.flag_malformed();
                return 0;
            }
            self.skip(32);
        }
        let suffix = self.get_long(lz);
        ((1u64 << lz) - 1).wrapping_add(suffix)
    }

    fn ue_v_max(&mut self, max: u32) -> Result<u32> {
        let v = checked(self, GolombDecode::ue_v)?;
        too_large(v, max)
    }

    fn se_v_range(&mut self, min: i32, max: i32) -> Result<i32> {
        let v = checked(self, GolombDecode::se_v)?;
        if v < min || v > max {
            return Err(BitstreamError::ValueTooLarge {
                value: u64::from(v.unsigned_abs()),
                max: u64::from(max.unsigned_abs()),
            });
        }
        Ok(v)
    }

    fn te_v_checked(&mut self, c_max: u32) -> Result<u32> {
        let v = checked(self, |r| r.te_v(c_max))?;
        too_large(v, c_max)
    }

    fn me_v_checked(&mut self, chroma: ChromaArrayType, pred: MbPartPredMode) -> Result<u32> {
        let code_num = checked(self, GolombDecode::ue_v)?;
        match cbp_from_code_num(code_num, chroma, pred) {
            Some(v) => Ok(v),
            None => Err(BitstreamError::ValueTooLarge {
                value: u64::from(code_num),
                max: u64::from(cbp_code_num_count(chroma) - 1),
            }),
        }
    }

    fn ue_k_max(&mut self, k: u32, max: u32) -> Result<u32> {
        let v = checked(self, |r| r.ue_k(k))?;
        too_large(v, max)
    }
}

#[inline]
fn too_large(value: u32, max: u32) -> Result<u32> {
    if value > max {
        return Err(BitstreamError::ValueTooLarge {
            value: u64::from(value),
            max: u64::from(max),
        });
    }
    Ok(value)
}
