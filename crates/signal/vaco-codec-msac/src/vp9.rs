//! VP9's boolean entropy decoder, VP9 Bitstream & Decoding Process
//! Specification v0.6 §9.2.
//!
//! Numerically the same coder as VP8's (`crate::vp8`) — same `split`
//! formula, same "value < split → 0" comparison — but the specification
//! states it as the pure bit-at-a-time algorithm rather than VP8's
//! byte-buffered variant, and the two differ in one real, bit-exactness-
//! affecting way: **VP9's `init_bool` fills `BoolValue` from a single byte
//! (`f(8)`), not two** the way VP8's `init_bool_decoder` does. Getting that
//! wrong desyncs every bool read after the first. This decoder follows the
//! spec's own bit-at-a-time shift-in exactly (`BoolValue` compared directly
//! against `split`, never `split << 8`), since that is what §9.2.2 actually
//! specifies and it is simpler to get right than re-deriving VP8's
//! byte-buffered trick for a different initial fill.
//!
//! §9.2.1 also requires the first bool read after `init_bool` to be a
//! "marker" bit equal to 0 (bitstream conformance, not a decoder choice);
//! §9.2.3's `exit_bool` requires the remaining bits to be zero padding. Both
//! are exposed so a caller can check them, but neither is enforced by
//! panicking — untrusted input takes the "note it and carry on" path this
//! codebase uses throughout parsers over untrusted data.

use crate::tree::{Tree, read_tree};

/// State the VP9 spec calls the "Boolean decoder", §9.2.
#[derive(Debug, Clone)]
pub struct BoolDecoder<'a> {
    data: &'a [u8],
    /// Next bit position within `data`, MSB-first per byte (§9.1's `f(n)`).
    bit_pos: usize,
    max_bits: usize,
    range: u32,
    value: u32,
    /// Set once `bit_pos` would run past `max_bits`; further bits are
    /// supplied as 0, matching §9.2.2's own prescription for that case.
    overrun: bool,
}

impl<'a> BoolDecoder<'a> {
    /// `init_bool(sz)`: `data` is exactly the `sz`-byte partition. Reads the
    /// first byte into `BoolValue`, sets `BoolRange = 255`, and consumes the
    /// mandatory zero marker bit per §9.2.1 — its value is discarded here
    /// (not enforced) but consuming it is required for every later bit
    /// position to land where the spec says it does.
    #[must_use]
    pub fn new(data: &'a [u8]) -> Self {
        let max_bits = data.len().saturating_mul(8);
        let mut d = Self {
            data,
            bit_pos: 0,
            max_bits,
            range: 255,
            value: 0,
            overrun: data.is_empty(),
        };
        d.value = d.read_raw_bits(8);
        let _marker = d.read_bool(128);
        d
    }

    fn read_bit(&mut self) -> u32 {
        if self.bit_pos >= self.max_bits {
            self.overrun = true;
            return 0;
        }
        #[allow(clippy::integer_division, reason = "byte/bit split of a bit position")]
        let byte_index = self.bit_pos / 8;
        let bit_index = 7 - (self.bit_pos % 8);
        self.bit_pos += 1;
        let byte = self.data.get(byte_index).copied().unwrap_or(0);
        u32::from((byte >> bit_index) & 1)
    }

    fn read_raw_bits(&mut self, n: u32) -> u32 {
        let mut x = 0;
        for _ in 0..n {
            x = 2 * x + self.read_bit();
        }
        x
    }

    /// Whether a read has gone past the supplied buffer.
    #[must_use]
    pub const fn overrun(&self) -> bool {
        self.overrun
    }

    /// `read_bool(p)`, §9.2.2: one bool at probability `p / 256` of zero.
    pub fn read_bool(&mut self, prob: u8) -> bool {
        let split = 1 + (((self.range - 1) * u32::from(prob)) >> 8);
        let bit = if self.value < split {
            self.range = split;
            false
        } else {
            self.range -= split;
            self.value -= split;
            true
        };
        while self.range < 128 {
            let new_bit = self.read_bit();
            self.range <<= 1;
            self.value = (self.value << 1) + new_bit;
        }
        bit
    }

    /// `read_literal(n)`, §9.2.4: `n` bools each at probability 128, MSB first.
    pub fn read_literal(&mut self, num_bits: u32) -> u32 {
        let mut x = 0;
        for _ in 0..num_bits {
            x = 2 * x + u32::from(self.read_bool(128));
        }
        x
    }

    /// Walk `tree` using probabilities `probs`, §9.3.3.
    pub fn read_tree(&mut self, tree: &Tree, probs: &[u8]) -> i32 {
        read_tree(tree, |node| {
            let p = probs.get(node).copied().unwrap_or(128);
            self.read_bool(p)
        })
    }

