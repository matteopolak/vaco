//! VP8's boolean entropy decoder, RFC 6386 §7.3 ("Actual Implementation").
//!
//! Transcribed from the specification's byte-buffered variant (not the
//! bit-at-a-time teaching version in §7.2, which it is stated to be
//! "logically identical" to): `value` holds 16 significant bits so a whole
//! input byte can be folded in at once instead of renormalising one bit at a
//! time, and `split` is compared against `value` after shifting `split` left
//! by 8 to match. RFC 6386 (`rfc-6386`) §7.3.

use crate::tree::{Tree, read_tree, read_tree_at};

/// State RFC 6386 §7.3 calls `bool_decoder`.
#[derive(Debug, Clone)]
pub struct BoolDecoder<'a> {
    input: &'a [u8],
    pos: usize,
    range: u32,
    value: u32,
    bit_count: i32,
    /// Set once a read reaches past the end of `input`. The decoder keeps
    /// producing deterministic output (zero-filled) rather than stopping, so
    /// a caller mid-tree-walk always reaches a leaf; check this once the
    /// syntax structure is fully read, matching `vaco-codec-cabac`'s
    /// `malformed()` convention.
    overrun: bool,
}

impl<'a> BoolDecoder<'a> {
    /// `init_bool_decoder`: prime `value` with the partition's first two
    /// bytes and set `range` to the full span.
    #[must_use]
    pub fn new(partition: &'a [u8]) -> Self {
        let mut d = Self {
            input: partition,
            pos: 0,
            range: 255,
            value: 0,
            bit_count: 0,
            overrun: false,
        };
        d.value = (u32::from(d.next_byte()) << 8) | u32::from(d.next_byte());
        d
    }

    fn next_byte(&mut self) -> u8 {
        if let Some(&b) = self.input.get(self.pos) {
            self.pos += 1;
            b
        } else {
            self.overrun = true;
            0
        }
    }

    /// Whether a read has gone past the end of the supplied partition.
    #[must_use]
    pub const fn overrun(&self) -> bool {
        self.overrun
    }

    /// `read_bool`: one bool at probability `prob / 256` of being zero.
    pub fn read_bool(&mut self, prob: u8) -> bool {
        let split = 1 + (((self.range - 1) * u32::from(prob)) >> 8);
        let big_split = split << 8;
        let bit = if self.value >= big_split {
            self.range -= split;
            self.value -= big_split;
            true
        } else {
            self.range = split;
            false
        };
        while self.range < 128 {
            self.value <<= 1;
            self.range <<= 1;
            self.bit_count += 1;
            if self.bit_count == 8 {
                self.bit_count = 0;
                self.value |= u32::from(self.next_byte());
            }
        }
        bit
    }

    /// `Flag`/`F`: a bool at probability 128/256 (one half).
    pub fn read_flag(&mut self) -> bool {
        self.read_bool(128)
    }

    /// `L(n)`/`Lit(n)`: an unsigned `n`-bit literal, high bit first, each bit
    /// coded at probability 128.
    pub fn read_literal(&mut self, num_bits: u32) -> u32 {
        let mut v = 0;
        for _ in 0..num_bits {
            v = (v << 1) | u32::from(self.read_flag());
        }
        v
    }

    /// `SignedLit(n)`: an `n`-bit two's-complement-style value, spec form —
    /// a sign flag (1 = negative) followed by an `(n-1)`-bit magnitude
    /// literal, so the sign contributes `-1` scaled by the same shift the
    /// magnitude bits accumulate into rather than a separate negation.
    pub fn read_signed_literal(&mut self, num_bits: u32) -> i32 {
        if num_bits == 0 {
            return 0;
        }
        let mut v: i32 = if self.read_flag() { -1 } else { 0 };
        for _ in 1..num_bits {
            v = (v << 1) | i32::from(self.read_flag());
        }
        v
    }

