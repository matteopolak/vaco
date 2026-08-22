//! The arithmetic encoding engine, ITU-T H.264 clause 9.3.4.
//!
//! | Spec | Here | Clause |
//! |---|---|---|
//! | `InitEncoder` | [`CabacEncoder::new`] | 9.3.4.1 |
//! | `EncodeDecision` | [`CabacEncoder::encode_decision`] | 9.3.4.2 |
//! | `RenormE` / `PutBit` | internal | 9.3.4.3 |
//! | `EncodeBypass` | [`CabacEncoder::encode_bypass`] | 9.3.4.4 |
//! | `EncodeTerminate` | [`CabacEncoder::encode_terminate`] | 9.3.4.5 |
//! | `EncodeFlush` | [`CabacEncoder::finish`] | 9.3.4.6 |
//!
//! # Why the encoder exists here at all
//!
//! Not because Vaco ships an H.264 or HEVC encoder — D9 makes HEVC and VVC RED,
//! and H.264 encode is not in the default build either. It exists because
//! **it is the only way to test the decoder against something other than
//! itself.**
//!
//! A CABAC decoder cannot be checked against hand-written bit patterns the way
//! Exp-Golomb can: the specification gives no worked bitstream, and a bin
//! sequence maps onto bytes only through the whole adaptive state machine. With
//! an encoder written independently from clause 9.3.4, the property test becomes
//! "encode an arbitrary bin sequence with arbitrary contexts, decode it, get the
//! same bins back", which exercises every state transition, every
//! renormalisation width and the carry propagation — and fails loudly if either
//! side misreads the standard. That is the strongest oracle available without a
//! conformance stream, and it costs about 120 lines.
//!
//! # `bitsOutstanding` is the unbounded thing here
//!
//! `PutBit` writes `bitsOutstanding` opposite bits after each real one — the
//! carry-propagation mechanism. Nothing in clause 9.3.4 bounds that counter, and
//! an adversarial bin sequence can drive it up indefinitely, so the output size
//! is not a function of the input size. [`CabacEncoder::with_limit`] caps it;
//! past the cap the encoder stops appending and reports
//! [`CabacEncoder::overflowed`], which is a bounded failure rather than an
//! unbounded allocation.

use crate::ContextModel;
use crate::tables::{LPS_RANGE, TRANS};

/// `ivlCurrRange` at initialisation, clause 9.3.4.1.
const INITIAL_RANGE: u32 = 510;

/// Default ceiling on encoder output, in bytes.
///
/// 16 MiB: larger than any single slice a real encoder produces, small enough
/// that a fuzz case driving carry propagation cannot exhaust memory. Change it
/// per instance with [`CabacEncoder::with_limit`].
pub const DEFAULT_MAX_BYTES: usize = 16 << 20;

/// The CABAC arithmetic encoding engine.
///
/// Produces the byte string a [`CabacDecoder`](crate::CabacDecoder) initialised
/// on it will decode back to the same bins. See the module documentation for
/// why an encoder is here.
#[derive(Debug, Clone)]
pub struct CabacEncoder {
    /// `ivlLow`.
    low: u32,
    /// `ivlCurrRange`.
    range: u32,
    /// `bitsOutstanding`, clause 9.3.4.3.
    outstanding: u64,
    /// `firstBitFlag` — the first `PutBit` emits nothing, clause 9.3.4.1.
    first_bit: bool,
    out: Vec<u8>,
    /// Partial byte, MSB-first, `bit_count` bits valid.
    partial: u8,
    bit_count: u32,
    max_bytes: usize,
    overflowed: bool,
}

impl Default for CabacEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl CabacEncoder {
    /// `InitEncoder`, clause 9.3.4.1, with [`DEFAULT_MAX_BYTES`] of output
    /// allowed.
    #[must_use]
    pub const fn new() -> Self {
        Self::with_limit(DEFAULT_MAX_BYTES)
    }

    /// As [`new`](CabacEncoder::new), with an explicit output ceiling.
    #[must_use]
    pub const fn with_limit(max_bytes: usize) -> Self {
        Self {
            low: 0,
            range: INITIAL_RANGE,
            outstanding: 0,
            first_bit: true,
            out: Vec::new(),
            partial: 0,
            bit_count: 0,
            max_bytes,
            overflowed: false,
        }
    }

    /// Whether the output ceiling was reached and bits were dropped.
    ///
    /// Once true the byte string is no longer decodable; the caller should
    /// discard it. Reachable only from a bin sequence that drives carry
    /// propagation far past anything a real encoder produces.
    #[must_use]
    pub const fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// Bits written so far, including the unflushed partial byte.
    #[must_use]
    pub fn bit_len(&self) -> u64 {
        (self.out.len() as u64)
            .saturating_mul(8)
            .saturating_add(u64::from(self.bit_count))
    }

    // ----------------------------------------------------------- bit emission

    #[inline]
    fn write_bit(&mut self, b: u32) {
        if self.out.len() >= self.max_bytes {
            self.overflowed = true;
            return;
        }
        self.partial = (self.partial << 1) | ((b & 1) as u8);
        self.bit_count += 1;
        if self.bit_count == 8 {
            self.out.push(self.partial);
            self.partial = 0;
            self.bit_count = 0;
        }
    }

