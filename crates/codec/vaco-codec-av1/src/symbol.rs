//! The AV1 symbol decoder: AV1 spec §8.2, transcribed field-for-field.
//!
//! Everything downstream of the frame header depends on this being exactly
//! right — a single wrong bit in the range-splitting arithmetic or the CDF
//! update rule does not produce a slightly-wrong pixel, it desynchronises
//! the bit position for the rest of the tile, so this module gets the most
//! deliberate, least-clever treatment in the crate: every step below is
//! named after the specification paragraph it implements, in the same
//! order, with no algebraic simplification that would make the two harder
//! to compare line by line.
//!
//! `Vaco-Spec-Ref: av1-spec 8.2 (parsing process for symbol decoder)`.

use vaco_bitstream::BitReader;

/// §3, "Symbols and abbreviated terms": bits of CDF precision the range
/// coder discards per symbol.
const EC_PROB_SHIFT: u32 = 6;
/// §3: the minimum probability mass reserved for every symbol, so no symbol
/// can ever have a coding cost approaching infinity.
const EC_MIN_PROB: u32 = 4;

/// The symbol decoder's mutable state (`SymbolValue`/`SymbolRange`/
/// `SymbolMaxBits`, §8.2.2), plus the bit reader it draws raw bits from.
///
/// One instance per tile: §8.2.2's `init_symbol(sz)` is invoked once per
/// tile with `sz` set to that tile's own byte length, and everything this
/// type touches is scoped to that range — `reader` is constructed over
/// exactly the tile's `sz` bytes, so a read past the end of the tile
/// naturally returns the zero padding bits §8.2.2's own note describes,
/// via [`BitReader::get`]'s documented sticky-overrun behaviour, with no
/// special-casing needed here.
#[derive(Debug)]
pub struct SymbolDecoder<'a> {
    reader: BitReader<'a>,
    /// `SymbolValue`.
    value: u32,
    /// `SymbolRange`.
    range: u32,
    /// `SymbolMaxBits`. Signed: the specification explicitly allows this to
    /// go negative, at which point reads consume padding rather than real
    /// bits.
    max_bits: i64,
    /// `disable_cdf_update` from the frame header, latched for the tile.
    disable_cdf_update: bool,
}

impl<'a> SymbolDecoder<'a> {
    /// `init_symbol(sz)`, §8.2.2. `data` is exactly the tile's own bytes —
    /// the caller slices the tile group payload to `sz` bytes before
    /// calling this, so `data.len()` already *is* `sz`.
    #[must_use]
    pub fn new(data: &'a [u8], disable_cdf_update: bool) -> Self {
        let sz = data.len();
        let mut reader = BitReader::new(data);
        let num_bits = (sz.saturating_mul(8)).min(15);
        let buf = reader.get(u32::try_from(num_bits).unwrap_or(0));
        let padded_buf = buf << (15 - num_bits);
        let value = ((1u32 << 15) - 1) ^ padded_buf;
        let range = 1u32 << 15;
        let max_bits = i64::try_from(sz.saturating_mul(8)).unwrap_or(i64::MAX) - 15;
        Self { reader, value, range, max_bits, disable_cdf_update }
    }

