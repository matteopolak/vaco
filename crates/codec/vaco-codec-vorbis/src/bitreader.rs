//! Vorbis's own bit-packing convention (spec section 2): least-significant-bit
//! first, both within a byte and across the field being assembled.
//!
//! `vaco-bitstream`'s shared [`vaco_bitstream::BitReader`] is MSB-first (see its
//! own doc comment), which is the right convention for the video codecs that
//! use it and the wrong one here, so this crate owns a small reader of its own
//! rather than bending the shared one.
//!
//! Follows the sticky-overrun shape the rest of the tree uses: reads past the
//! end of the packet return zero and set a flag, rather than failing
//! immediately. Vorbis's own spec requires exactly this for most contexts —
//! section 2.1.8 calls a truncated read "a normal mode of operation" — while a
//! handful of contexts (codebook *setup*, in particular) must instead treat
//! end-of-packet as fatal; callers that need that distinction check
//! [`BitReaderLsb::overran`] themselves at the point the spec says to.

/// LSB-first bit reader over one packet's payload.
#[derive(Debug, Clone)]
pub(crate) struct BitReaderLsb<'a> {
    data: &'a [u8],
    /// Next bit to read, counting from bit 0 (the `LSb` of byte 0).
    bit_pos: u64,
    logical_bits: u64,
    overran: bool,
}

impl<'a> BitReaderLsb<'a> {
    #[must_use]
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            bit_pos: 0,
            logical_bits: (data.len() as u64).saturating_mul(8),
            overran: false,
        }
    }

    /// Whether any read so far has run past the end of the packet.
    ///
    /// Sticky: once true, stays true for the life of this reader.
    #[must_use]
    pub(crate) const fn overran(&self) -> bool {
        self.overran
    }

    fn read_bit(&mut self) -> u32 {
        if self.bit_pos >= self.logical_bits {
            self.overran = true;
            self.bit_pos = self.bit_pos.saturating_add(1);
            return 0;
        }
        #[allow(
            clippy::integer_division,
            reason = "byte index from a bit position; the truncation is the point"
        )]
        let byte_idx = (self.bit_pos / 8) as usize;
        let bit_idx = u32::try_from(self.bit_pos % 8).unwrap_or(0);
        let byte = self.data.get(byte_idx).copied().unwrap_or(0);
        self.bit_pos = self.bit_pos.saturating_add(1);
        u32::from((byte >> bit_idx) & 1)
    }

    /// Read `n` bits (`0..=32`) as an unsigned integer, `LSb` of the field read
    /// first per spec section 2.1.4.
    ///
    /// A zero-width read returns 0 and consumes nothing (spec section 2.1.9).
    /// Past the end of the packet, missing bits read as zero and
    /// [`overran`](Self::overran) becomes true — a truncated packet never
    /// panics and never reads out of bounds.
    pub(crate) fn get(&mut self, n: u32) -> u32 {
        let n = n.min(32);
        let mut result: u32 = 0;
        for i in 0..n {
            let bit = self.read_bit();
            result |= bit << i;
        }
        result
    }

    /// Read `n` bits (`0..=32`) and sign-extend from bit `n-1`.
    pub(crate) fn get_signed(&mut self, n: u32) -> i32 {
        let n = n.min(32);
        let v = self.get(n);
        if n == 0 || n == 32 {
            return v.cast_signed();
        }
        let sign_bit = 1u32 << (n - 1);
        if v & sign_bit != 0 {
            (v | !((sign_bit << 1).wrapping_sub(1))).cast_signed()
        } else {
            v.cast_signed()
        }
    }

    pub(crate) fn get_bool(&mut self) -> bool {
        self.get(1) != 0
    }

    /// Read one bit for use as a Huffman-tree decision, MSb-first within the
    /// codeword being assembled (spec section 3.2.1's tree-walk description).
    /// This is the same underlying bit stream `get` reads from; only the
    /// grouping into a value differs.
    pub(crate) fn read_tree_bit(&mut self) -> u32 {
        self.read_bit()
    }
}

/// LSB-first bit writer, the encode-side mirror of [`BitReaderLsb`]: `put`
/// writes a field's least significant bit first, matching [`BitReaderLsb::get`]
/// bit for bit, and [`BitWriterLsb::put_tree_bit`] writes one raw stream bit,
/// matching [`BitReaderLsb::read_tree_bit`] — both are the same underlying
/// operation as [`BitWriterLsb::put`] with `n = 1`; the two names exist so a
/// call site reads the same way its decode-side counterpart does.
#[derive(Debug, Clone, Default)]
pub(crate) struct BitWriterLsb {
    bytes: Vec<u8>,
    bit_pos: u64,
}