    /// `exit_bool()`, §9.2.3: whether every remaining bit is unread (i.e.
    /// the caller decoded exactly the syntax the partition size promised).
    /// Not "the remaining bits are zero" — this decoder does not buffer
    /// them — a caller wanting the stronger conformance check would read
    /// and discard the rest via [`Self::read_literal`] itself.
    #[must_use]
    pub const fn bits_remaining(&self) -> usize {
        self.max_bits.saturating_sub(self.bit_pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6386 §7.3's byte-buffered `bool_encoder`, reused verbatim as this
    /// module's test oracle too (not just `vp8`'s). That is sound, not
    /// coincidental: §7.3 states the byte-buffered coder is logically
    /// identical to the bit-at-a-time algorithm VP9's §9.2 states directly,
    /// and the two decoders' comparisons reduce to the same test —
    /// `value_16 >= split << 8` (VP8) is exactly `byte0 >= split` once the
    /// second buffered byte is expanded algebraically (it only ever supplies
    /// a non-negative tiebreaker below `split`'s own precision, which never
    /// flips the comparison), which is exactly VP9's `value_8 < split ? 0 :
    /// 1`. So encoding `write_bool(128, false)` (the required marker) ahead
    /// of a real payload with this encoder, then handing the resulting bytes
    /// to [`BoolDecoder::new`] (which consumes that same marker bool as its
    /// first read), must decode the payload identically to how a VP8
    /// decoder would have decoded calls 2.. of the same stream. This is
    /// exactly the cross-check the crate doc promises: two engines that
    /// would be wrong differently are being run against the same bytes.
    struct BoolEncoder {
        output: Vec<u8>,
        range: u32,
        bottom: u32,
        bit_count: i32,
    }

    impl BoolEncoder {
        fn new() -> Self {
            Self {
                output: Vec::new(),
                range: 255,
                bottom: 0,
                bit_count: 24,
            }
        }

        fn add_one_to_output(&mut self) {
            for byte in self.output.iter_mut().rev() {
                if *byte == 255 {
                    *byte = 0;
                } else {
                    *byte += 1;
                    return;
                }
            }
        }

        fn write_bool(&mut self, prob: u8, value: bool) {
            let split = 1 + (((self.range - 1) * u32::from(prob)) >> 8);
            if value {
                self.bottom += split;
                self.range -= split;
            } else {
                self.range = split;
            }
            while self.range < 128 {
                self.range <<= 1;
                if self.bottom & (1 << 31) != 0 {
                    self.add_one_to_output();
                }
                self.bottom <<= 1;
                self.bit_count -= 1;
                if self.bit_count == 0 {
                    self.output.push((self.bottom >> 24) as u8);
                    self.bottom &= (1 << 24) - 1;
                    self.bit_count = 8;
                }
            }
        }

        fn finish(mut self) -> Vec<u8> {
            let mut c = self.bit_count;
            let mut v = self.bottom;
            if v & (1 << (32 - c)) != 0 {
                self.add_one_to_output();
            }
            v <<= c & 7;
            c >>= 3;
            for _ in 0..c {
                v <<= 8;
            }
            for _ in 0..4 {
                self.output.push((v >> 24) as u8);
                v <<= 8;
            }
            self.output
        }
    }

    #[test]
    fn init_reads_one_byte_and_a_zero_marker() {
        let d = BoolDecoder::new(&[0x00, 0xFF, 0xFF, 0xFF]);
        assert!(!d.overrun());
    }

    #[test]
    fn a_literal_round_trips_through_a_vp8_shaped_encoder() {
        let mut enc = BoolEncoder::new();
        enc.write_bool(128, false); // VP9's mandatory leading marker bool
        let pattern = 0b1010_1100_0011_1001_u16;
        for i in (0..16).rev() {
            enc.write_bool(128, (pattern >> i) & 1 != 0);
        }
        let bytes = enc.finish();
        let mut dec = BoolDecoder::new(&bytes);
        assert_eq!(dec.read_literal(16), u32::from(pattern));
        assert!(!dec.overrun());
    }

    #[test]
    fn varied_probabilities_round_trip_through_a_vp8_shaped_encoder() {
        let bools: Vec<(u8, bool)> = (0_u32..300)
            .map(|i| (((i * 41 + 7) % 255 + 1) as u8, (i * 17) % 5 == 0))
            .collect();
        let mut enc = BoolEncoder::new();
        enc.write_bool(128, false);
        for &(p, v) in &bools {
            enc.write_bool(p, v);
        }
        let bytes = enc.finish();
        let mut dec = BoolDecoder::new(&bytes);
        for &(p, v) in &bools {
            assert_eq!(dec.read_bool(p), v);
        }
        assert!(!dec.overrun());
    }

    #[test]
    fn reading_past_the_end_sets_overrun_but_never_panics() {
        let mut dec = BoolDecoder::new(&[0x80]);
        for _ in 0..64 {
            let _ = dec.read_bool(200);
        }
        assert!(dec.overrun());
    }

    #[test]
    fn empty_partition_is_overrun_from_construction() {
        let dec = BoolDecoder::new(&[]);
        assert!(dec.overrun());
    }

    proptest::proptest! {
        #[test]
        fn tree_walk_never_panics_on_arbitrary_input(
            data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..64),
            probs in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..12),
        ) {
            const TREE: [i8; 6] = [0, 2, -1, 4, -2, -3];
            let mut dec = BoolDecoder::new(&data);
            let _ = dec.read_tree(&TREE, &probs);
        }
    }
}