    /// `read_symbol(cdf)`, §8.2.6.
    ///
    /// `cdf` has length `N + 1` for an `N`-valued symbol: `cdf[0..N-1]` are
    /// the (adapted) cumulative thresholds, `cdf[N-1]` is always `1 << 15`,
    /// and `cdf[N]` is the specification's own per-context adaptation-rate
    /// counter, updated in place here exactly as it is in the array the
    /// specification adapts.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "every intermediate value is bounded by the 15-bit range coder \
                  state the specification itself defines; casts mirror the spec's \
                  own fixed-width arithmetic, not an approximation of it"
    )]
    pub fn read_symbol(&mut self, cdf: &mut [u16]) -> u32 {
        let n = cdf.len().saturating_sub(1);
        debug_assert!(n >= 2, "read_symbol needs cdf[N-1] == 32768 for some N >= 2");

        let mut cur = self.range;
        let mut prev;
        let mut symbol: i64 = -1;
        loop {
            symbol += 1;
            prev = cur;
            let idx = usize::try_from(symbol).unwrap_or(0);
            let cdf_val = u32::from(cdf.get(idx).copied().unwrap_or(1 << 15));
            let f = (1u32 << 15) - cdf_val;
            cur = ((self.range >> 8) * (f >> EC_PROB_SHIFT)) >> (7 - EC_PROB_SHIFT);
            let remaining = u32::try_from(n).unwrap_or(0).saturating_sub(u32::try_from(symbol).unwrap_or(0) + 1);
            cur += EC_MIN_PROB * remaining;
            if self.value >= cur {
                break;
            }
        }
        self.range = prev - cur;
        self.value -= cur;

        // Renormalization, §8.2.6's seven ordered steps. `range` never
        // reaches 0 here: `cur` strictly decreases as `symbol` grows (the
        // `EC_MIN_PROB * (N - symbol - 1)` term drops by exactly
        // `EC_MIN_PROB` per step while the other term never increases), so
        // `prev - cur` is always positive; `checked_ilog2` guards it anyway
        // rather than trusting that argument at runtime.
        let bits = 15u32.saturating_sub(self.range.checked_ilog2().unwrap_or(0));
        self.range <<= bits;
        let num_bits = bits.min(u32::try_from(self.max_bits.max(0)).unwrap_or(u32::MAX));
        let new_data = self.reader.get(num_bits);
        let padded_data = new_data << (bits - num_bits);
        self.value = padded_data ^ (((self.value + 1) << bits).wrapping_sub(1));
        self.max_bits -= i64::from(bits);

        let symbol_u = u32::try_from(symbol).unwrap_or(0);
        if !self.disable_cdf_update {
            update_cdf(cdf, symbol_u, n);
        }
        symbol_u
    }

    /// `read_bool()`, §8.2.3: an equal-probability bit, built from a fresh
    /// two-outcome cdf each call. The specification's own note says the
    /// resulting adaptation is never observed (the array is discarded), so
    /// this uses a scratch array rather than threading a persistent one.
    pub fn read_bool(&mut self) -> u32 {
        let mut cdf = [1u16 << 14, 1u16 << 15, 0];
        self.read_symbol(&mut cdf)
    }

    /// `read_literal(n)`, §8.2.5: `n` equal-probability bits, MSB first.
    pub fn read_literal(&mut self, n: u32) -> u32 {
        let mut x = 0u32;
        for _ in 0..n {
            x = 2 * x + self.read_bool();
        }
        x
    }

    /// `exit_symbol()`, §8.2.4: advance the underlying bit position past any
    /// unread trailing bits, so a caller reading the *next* tile (or the
    /// OBU's own trailing bits) starts from the right byte-aligned offset.
    ///
    /// The specification also states two bitstream-conformance requirements
    /// on the trailing-bit pattern (a `1` at `trailingBitPosition`, `0`s
    /// after); this crate does not reject non-conforming input on them —
    /// consistent with `vaco-parse-av1`'s stance that this crate parses
    /// untrusted data and prefers a bounded, malformed-but-recovered result
    /// over a hard failure on a coding-only conformance point.
    pub fn exit_symbol(&mut self) {
        if self.max_bits > 0 {
            let skip = u64::try_from(self.max_bits).unwrap_or(0);
            self.reader.skip_long(skip);
        }
    }

    /// Whether the underlying reader ran past the buffer it was given —
    /// meaning every subsequent [`Self::read_symbol`] call has been reading
    /// the specification's own zero padding rather than real bits. Exposed
    /// so a caller decoding a tile can tell "truncated input" apart from "a
    /// tile that legitimately ends exactly on a byte boundary".
    #[must_use]
    pub fn overrun(&self) -> bool {
        self.reader.overrun()
    }
}

