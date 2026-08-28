//! Bit-level I/O over one JPEG-LS entropy-coded scan.
//!
//! JPEG-LS's byte-stuffing rule (Annex A) is not JPEG's: whenever an output
//! byte equals `0xFF`, the encoder inserts a single `0` *bit* — not a whole
//! stuff byte — occupying the most significant bit position of the byte that
//! follows. Measured directly against `ffmpeg -c:v jpegls` output: every
//! literal `0xFF` byte the encoder emits is followed by a byte whose top bit
//! is `0`. `vaco-codec-jpeg`'s `EntropyReader` cannot be reused here because
//! it implements the *other* convention (`0xFF 0x00`).

use vaco_core::{Error, Result};

/// MSB-first bit writer that stuffs a `0` bit after every literal `0xFF`
/// byte.
#[derive(Debug, Default)]
pub(crate) struct BitWriter {
    out: Vec<u8>,
    cur: u8,
    nbits: u8,
}

impl BitWriter {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Append the low `n` bits of `value`, most significant first. `n` may
    /// be zero (a no-op) up to 32.
    pub(crate) fn put_bits(&mut self, value: u32, n: u32) {
        for i in (0..n).rev() {
            let bit = ((value >> i) & 1) as u8;
            self.put_bit(bit);
        }
    }

    /// Append `n` copies of `bit` (used for the unary run of a Golomb code).
    pub(crate) fn put_run(&mut self, bit: u8, n: u32) {
        for _ in 0..n {
            self.put_bit(bit);
        }
    }

    fn put_bit(&mut self, bit: u8) {
        self.cur = (self.cur << 1) | (bit & 1);
        self.nbits += 1;
        if self.nbits == 8 {
            self.out.push(self.cur);
            if self.cur == 0xFF {
                // Stuff a 0 bit as the first bit of the next byte, in place
                // of starting that byte empty.
                self.cur = 0;
                self.nbits = 1;
            } else {
                self.cur = 0;
                self.nbits = 0;
            }
        }
    }

    /// Pad the final partial byte with `1` bits (matches the reference's own
    /// marker-safe padding: a byte with no stuffing hazard, since a run of
    /// `1`s can never equal `0xFF`'s successor-of-`0xFF` stuffed pattern and
    /// cannot itself be mistaken for the start of a marker unless the byte
    /// is exactly `0xFF`, which a pad of `1`s starting from `nbits<8` never
    /// reaches from `0` accumulated bits) and return the finished byte
    /// stream.
    pub(crate) fn finish(mut self) -> Vec<u8> {
        while self.nbits != 0 {
            self.put_bit(1);
        }
        self.out
    }
}

/// MSB-first bit reader over an entropy segment, undoing the same
/// single-bit stuffing [`BitWriter`] performs.
#[derive(Debug)]
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    byte_pos: usize,
    bit_pos: u8,
    last_was_ff: bool,
}

impl<'a> BitReader<'a> {
    pub(crate) const fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_pos: 0,
            last_was_ff: false,
        }
    }

    /// One bit, MSB first.
    ///
    /// # Errors
    /// [`Error::UnexpectedEof`] once the segment is exhausted.
    pub(crate) fn get_bit(&mut self) -> Result<u8> {
        if self.bit_pos == 0 && self.last_was_ff {
            // The byte we are about to start is the one right after a
            // literal 0xFF: its top bit is a stuffed 0, not data.
            self.bit_pos = 1;
            self.last_was_ff = false;
        }
        let byte = *self
            .data
            .get(self.byte_pos)
            .ok_or(Error::UnexpectedEof)?;
        let bit = (byte >> (7 - self.bit_pos)) & 1;
        self.bit_pos += 1;
        if self.bit_pos == 8 {
            self.bit_pos = 0;
            self.byte_pos += 1;
            self.last_was_ff = byte == 0xFF;
        }
        Ok(bit)
    }

    /// `n` bits (0..=32), MSB first, as an integer.
    ///
    /// # Errors
    /// As [`BitReader::get_bit`].
    pub(crate) fn get_bits(&mut self, n: u32) -> Result<u32> {
        let mut v: u32 = 0;
        for _ in 0..n {
            v = (v << 1) | u32::from(self.get_bit()?);
        }
        Ok(v)
    }

    /// Consume leading `0` bits up to (and including) the terminating `1`,
    /// returning the count of zeros seen. Used for the unary prefix of a
    /// Golomb code; the caller is responsible for the length-limit escape.
    ///
    /// # Errors
    /// As [`BitReader::get_bit`].
    pub(crate) fn get_unary(&mut self, max_zeros: u32) -> Result<u32> {
        let mut zeros = 0u32;
        while zeros < max_zeros {
            if self.get_bit()? == 1 {
                return Ok(zeros);
            }
            zeros += 1;
        }
        // Limited-length escape: `max_zeros` zeros were seen without a `1`
        // among them. The encoder still emits a terminating `1` here (it is
        // redundant for detection, not for decoding, per T.87's own
        // rationale) — consume it so the bit cursor stays in sync.
        self.get_bit()?;
        Ok(zeros)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a test that cannot set up is a failed test"
)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_arbitrary_bit_sequences() {
        let mut w = BitWriter::new();
        let bits: Vec<(u32, u32)> = vec![(0b101, 3), (0xFF, 8), (0, 1), (0b11_0011, 6), (7, 3)];
        for &(v, n) in &bits {
            w.put_bits(v, n);
        }
        let out = w.finish();
        let mut r = BitReader::new(&out);
        for &(v, n) in &bits {
            assert_eq!(r.get_bits(n).unwrap(), v);
        }
    }

    #[test]
    fn a_literal_ff_byte_is_followed_by_a_zero_stuff_bit() {
        let mut w = BitWriter::new();
        w.put_bits(0xFF, 8);
        w.put_bits(0b1111_1111, 8);
        let out = w.finish();
        assert_eq!(out[0], 0xFF);
        assert_eq!(out[1] & 0x80, 0);
    }

    #[test]
    fn stuffing_round_trips_through_many_ff_bytes() {
        let mut w = BitWriter::new();
        for _ in 0..50 {
            w.put_bits(0xFF, 8);
        }
        let out = w.finish();
        let mut r = BitReader::new(&out);
        for _ in 0..50 {
            assert_eq!(r.get_bits(8).unwrap(), 0xFF);
        }
    }

    #[test]
    fn unary_prefix_counts_zeros_and_consumes_the_terminator() {
        let mut w = BitWriter::new();
        w.put_run(0, 5);
        w.put_bit(1);
        w.put_bits(0b101, 3);
        let out = w.finish();
        let mut r = BitReader::new(&out);
        assert_eq!(r.get_unary(100).unwrap(), 5);
        assert_eq!(r.get_bits(3).unwrap(), 0b101);
    }
}
