//! The range decoder shared by SILK and CELT. RFC 6716 §4.1.
//!
//! Transliterated from the reference `entdec.c`/`entcode.c`/`laplace.c`
//! embedded in RFC 6716 Appendix A (extracted via `A.1`'s own `base64`
//! recipe) rather than re-derived from the prose, because the prose in
//! §4.1 states the same recurrences less precisely than the C does about
//! integer promotion and truncation, and this decoder must match the
//! encoder's arithmetic bit for bit or every symbol after the first
//! desyncs. All of it is exact 32-bit integer arithmetic; nothing here is
//! a floating-point approximation.

use vaco_core::{Error, Result};

const SYM_BITS: u32 = 8;
const CODE_BITS: u32 = 32;
const SYM_MAX: u32 = (1 << SYM_BITS) - 1;
/// `(CODE_BITS - 2) % SYM_BITS + 1` = 7: the decoder's initial range is
/// `1 << CODE_EXTRA`.
const CODE_EXTRA: u32 = (CODE_BITS - 2) % SYM_BITS + 1;
const CODE_TOP: u32 = 1 << (CODE_BITS - 1);
const CODE_BOT: u32 = CODE_TOP >> SYM_BITS;
const UINT_BITS: u32 = 8;
const WINDOW_BITS: u32 = 32;

/// The entropy decoder. One instance per Opus frame (CELT and SILK inside
/// a hybrid frame share the same instance and the same bit budget).
#[derive(Debug, Clone)]
pub struct RangeDecoder<'a> {
    buf: &'a [u8],
    /// Bytes consumed from the front (normalisation).
    offs: usize,
    /// Bytes consumed from the back (`dec_bits`' raw-bit region).
    end_offs: usize,
    end_window: u32,
    nend_bits: u32,
    /// Bits "spent" so far, `<< BITRES` fractional precision available via
    /// [`RangeDecoder::tell_frac`].
    nbits_total: i32,
    rng: u32,
    val: u32,
    ext: u32,
    rem: u32,
    error: bool,
}

/// `1/8`-bit resolution used by [`RangeDecoder::tell_frac`] and CELT's bit
/// allocator.
pub const BITRES: u32 = 3;

impl<'a> RangeDecoder<'a> {
    /// Initialise from a whole Opus frame's payload.
    #[must_use]
    pub fn new(buf: &'a [u8]) -> Self {
        let mut dec = Self {
            buf,
            offs: 0,
            end_offs: 0,
            end_window: 0,
            nend_bits: 0,
            nbits_total: (CODE_BITS as i32 + 1)
                - (((CODE_BITS - CODE_EXTRA) / SYM_BITS) * SYM_BITS) as i32,
            rng: 1 << CODE_EXTRA,
            val: 0,
            ext: 0,
            rem: 0,
            error: false,
        };
        dec.rem = dec.read_byte();
        dec.val = dec
            .rng
            .wrapping_sub(1)
            .wrapping_sub(dec.rem >> (SYM_BITS - CODE_EXTRA));
        dec.normalize();
        dec
    }

    /// Total encoded length in bytes (the packet/frame's own length, used by
    /// CELT to compute its per-frame bit budget).
    #[must_use]
    pub fn storage(&self) -> usize {
        self.buf.len()
    }

    fn read_byte(&mut self) -> u32 {
        let b = self.buf.get(self.offs).copied().unwrap_or(0);
        self.offs = self.offs.saturating_add(1);
        u32::from(b)
    }

    fn read_byte_from_end(&mut self) -> u32 {
        self.end_offs = self.end_offs.saturating_add(1);
        if self.end_offs <= self.buf.len() {
            let idx = self.buf.len() - self.end_offs;
            u32::from(self.buf.get(idx).copied().unwrap_or(0))
        } else {
            0
        }
    }

    fn normalize(&mut self) {
        while self.rng <= CODE_BOT {
            self.nbits_total = self.nbits_total.wrapping_add(SYM_BITS as i32);
            self.rng <<= SYM_BITS;
            let sym = self.rem;
            self.rem = self.read_byte();
            let sym = (sym << SYM_BITS | self.rem) >> (SYM_BITS - CODE_EXTRA);
            self.val = ((self.val << SYM_BITS).wrapping_add(SYM_MAX & !sym)) & (CODE_TOP - 1);
        }
    }

    /// `ec_decode`: the current symbol's frequency-scaled position, for a
    /// total frequency `ft`.
    fn decode(&mut self, ft: u32) -> u32 {
        self.ext = self.rng / ft;
        let s = self.val / self.ext;
        ft - (s + 1).min(ft)
    }

    /// `ec_decode_bin`.
    fn decode_bin(&mut self, bits: u32) -> u32 {
        self.ext = self.rng >> bits;
        let s = self.val / self.ext;
        (1u32 << bits) - (s + 1).min(1u32 << bits)
    }