    /// A magnitude-then-sign value, RFC 6386's other common shape (used for
    /// quantizer/loop-filter deltas and MV components): an unsigned
    /// `n`-bit literal magnitude, followed by one flag giving the sign
    /// (`true` = negative).
    pub fn read_magnitude_and_sign(&mut self, num_bits: u32) -> i32 {
        let magnitude = self.read_literal(num_bits).cast_signed();
        if magnitude != 0 && self.read_flag() {
            magnitude.wrapping_neg()
        } else {
            magnitude
        }
    }

    /// Walk `tree` using probabilities `probs`, one bool per interior node.
    ///
    /// `probs.get(node)` rather than indexing: an undersized probability
    /// array (never legitimate, but not this decoder's job to reject) falls
    /// back to 128, keeping the walk total instead of panicking.
    pub fn read_tree(&mut self, tree: &Tree, probs: &[u8]) -> i32 {
        read_tree(tree, |node| {
            let p = probs.get(node).copied().unwrap_or(128);
            self.read_bool(p)
        })
    }

    /// [`Self::read_tree`], starting the walk at tree index `start` — RFC
    /// 6386 §13.2's "no EOB after a `DCT_0`" rule.
    pub fn read_tree_at(&mut self, tree: &Tree, start: i32, probs: &[u8]) -> i32 {
        read_tree_at(tree, start, |node| {
            let p = probs.get(node).copied().unwrap_or(128);
            self.read_bool(p)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6386 §7.3's `write_bool`/`flush_bool_encoder`, transcribed as the
    /// test oracle this decoder is checked against — necessary because there
    /// are no hand-derivable bit-exact fixtures for an arithmetic coder (see
    /// `vaco-codec-cabac`'s identical justification for shipping an encoder
    /// purely as test infrastructure).
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
    fn thirty_two_bit_magnitude_does_not_overflow_when_negated() {
        let mut enc = BoolEncoder::new();
        enc.write_bool(128, true);
        for _ in 0..31 {
            enc.write_bool(128, false);
        }
        enc.write_bool(128, true);
        let data = enc.finish();
        let mut decoder = BoolDecoder::new(&data);
        assert_eq!(decoder.read_magnitude_and_sign(32), i32::MIN);
    }

    #[test]
    fn round_trips_a_sequence_of_bools_at_varied_probabilities() {
        let bools: Vec<(u8, bool)> = (0_u32..500)
            .map(|i| {
                let prob = ((i * 37 + 5) % 255 + 1) as u8;
                let value = (i * 13) % 3 == 0;
                (prob, value)
            })
            .collect();
        let mut enc = BoolEncoder::new();
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
    fn a_literal_round_trips() {
        let mut enc = BoolEncoder::new();
        for shift in (0..16).rev() {
            enc.write_bool(128, (0xB4A3_u32 >> shift) & 1 != 0);
        }
        let bytes = enc.finish();
        let mut dec = BoolDecoder::new(&bytes);
        assert_eq!(dec.read_literal(16), 0xB4A3);
    }

    #[test]
    fn reading_past_the_end_sets_overrun_but_never_panics() {
        let mut dec = BoolDecoder::new(&[0x80]);
        for _ in 0..64 {
            let _ = dec.read_bool(128);
        }
        assert!(dec.overrun());
    }

    #[test]
    fn empty_partition_is_all_zero_and_marked_overrun() {
        let mut dec = BoolDecoder::new(&[]);
        assert!(dec.overrun());
        assert!(!dec.read_flag());
    }

    proptest::proptest! {
        #[test]
        fn tree_walk_never_panics_on_arbitrary_probs(
            data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..64),
            probs in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..12),
        ) {
            // RFC 6386 §8.2's uv_mode_tree, an arbitrary small tree.
            const TREE: [i8; 6] = [0, 2, -1, 4, -2, -3];
            let mut dec = BoolDecoder::new(&data);
            let _ = dec.read_tree(&TREE, &probs);
        }
    }
}