    /// `PutBit(B)`, clause 9.3.4.3.
    #[inline]
    fn put_bit(&mut self, b: u32) {
        if self.first_bit {
            self.first_bit = false;
        } else {
            self.write_bit(b);
        }
        while self.outstanding > 0 {
            self.write_bit(1 - (b & 1));
            self.outstanding -= 1;
            if self.overflowed {
                // The counter is unbounded by the specification; the ceiling is
                // what turns a hostile bin sequence into a reported failure
                // rather than an unbounded write.
                self.outstanding = 0;
                return;
            }
        }
    }

    /// `RenormE`, clause 9.3.4.3.
    #[inline]
    fn renorm(&mut self) {
        while self.range < 256 {
            if self.low < 256 {
                self.put_bit(0);
            } else if self.low >= 512 {
                self.low -= 512;
                self.put_bit(1);
            } else {
                self.low -= 256;
                self.outstanding = self.outstanding.saturating_add(1);
            }
            self.range <<= 1;
            self.low <<= 1;
        }
    }

    // --------------------------------------------------------------- the codes

    /// `EncodeDecision`, clause 9.3.4.2.
    ///
    /// The state update is the same packed-byte table the decoder uses, so the
    /// two cannot drift: a transcription error in `tables` breaks both, and the
    /// round-trip property test catches it immediately.
    pub fn encode_decision(&mut self, ctx: &mut ContextModel, bin: u32) {
        let state = ctx.0 as usize;
        let q = ((self.range >> 6) & 3) as usize;
        let lps_range = u32::from(
            LPS_RANGE
                .get((state >> 1) * 4 + q)
                .copied()
                .unwrap_or_default(),
        );
        self.range -= lps_range;

        let mps = state as u32 & 1;
        let is_lps = u32::from((bin & 1) != mps);
        if is_lps == 1 {
            self.low += self.range;
            self.range = lps_range;
        }
        ctx.0 = TRANS
            .get(((is_lps as usize) << 8) | state)
            .copied()
            .unwrap_or_default();
        self.renorm();
    }

    /// `EncodeBypass`, clause 9.3.4.4.
    pub fn encode_bypass(&mut self, bin: u32) {
        self.low <<= 1;
        if bin & 1 != 0 {
            self.low += self.range;
        }
        if self.low >= 1024 {
            self.put_bit(1);
            self.low -= 1024;
        } else if self.low < 512 {
            self.put_bit(0);
        } else {
            self.low -= 512;
            self.outstanding = self.outstanding.saturating_add(1);
        }
    }

    /// `n` bypass bins from `value`, MSB first — the inverse of
    /// [`CabacDecoder::decode_bypass_bits`](crate::CabacDecoder::decode_bypass_bits).
    pub fn encode_bypass_bits(&mut self, n: u32, value: u32) {
        let n = n.min(32);
        for i in (0..n).rev() {
            self.encode_bypass((value >> i) & 1);
        }
    }

    /// `EncodeTerminate`, clause 9.3.4.5.
    ///
    /// `bin == 1` terminates the stream and flushes; `bin == 0` renormalises and
    /// encoding continues. Call [`finish`](CabacEncoder::finish) after a
    /// terminating bin to take the bytes.
    pub fn encode_terminate(&mut self, bin: u32) {
        self.range -= 2;
        if bin & 1 != 0 {
            self.low += self.range;
            self.flush();
        } else {
            self.renorm();
        }
    }

    /// `EncodeFlush`, clause 9.3.4.6.
    fn flush(&mut self) {
        self.range = 2;
        self.renorm();
        self.put_bit((self.low >> 9) & 1);
        // WriteBits(((ivlLow >> 7) & 3) | 1, 2) — the trailing one bit that
        // makes the result an `rbsp_stop_one_bit`.
        let two = ((self.low >> 7) & 3) | 1;
        self.write_bit(two >> 1);
        self.write_bit(two & 1);
    }

    /// The `EGk` suffix of clause 9.3.3.1.3, in bypass mode — the inverse of
    /// [`CabacDecoder::decode_bypass_egk`](crate::CabacDecoder::decode_bypass_egk).
    ///
    /// A `value` needing a prefix longer than the decoder's 32-bin ceiling is
    /// not encodable; the encoder emits what it can and the round trip will not
    /// hold, which is why the property tests bound the values they feed in.
    pub fn encode_bypass_egk(&mut self, k: u32, value: u32) {
        let mut k = k.min(31);
        let mut remaining = value;
        let mut run = 0u32;
        while remaining >= 1u32.checked_shl(k).unwrap_or(u32::MAX) && run < 32 {
            self.encode_bypass(1);
            remaining -= 1u32.checked_shl(k).unwrap_or(u32::MAX);
            k += 1;
            run += 1;
            if k > 31 {
                return;
            }
        }
        self.encode_bypass(0);
        self.encode_bypass_bits(k, remaining);
    }

    /// Take the encoded bytes, padding the final partial byte with zeros.
    ///
    /// Call [`encode_terminate(1)`](CabacEncoder::encode_terminate) first for a
    /// stream a conforming decoder will accept; without it the arithmetic
    /// interval is never resolved.
    #[must_use]
    pub fn finish(mut self) -> Vec<u8> {
        if self.bit_count > 0 {
            let pad = 8 - self.bit_count;
            self.partial <<= pad;
            self.out.push(self.partial);
            self.bit_count = 0;
        }
        self.out
    }
}
