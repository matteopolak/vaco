//! Bit-level resynchronisation (part of D-22d): finding the next occurrence
//! of a fixed-length marker bit pattern in a bitstream that need not be
//! byte-aligned.
//!
//! MPEG-1/2's own slice/picture start codes are byte-aligned (already served
//! by [`vaco_bitstream::annexb`], which this crate deliberately does not
//! duplicate), but H.261's GOB start code (`GBSC`) and H.263's own GOB/slice
//! start codes are **bit**-level markers that can begin at any bit offset —
//! a genuinely different search than a byte scanner. This module is that
//! shared shape: a family supplies its own marker pattern and length, and
//! gets a bounded, panic-free search over it.

use vaco_bitstream::BitReader;

/// Search forward from the reader's current position for the next `len`-bit
/// occurrence of `pattern` (right-justified, as every other bit pattern in
/// this workspace is represented), scanning at most `max_bits` candidate
/// start positions.
///
/// On a match, the reader is left positioned **immediately after** the
/// matched pattern (i.e. having consumed it) and this returns `true`. On no
/// match within `max_bits` positions (including running off the end of the
/// buffer), the reader is left however far the search reached and this
/// returns `false` — callers that need "leave the reader untouched on
/// failure" should [`vaco_bitstream::BitReader::mark`] first.
///
/// `max_bits` bounds this function's own work against a truncated or
/// adversarial stream that never contains the marker at all: without a
/// bound, a corrupt slice with no resync point would scan to the end of an
/// arbitrarily large buffer one bit at a time. A caller resynchronising
/// within one picture typically bounds this by the picture's own remaining
/// bit count.
pub fn find_bit_pattern(r: &mut BitReader<'_>, pattern: u32, len: u8, max_bits: usize) -> bool {
    if len == 0 || len > 32 {
        return false;
    }
    let mask = if len == 32 {
        u32::MAX
    } else {
        (1u32 << len) - 1
    };
    let wanted = pattern & mask;
    for _ in 0..max_bits {
        if r.check().is_err() {
            return false;
        }
        if r.peek(u32::from(len)) == wanted {
            r.skip(u32::from(len));
            return true;
        }
        r.skip(1);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::find_bit_pattern;
    use vaco_bitstream::{BitReader, BitWriter};

    #[test]
    fn finds_a_pattern_that_starts_mid_byte() {
        let mut w = BitWriter::new();
        w.put(3, 0b010); // misalignment padding
        w.put(9, 0b1_0000_0000); // a 9-bit marker, e.g. an H.261-style GBSC prefix
        w.put(4, 0b1111); // trailing payload
        w.align_zero();
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        r.skip(3);
        assert!(find_bit_pattern(&mut r, 0b1_0000_0000, 9, 64));
        // Positioned right after the marker: the next 4 bits are the payload.
        assert_eq!(r.get(4), 0b1111);
    }

    #[test]
    fn returns_false_and_stays_bounded_when_the_pattern_never_appears() {
        let bytes = [0u8; 8];
        let mut r = BitReader::new(&bytes);
        // Looking for an all-ones marker in an all-zero buffer.
        assert!(!find_bit_pattern(&mut r, 0xFF, 8, 32));
    }

    #[test]
    fn zero_length_pattern_never_matches() {
        let bytes = [0xFFu8; 4];
        let mut r = BitReader::new(&bytes);
        assert!(!find_bit_pattern(&mut r, 0, 0, 32));
    }

    #[test]
    fn matches_immediately_at_the_current_position() {
        let bytes = [0b1010_0000u8, 0];
        let mut r = BitReader::new(&bytes);
        assert!(find_bit_pattern(&mut r, 0b1010, 4, 16));
        assert_eq!(r.bit_pos(), 4);
    }

    proptest::proptest! {
        /// No combination of arbitrary bytes, pattern, length or bound may
        /// panic — this function's whole reason to exist is searching
        /// attacker-influenced bitstream content for a resync point, so it
        /// is exactly the kind of input this project's own fuzzing
        /// discipline asks be checked, even though this crate as a whole
        /// (a library of pure functions over already-framed data, like its
        /// sibling `vaco-codec-vlc`/`vaco-codec-dsp-idct`) has no owned
        /// packet/container boundary of its own to register a fuzz target
        /// against.
        #[test]
        fn never_panics_on_arbitrary_input(
            data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..64),
            pattern in proptest::prelude::any::<u32>(),
            len in 0u8..=40,
            max_bits in 0usize..200,
        ) {
            let mut r = BitReader::new(&data);
            let _ = find_bit_pattern(&mut r, pattern, len, max_bits);
        }
    }
}