/// The CDF adaptation rule inside `read_symbol`, §8.2.6's final block.
fn update_cdf(cdf: &mut [u16], symbol: u32, n: usize) {
    let count = cdf.get(n).copied().unwrap_or(0);
    let rate = 3 + u32::from(count > 15) + u32::from(count > 31) + n.checked_ilog2().unwrap_or(0).min(2);
    let mut tmp: u16 = 0;
    for (i, slot) in cdf.iter_mut().take(n.saturating_sub(1)).enumerate() {
        if u32::try_from(i).unwrap_or(0) == symbol {
            tmp = 1 << 15;
        }
        let cur = *slot;
        if tmp < cur {
            *slot = cur - ((cur - tmp) >> rate);
        } else {
            *slot = cur + ((tmp - cur) >> rate);
        }
    }
    if let Some(c) = cdf.get_mut(n)
        && *c < 32
    {
        *c += 1;
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;

    #[test]
    fn cdf_stays_sorted_and_terminated_under_random_symbol_streams() {
        // §8.2.6's own note: cdf[N-1] must stay 1<<15 and the array must stay
        // a valid (non-decreasing) cumulative distribution after every
        // update, for any sequence of decoded symbols. This is a property
        // every correct adaptation must have, checked over many pseudo-random
        // bit patterns rather than one fixed trace.
        for seed in 0u8..64 {
            let data: Vec<u8> = (0..64).map(|i: u8| i.wrapping_mul(seed).wrapping_add(seed)).collect();
            let mut sd = SymbolDecoder::new(&data, false);
            let mut cdf = [8192u16, 16384, 24576, 1 << 15, 0];
            for _ in 0..200 {
                let symbol = sd.read_symbol(&mut cdf);
                assert!(symbol < 4, "symbol {symbol} out of range");
                assert_eq!(cdf[3], 1 << 15, "cdf[N-1] must stay fixed");
                for w in cdf[..3].windows(2) {
                    assert!(w[0] <= w[1], "cdf must stay non-decreasing: {cdf:?}");
                }
                assert!(cdf[4] <= 32, "adaptation counter must saturate at 32");
            }
        }
    }

    #[test]
    fn read_bool_never_panics_on_truncated_or_empty_input() {
        for len in 0..8 {
            let data = vec![0xA5u8; len];
            let mut sd = SymbolDecoder::new(&data, false);
            for _ in 0..32 {
                let _ = sd.read_bool();
            }
            sd.exit_symbol();
        }
    }

    #[test]
    fn read_literal_matches_repeated_read_bool_by_construction() {
        // read_literal(n) is defined as n calls to read_bool folded MSB
        // first; check the fold itself against an independent loop over a
        // *second* decoder fed the same bytes, so a slip in the fold (wrong
        // shift direction, wrong accumulation order) is caught even though
        // both paths ultimately call the same read_bool.
        let data = [0x3Cu8, 0x91, 0x77, 0x02, 0xF0];
        let mut a = SymbolDecoder::new(&data, false);
        let mut b = SymbolDecoder::new(&data, false);
        let via_literal = a.read_literal(6);
        let mut via_bits = 0u32;
        for _ in 0..6 {
            via_bits = 2 * via_bits + b.read_bool();
        }
        assert_eq!(via_literal, via_bits);
    }

    #[test]
    fn disable_cdf_update_leaves_the_cdf_untouched() {
        let data = [0x12u8, 0x34, 0x56, 0x78];
        let mut sd = SymbolDecoder::new(&data, true);
        let mut cdf = [8192u16, 1 << 15, 0];
        let before = cdf;
        let _ = sd.read_symbol(&mut cdf);
        assert_eq!(before, cdf, "disable_cdf_update must skip the adaptation step entirely");
    }

    /// An independent Python transliteration of the same specification text
    /// (kept in `provenance/vaco-codec-av1-symbol-trace.py`) was run over
    /// this exact byte sequence and a fixed two-symbol cdf, decoding six
    /// symbols in a row while re-adapting the cdf each time exactly as
    /// `update_cdf` does. This test freezes that trace so a future edit to
    /// this file cannot silently change the arithmetic without a human
    /// re-deriving the expected sequence by a different route than the Rust
    /// code itself.
    #[test]
    fn matches_an_independently_transliterated_reference_trace() {
        let data = [0xB4u8, 0x2Fu8, 0x91u8, 0x0Cu8];
        let mut sd = SymbolDecoder::new(&data, false);
        let mut cdf = [1u16 << 14, 1 << 15, 0];
        let mut symbols = Vec::new();
        for _ in 0..6 {
            symbols.push(sd.read_symbol(&mut cdf));
        }
        assert_eq!(symbols, [1, 0, 1, 1, 1, 0], "diverged from the independent Python trace");
    }
}