impl BitWriterLsb {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            bytes: Vec::new(),
            bit_pos: 0,
        }
    }

    #[allow(
        clippy::integer_division,
        reason = "byte index from a bit position; the truncation is the point"
    )]
    fn put_bit(&mut self, bit: u32) {
        let byte_idx = (self.bit_pos / 8) as usize;
        let bit_idx = u32::try_from(self.bit_pos % 8).unwrap_or(0);
        while self.bytes.len() <= byte_idx {
            self.bytes.push(0);
        }
        if bit & 1 != 0
            && let Some(byte) = self.bytes.get_mut(byte_idx)
        {
            *byte |= 1 << bit_idx;
        }
        self.bit_pos = self.bit_pos.saturating_add(1);
    }

    /// Write the low `n` bits (`0..=32`) of `value`, `LSb` first — the exact
    /// inverse of [`BitReaderLsb::get`].
    pub(crate) fn put(&mut self, value: u32, n: u32) {
        let n = n.min(32);
        for i in 0..n {
            self.put_bit((value >> i) & 1);
        }
    }

    pub(crate) fn put_bool(&mut self, value: bool) {
        self.put(u32::from(value), 1);
    }

    /// Write one raw stream bit for a Huffman codeword — see the struct doc.
    pub(crate) fn put_tree_bit(&mut self, bit: u32) {
        self.put_bit(bit);
    }

    /// Finish the packet: whatever fraction of the last byte is unwritten
    /// reads back as zero, matching [`BitReaderLsb`]'s zero-padded overrun
    /// behaviour on the decode side.
    #[must_use]
    pub(crate) fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

/// `ilog(x)`: position (1-based) of the highest set bit, `0` for `x <= 0`
/// (spec section 9.2.1).
#[must_use]
pub(crate) fn ilog(x: i64) -> u32 {
    if x <= 0 {
        return 0;
    }
    64 - x.leading_zeros()
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code: byte/bit-offset splitting is exact by construction"
)]
mod tests {
    use super::*;

    #[test]
    fn spec_2_1_6_coding_example_round_trips() {
        // Section 2.1.6/2.1.7: 4-bit '12', 3-bit '-1', 7-bit '17', 13-bit '6969'
        // packed LSb-first, decode back the same widths.
        let mut bytes = [0u8; 4];
        let mut pos = 0usize;
        let put = |v: u32, n: u32, bytes: &mut [u8; 4], pos: &mut usize| {
            for i in 0..n {
                if (v >> i) & 1 != 0 {
                    bytes[*pos / 8] |= 1 << (*pos % 8);
                }
                *pos += 1;
            }
        };
        put(12, 4, &mut bytes, &mut pos);
        put(0b111, 3, &mut bytes, &mut pos);
        put(17, 7, &mut bytes, &mut pos);
        put(6969, 13, &mut bytes, &mut pos);

        let mut r = BitReaderLsb::new(&bytes);
        assert_eq!(r.get(4), 12);
        assert_eq!(r.get(3), 0b111);
        assert_eq!(r.get(7), 17);
        assert_eq!(r.get(13), 6969);
        assert!(!r.overran());
    }

    #[test]
    fn overrun_pads_with_zero_and_is_sticky() {
        let mut r = BitReaderLsb::new(&[0xFF]);
        assert_eq!(r.get(8), 0xFF);
        assert!(!r.overran());
        assert_eq!(r.get(8), 0);
        assert!(r.overran());
        assert_eq!(r.get(1), 0);
        assert!(r.overran());
    }

    #[test]
    fn zero_width_read_consumes_nothing() {
        let mut r = BitReaderLsb::new(&[0xAB]);
        assert_eq!(r.get(0), 0);
        assert_eq!(r.get(8), 0xAB);
    }

    #[test]
    fn bit_writer_round_trips_through_bit_reader() {
        let mut w = BitWriterLsb::new();
        w.put(12, 4);
        w.put(0b111, 3);
        w.put(17, 7);
        w.put(6969, 13);
        w.put_bool(true);
        let bytes = w.finish();

        let mut r = BitReaderLsb::new(&bytes);
        assert_eq!(r.get(4), 12);
        assert_eq!(r.get(3), 0b111);
        assert_eq!(r.get(7), 17);
        assert_eq!(r.get(13), 6969);
        assert!(r.get_bool());
        assert!(!r.overran());
    }

    #[test]
    fn tree_bits_written_msb_first_decode_as_the_flat_binary_value() {
        // A flat 3-bit code: writing entry 5's codeword (binary 101,
        // root-decision first) must read back as the raw 3-bit sequence
        // 1,0,1 via `read_tree_bit`, in that order.
        let mut w = BitWriterLsb::new();
        let entry = 5u32;
        for bit_index in (0..3).rev() {
            w.put_tree_bit((entry >> bit_index) & 1);
        }
        let bytes = w.finish();
        let mut r = BitReaderLsb::new(&bytes);
        let mut decoded = 0u32;
        for _ in 0..3 {
            decoded = (decoded << 1) | r.read_tree_bit();
        }
        assert_eq!(decoded, entry);
    }

    #[test]
    fn ilog_matches_spec_examples() {
        assert_eq!(ilog(0), 0);
        assert_eq!(ilog(1), 1);
        assert_eq!(ilog(2), 2);
        assert_eq!(ilog(3), 2);
        assert_eq!(ilog(4), 3);
        assert_eq!(ilog(7), 3);
        assert_eq!(ilog(-5), 0);
    }
}