    /// `ec_dec_update`.
    fn update(&mut self, fl: u32, fh: u32, ft: u32) {
        let s = self.ext.wrapping_mul(ft - fh);
        self.val = self.val.wrapping_sub(s);
        self.rng = if fl > 0 {
            self.ext.wrapping_mul(fh - fl)
        } else {
            self.rng.wrapping_sub(s)
        };
        self.normalize();
    }

    /// Decode one symbol out of `ft` equally-likely-scaled possibilities and
    /// resolve it against `[fl, fh)`. Most callers want [`Self::icdf`] or
    /// [`Self::bit_logp`] instead; this is the primitive CELT's own
    /// range-coded fields (the split angle, PVQ indices) build on directly.
    pub fn decode_raw(&mut self, ft: u32) -> u32 {
        self.decode(ft)
    }

    /// Resolve a previously-`decode_raw`'d symbol against `[fl, fh)` out of
    /// `ft`.
    pub fn update_raw(&mut self, fl: u32, fh: u32, ft: u32) {
        self.update(fl, fh, ft);
    }

    /// Decode a symbol whose probability is `1/(1<<logp)` of being one.
    pub fn bit_logp(&mut self, logp: u32) -> bool {
        let r = self.rng;
        let d = self.val;
        let s = r >> logp;
        let ret = d < s;
        if !ret {
            self.val = d - s;
        }
        self.rng = if ret { s } else { r - s };
        self.normalize();
        ret
    }

    /// Decode a symbol against an inverse-CDF table in `1/(1<<ftb)` units.
    /// `icdf` is descending and its last entry is `0`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if no entry in `icdf` satisfies the decode —
    /// only reachable with a malformed (non-terminating) table, since every
    /// table this crate passes ends in `0`.
    pub fn icdf(&mut self, icdf: &[u8], ftb: u32) -> Result<i32> {
        let s = self.rng;
        let d = self.val;
        let r = s >> ftb;
        let mut ret: i32 = -1;
        let mut t;
        let mut s_val = s;
        loop {
            t = s_val;
            ret += 1;
            let entry = *icdf
                .get(ret as usize)
                .ok_or(Error::InvalidData("Opus icdf table exhausted"))?;
            s_val = r.wrapping_mul(u32::from(entry));
            if d >= s_val {
                break;
            }
        }
        self.val = d - s_val;
        self.rng = t - s_val;
        self.normalize();
        Ok(ret)
    }

    /// Decode a value uniformly distributed on `0..ft`. `ft` must be `> 1`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if `ft <= 1`. A successful decode can still
    /// mark [`RangeDecoder::had_error`] (RFC 6716's own `ec_dec_uint`
    /// clamps rather than fails outright when the raw bits push the result
    /// out of range, matching the reference decoder's leniency).
    pub fn dec_uint(&mut self, ft: u32) -> Result<u32> {
        if ft <= 1 {
            return Err(Error::InvalidData("Opus ec_dec_uint total must exceed 1"));
        }
        let ft1 = ft - 1;
        let ftb = 32 - ft1.leading_zeros();
        if ftb > UINT_BITS {
            let extra = ftb - UINT_BITS;
            let ft_small = (ft1 >> extra) + 1;
            let s = self.decode(ft_small);
            self.update(s, s + 1, ft_small);
            let t = (s << extra) | self.dec_bits(extra);
            if t <= ft1 {
                Ok(t)
            } else {
                self.error = true;
                Ok(ft1)
            }
        } else {
            let s = self.decode(ft);
            self.update(s, s + 1, ft);
            Ok(s)
        }
    }

    /// Read `bits` raw bits from the back of the buffer (CELT's fine-energy
    /// and sign bits; SILK's LSBs and shell-code sign bits share this same
    /// physical byte stream, worked from the end inward).
    ///
    /// `bits` is never more than a couple of dozen anywhere in this codec
    /// (the widest caller is CELT's post-filter octave field), so this does
    /// not need to handle a full 32-bit request.
    pub fn dec_bits(&mut self, bits: u32) -> u32 {
        debug_assert!(bits <= 25, "no Opus field reads this many raw bits at once");
        let bits = bits.min(25);
        let mut window = self.end_window;
        let mut available = self.nend_bits;
        while available < bits {
            window |= self.read_byte_from_end() << available;
            available += SYM_BITS;
            if available > WINDOW_BITS - SYM_BITS {
                break;
            }
        }
        let mask = (1u32 << bits) - 1;
        let out = window & mask;
        window >>= bits;
        available = available.saturating_sub(bits);
        self.end_window = window;
        self.nend_bits = available;
        self.nbits_total += bits as i32;
        out
    }

