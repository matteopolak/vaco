//! VP8L's bit-packing convention: least-significant-bit first, both within a
//! byte and across a multi-bit field (`ReadBits(2)` is equivalent to reading
//! bit 0 then bit 1 and OR-ing the second in at position 1).
//!
//! `vaco-bitstream`'s shared reader is MSB-first, the right convention for
//! the video codecs that use it and the wrong one here, so this crate owns a
//! small reader/writer pair of its own — the same call `vaco-codec-vorbis`
//! made for the same reason.
//!
//! Follows the sticky-overrun shape the rest of the tree uses for untrusted
//! input: a read past the end of the buffer returns zero and sets a flag
//! rather than failing immediately, since [`crate::vp8l`]'s own loops are
//! bounded by the pixel count decoded from the header, never by a sentinel
//! this reader would need to search for.

/// LSB-first bit reader over a byte slice.
#[derive(Debug, Clone)]
pub(crate) struct BitReaderLsb<'a> {
    data: &'a [u8],
    bit_pos: u64,
    total_bits: u64,
    overran: bool,
}

impl<'a> BitReaderLsb<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            bit_pos: 0,
            total_bits: (data.len() as u64).saturating_mul(8),
            overran: false,
        }
    }

    /// Whether any read so far ran past the end of the buffer. Sticky.
    pub(crate) const fn overran(&self) -> bool {
        self.overran
    }

    pub(crate) fn read_bit(&mut self) -> u32 {
        if self.bit_pos >= self.total_bits {
            self.overran = true;
            self.bit_pos = self.bit_pos.saturating_add(1);
            return 0;
        }
        #[allow(
            clippy::integer_division,
            reason = "byte index from a bit position; truncation is the point"
        )]
        let byte_idx = (self.bit_pos / 8) as usize;
        let bit_idx = u32::try_from(self.bit_pos % 8).unwrap_or(0);
        let byte = self.data.get(byte_idx).copied().unwrap_or(0);
        self.bit_pos = self.bit_pos.saturating_add(1);
        u32::from((byte >> bit_idx) & 1)
    }

    /// Read `n` bits (0..=32), LSB-first, and assemble them so the first bit
    /// read is the least-significant bit of the result.
    pub(crate) fn read_bits(&mut self, n: u32) -> u32 {
        let mut value: u32 = 0;
        for i in 0..n {
            value |= self.read_bit() << i;
        }
        value
    }
}

/// LSB-first bit writer, buffering into a byte vector.
#[derive(Debug, Default)]
pub(crate) struct BitWriterLsb {
    bytes: Vec<u8>,
    cur: u8,
    nbits: u8,
}

impl BitWriterLsb {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn write_bit(&mut self, bit: u32) {
        self.cur |= ((bit & 1) as u8) << self.nbits;
        self.nbits += 1;
        if self.nbits == 8 {
            self.bytes.push(self.cur);
            self.cur = 0;
            self.nbits = 0;
        }
    }

    /// Write the low `n` bits of `value`, LSB-first (matching
    /// [`BitReaderLsb::read_bits`]'s assembly order).
    pub(crate) fn write_bits(&mut self, value: u32, n: u32) {
        for i in 0..n {
            self.write_bit((value >> i) & 1);
        }
    }

    /// Write a canonical Huffman codeword's bits root-to-leaf: the
    /// most-significant bit of `code` first. This is the one place bit
    /// order is *not* "first bit is the LSB" — a prefix code's bits are
    /// consumed by walking a binary tree one decision at a time, and the
    /// decision at depth 0 is the code's top bit by construction (matching
    /// [`super::huffman`]'s canonical assignment on both ends).
    pub(crate) fn write_code_msb_first(&mut self, code: u32, len: u8) {
        for k in (0..len).rev() {
            self.write_bit((code >> k) & 1);
        }
    }

    /// Flush the last partial byte (zero-padded) and return the buffer.
    pub(crate) fn finish(mut self) -> Vec<u8> {
        if self.nbits > 0 {
            self.bytes.push(self.cur);
        }
        self.bytes
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn round_trips_arbitrary_bit_widths() {
        let mut w = BitWriterLsb::new();
        let values = [(3u32, 2u32), (0, 1), (511, 9), (0xABCD, 16), (1, 1), (0, 0)];
        for &(v, n) in &values {
            w.write_bits(v, n);
        }
        let bytes = w.finish();
        let mut r = BitReaderLsb::new(&bytes);
        for &(v, n) in &values {
            let mask: u32 = if n == 0 { 0 } else { u32::MAX >> (32 - n) };
            assert_eq!(r.read_bits(n), v & mask);
        }
    }

    #[test]
    fn overrun_reads_zero_and_sets_flag() {
        let mut r = BitReaderLsb::new(&[0xFF]);
        assert_eq!(r.read_bits(8), 0xFF);
        assert!(!r.overran());
        assert_eq!(r.read_bits(8), 0);
        assert!(r.overran());
    }

    #[test]
    fn huffman_code_bit_order_matches_tree_walk() {
        // A 3-bit code 0b101 written MSB-first should be read back as the
        // bit sequence 1,0,1 by a plain bit-at-a-time reader.
        let mut w = BitWriterLsb::new();
        w.write_code_msb_first(0b101, 3);
        let bytes = w.finish();
        let mut r = BitReaderLsb::new(&bytes);
        assert_eq!(r.read_bit(), 1);
        assert_eq!(r.read_bit(), 0);
        assert_eq!(r.read_bit(), 1);
    }
}
