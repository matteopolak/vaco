//! Exp-Golomb coding, as defined by ITU-T H.264 §9.1.
//!
//! An extension trait rather than inherent methods so a codec crate imports
//! exactly the vocabulary it uses.
//!
//! # The definition, and the cap
//!
//! `ue(v)` is: count leading zero bits (`leadingZeroBits`), consume the
//! terminating one bit, read `leadingZeroBits` more bits, and the value is
//! `2^leadingZeroBits - 1 + read_bits`. `se(v)` maps that unsigned code number
//! `k` to a signed value: `k` odd gives `(k+1)/2`, `k` even gives `-(k/2)`.
//!
//! A prefix of more than 31 zeros cannot produce a `u32` and is malformed in
//! every codec that uses this coding, so it flags the reader and returns 0
//! rather than looping. **That cap is the difference between a fuzz hang and a
//! clean rejection**, and it is why nothing here has an unbounded loop.

use crate::{BitReader, BitstreamError, Result};

/// Exp-Golomb reads over a [`BitReader`].
pub trait GolombRead {
    /// `ue(v)`. Values whose prefix exceeds the coding's ceiling flag the reader
    /// and return 0 rather than looping.
    fn ue(&mut self) -> u32;

    /// `se(v)`.
    ///
    /// The representable range is `-(2^31 - 1) ..= 2^31 - 1`; a code number
    /// beyond that flags the reader.
    fn se(&mut self) -> i32;

    /// `ue(v)` with an explicit inclusive ceiling, checked at the read site.
    ///
    /// # Errors
    ///
    /// [`BitstreamError::Overrun`] if the read ran past the end,
    /// [`BitstreamError::Malformed`] on an impossible prefix, and
    /// [`BitstreamError::ValueTooLarge`] if the value exceeds `max`.
    fn ue_max(&mut self, max: u32) -> Result<u32>;

    /// `se(v)` with an inclusive range, checked at the read site.
    ///
    /// # Errors
    ///
    /// As [`ue_max`](GolombRead::ue_max).
    fn se_range(&mut self, min: i32, max: i32) -> Result<i32>;

    /// Order-`k` Exp-Golomb: `(2^lz - 1) * 2^k + read(lz + k)`.
    ///
    /// Order 0 is [`ue`](GolombRead::ue).
    fn ue_golomb_k(&mut self, k: u32) -> u32;

    /// `ue(v)` widened to 64 bits, for the rare field whose prefix exceeds 31
    /// zeros. Prefixes past 63 zeros are still malformed.
    fn ue_long(&mut self) -> u64;
}

/// Largest Exp-Golomb prefix that still yields a `u32`.
const MAX_PREFIX_32: u32 = 31;
/// Largest prefix that still yields a `u64`.
const MAX_PREFIX_64: u32 = 63;

impl GolombRead for BitReader<'_> {
    #[inline]
    fn ue(&mut self) -> u32 {
        // One peek, one `leading_zeros`, one skip, one get: no loop anywhere.
        let prefix = self.peek(32);
        let lz = prefix.leading_zeros();
        if lz > MAX_PREFIX_32 {
            self.flag_malformed();
            return 0;
        }
        self.skip(lz + 1);
        let suffix = self.get(lz);
        // lz <= 31, so `1 << lz` and the sum both fit: max is 2^32 - 2.
        ((1u32 << lz) - 1).wrapping_add(suffix)
    }

    #[inline]
    fn se(&mut self) -> i32 {
        let k = self.ue();
        // k odd -> +(k + 1) / 2, k even -> -(k / 2).
        //
        // No range check is needed and none is emitted: `ue` returns at most
        // 2^32 - 2, so the magnitude is at most 2^31 - 1 either way. The largest
        // odd k is 2^32 - 3, giving (k >> 1) + 1 = 2^31 - 1; the largest even k
        // is 2^32 - 2, giving k >> 1 = 2^31 - 1, which negates without wrapping.
        let half = (k >> 1).cast_signed();
        if k & 1 == 1 { half + 1 } else { -half }
    }

    fn ue_max(&mut self, max: u32) -> Result<u32> {
        let before = self.overrun();
        let v = self.ue();
        if !before && self.overrun() {
            return Err(if self.bits_left() == 0 {
                BitstreamError::Overrun
            } else {
                BitstreamError::Malformed
            });
        }
        if v > max {
            return Err(BitstreamError::ValueTooLarge {
                value: u64::from(v),
                max: u64::from(max),
            });
        }
        Ok(v)
    }

    fn se_range(&mut self, min: i32, max: i32) -> Result<i32> {
        let before = self.overrun();
        let v = self.se();
        if !before && self.overrun() {
            return Err(BitstreamError::Overrun);
        }
        if v < min || v > max {
            return Err(BitstreamError::ValueTooLarge {
                value: v.unsigned_abs().into(),
                max: max.unsigned_abs().into(),
            });
        }
        Ok(v)
    }

    #[inline]
    fn ue_golomb_k(&mut self, k: u32) -> u32 {
        if k == 0 {
            return self.ue();
        }
        let prefix = self.peek(32);
        let lz = prefix.leading_zeros();
        if lz > MAX_PREFIX_32 || lz + k > 32 {
            self.flag_malformed();
            return 0;
        }
        self.skip(lz + 1);
        let suffix = self.get(lz + k);
        (((1u32 << lz) - 1) << k).wrapping_add(suffix)
    }

    fn ue_long(&mut self) -> u64 {
        // At most two iterations: 64 zeros already exceeds the ceiling.
        let mut lz = 0u32;
        loop {
            let z = self.peek(32).leading_zeros();
            if z < 32 {
                lz += z;
                if lz > MAX_PREFIX_64 {
                    self.flag_malformed();
                    return 0;
                }
                // The remaining zeros and the terminating one bit.
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
}