    /// Bits consumed so far, rounded up to the next whole bit.
    #[must_use]
    pub fn tell(&self) -> i32 {
        // `entcode.c`'s `ec_tell`: `nbits_total - EC_ILOG(rng)`, its own
        // whole-bit estimate -- *not* `tell_frac() >> BITRES`. The two are
        // close but not identical (`tell_frac`'s fractional-bit refinement
        // loop only ever grows its `l`, so the truncated quotient can come
        // out a bit or two lower than this), and CELT's frame syntax uses
        // `tell() + N <= budget` to decide whether an optional field is
        // present. A decoder whose `tell()` disagrees with the reference's
        // at exactly the wrong boundary makes a different presence
        // decision than the bitstream was encoded with. Like the
        // reference's `int`-returning `ec_tell`, this can be small or
        // (briefly, right after construction) even non-positive; callers
        // compare it directly rather than treating it as a bit count that
        // must be `>= 0`.
        let ilog = if self.rng == 0 {
            0
        } else {
            32 - self.rng.leading_zeros()
        };
        self.nbits_total - ilog as i32
    }

    /// Bits consumed so far in `1/8`-bit units.
    #[must_use]
    pub fn tell_frac(&self) -> i32 {
        let nbits = self.nbits_total << BITRES;
        if self.rng == 0 {
            return nbits;
        }
        let mut l = 32 - self.rng.leading_zeros() as i32;
        let mut r = self.rng >> (l - 16).max(0);
        for _ in 0..BITRES {
            r = (r * r) >> 15;
            let b = (r >> 16) as i32;
            l = (l << 1) | b;
            r >>= b;
        }
        nbits - l
    }

    /// Whether a decode error (an out-of-range `ec_dec_uint`) has occurred.
    #[must_use]
    pub const fn had_error(&self) -> bool {
        self.error
    }

    /// The decoder's final `rng`, folded into `celt_lcg_rand`'s seed by the
    /// CELT decoder so packet-loss noise fill differs frame to frame.
    #[must_use]
    pub const fn rng(&self) -> u32 {
        self.rng
    }
}

/// `ec_laplace_decode`: a Laplace-distributed value used for CELT's coarse
/// energy deltas. `fs0`/`decay` are in the encoder's native fixed units
/// (probability of zero and decay rate, each pre-scaled by the caller as
/// RFC 6716 §4.3.2.1 states); this stays exact 32-bit integer arithmetic for
/// the same reason [`RangeDecoder`] itself does.
pub fn laplace_decode(dec: &mut RangeDecoder<'_>, mut fs: u32, decay: u32) -> i32 {
    const MINP: u32 = 1;

    let mut val: i32 = 0;
    let fm = dec.decode_bin(15);
    let mut fl;
    if fm >= fs {
        val += 1;
        fl = fs;
        let mut fs1 = laplace_freq1(fs, decay) + MINP;
        while fs1 > MINP && fm >= fl + 2 * fs1 {
            fs1 *= 2;
            fl += fs1;
            fs1 = ((fs1 - 2 * MINP) * decay) >> 15;
            fs1 += MINP;
            val += 1;
        }
        if fs1 <= MINP {
            let di = (fm - fl) >> 1;
            val += di as i32;
            fl += 2 * di * MINP;
        }
        if fm < fl + fs1 {
            val = -val;
        } else {
            fl += fs1;
        }
        fs = fs1;
    } else {
        fl = 0;
    }
    let fh = (fl + fs).min(32768);
    dec.update(fl, fh, 32768);
    val
}

fn laplace_freq1(fs0: u32, decay: u32) -> u32 {
    const NMIN: u32 = 16;
    let ft = 32768 - (2 * NMIN) - fs0;
    (ft * (16384 - decay)) >> 15
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_still_normalizes_without_panicking() {
        let mut dec = RangeDecoder::new(&[]);
        assert!(!dec.had_error());
        let _ = dec.icdf(&[128, 0], 8);
    }

    #[test]
    fn tell_starts_near_one_bit() {
        let dec = RangeDecoder::new(&[0xaa, 0x55, 0x00]);
        // RFC 6716 note in entcode.c: a fresh decoder claims ~1 bit used.
        assert!(dec.tell() <= 1);
    }

    #[test]
    fn icdf_round_trips_a_uniform_table() {
        // A flat 4-symbol icdf: {192, 128, 64, 0} in 1/256 units.
        let icdf = [192u8, 128, 64, 0];
        let mut dec = RangeDecoder::new(&[0x3f, 0x00, 0x00, 0x00]);
        assert!(dec.icdf(&icdf, 8).is_ok_and(|sym| (0..4).contains(&sym)));
    }

    #[test]
    fn dec_bits_reads_from_the_back_independent_of_the_front() {
        let mut dec = RangeDecoder::new(&[0x00, 0x00, 0x00, 0xff]);
        let v = dec.dec_bits(8);
        assert_eq!(v, 0xff);
    }
}
