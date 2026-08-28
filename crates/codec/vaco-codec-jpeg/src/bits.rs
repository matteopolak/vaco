//! Bit-level access to an entropy-coded segment.
//!
//! `vaco_bitstream::BitReader` is the workspace's usual MSB-first reader, but
//! its contract is "zero-pad past the logical end"; it has no notion of a
//! `0xFF 0x00` stuff byte or of a marker interrupting the bitstream mid-block.
//! JPEG's entropy coding needs both, so this module is a small, purpose-built
//! reader rather than a wrapper: bolting marker detection onto a reader built
//! for a different contract would be the harder path, not the reuse.

/// MSB-first bit reader over one entropy-coded segment (Annex F.1.2.3): a
/// `0xFF` byte is data only when followed by `0x00` (which this reader
/// consumes and drops); `0xFF` followed by anything else is the start of the
/// next marker, and reads from that point return zero rather than consuming
/// marker bytes.
#[derive(Debug)]
pub(crate) struct EntropyReader<'a> {
    data: &'a [u8],
    pos: usize,
    bit_buf: u32,
    bit_cnt: u32,
    /// Set once a real marker byte (not a `0xFF 0x00` stuff, not a bare fill
    /// `0xFF`) is seen. `pos` is left pointing at that marker's leading
    /// `0xFF` so the caller can read it directly.
    marker: Option<u8>,
}

impl<'a> EntropyReader<'a> {
    /// Start reading `data` at byte offset `start`.
    #[must_use]
    pub(crate) const fn new(data: &'a [u8], start: usize) -> Self {
        Self {
            data,
            pos: start,
            bit_buf: 0,
            bit_cnt: 0,
            marker: None,
        }
    }

    /// The marker byte that ended the segment, once one has been seen.
    #[must_use]
    pub(crate) const fn marker(&self) -> Option<u8> {
        self.marker
    }

    /// The raw byte cursor: the offset of the next byte this reader has not
    /// yet consumed.
    ///
    /// Once a marker has interrupted decoding this is exactly the marker's
    /// leading `0xFF`, because [`EntropyReader::next_byte`] never advances
    /// past one — which is what lets a caller discard this reader's
    /// leftover buffered bits (a restart interval need not end on a byte
    /// boundary) and resume reading raw bytes, e.g. to consume an `RSTn`
    /// marker, from exactly this offset.
    #[must_use]
    pub(crate) const fn pos(&self) -> usize {
        self.pos
    }

    /// Pull one destuffed byte, or `None` at a marker or end of data.
    fn next_byte(&mut self) -> Option<u8> {
        loop {
            let b = *self.data.get(self.pos)?;
            if b != 0xFF {
                self.pos += 1;
                return Some(b);
            }
            let Some(&b2) = self.data.get(self.pos + 1) else {
                // A trailing lone 0xFF with nothing after it: the stream was
                // truncated mid-marker. Leave `marker` unset — the caller
                // sees end-of-data the same way it would for any other
                // truncation — rather than inventing a marker value.
                return None;
            };
            match b2 {
                0x00 => {
                    self.pos += 2;
                    return Some(0xFF);
                }
                // A run of fill bytes (Annex B.1.1.5) may precede a marker;
                // skip extras and keep looking at the byte after them.
                0xFF => self.pos += 1,
                marker => {
                    self.marker = Some(marker);
                    return None;
                }
            }
        }
    }

    /// One bit, MSB first. Reads past a marker or end of data return zero,
    /// matching this crate's sticky-degrade-not-panic policy for untrusted
    /// input: a truncated scan decodes to whatever garbage the zero bits
    /// imply rather than aborting mid-block.
    pub(crate) fn get_bit(&mut self) -> u32 {
        if self.bit_cnt == 0 {
            let Some(b) = self.next_byte() else {
                return 0;
            };
            self.bit_buf = u32::from(b);
            self.bit_cnt = 8;
        }
        self.bit_cnt -= 1;
        (self.bit_buf >> self.bit_cnt) & 1
    }

    /// `n` bits, MSB first. `n <= 32`.
    pub(crate) fn get_bits(&mut self, n: u32) -> u32 {
        let mut v = 0u32;
        for _ in 0..n.min(32) {
            v = (v << 1) | self.get_bit();
        }
        v
    }
}

/// Table C.2's `EXTEND`: recover a signed magnitude from `size` bits and
/// their raw value, folding the negative half of the code space back to
/// negative numbers (Annex F.2.2.1's `RECEIVE`+`EXTEND` pair).
#[must_use]
pub(crate) fn extend(value: i32, size: u32) -> i32 {
    if size == 0 {
        return 0;
    }
    let vt = 1i32 << (size - 1);
    if value < vt {
        value - (1i32 << size) + 1
    } else {
        value
    }
}

/// The entropy-coder byte writer: an MSB-first bit sink that stuffs a `0x00`
/// after every `0xFF` byte it emits (Annex F.1.2.3), so its output can sit
/// directly between `SOS` and the next marker with no post-processing pass.
#[derive(Debug, Default)]
pub(crate) struct EntropyWriter {
    out: Vec<u8>,
    bit_buf: u32,
    bit_cnt: u32,
}

impl EntropyWriter {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            out: Vec::new(),
            bit_buf: 0,
            bit_cnt: 0,
        }
    }

    fn push_byte(&mut self, b: u8) {
        self.out.push(b);
        if b == 0xFF {
            self.out.push(0x00);
        }
    }

    /// Write the low `n` bits of `value`, MSB first. `n <= 24` (every JPEG
    /// Huffman code plus its extend bits fits comfortably inside that, so the
    /// 32-bit accumulator here never needs a multi-flush loop).
    pub(crate) fn put_bits(&mut self, n: u32, value: u32) {
        let n = n.min(24);
        for i in (0..n).rev() {
            let bit = (value >> i) & 1;
            self.bit_buf = (self.bit_buf << 1) | bit;
            self.bit_cnt += 1;
            if self.bit_cnt == 8 {
                self.push_byte((self.bit_buf & 0xFF) as u8);
                self.bit_cnt = 0;
                self.bit_buf = 0;
            }
        }
    }

    /// Flush a partial final byte, padding the low bits with `1`s — the
    /// convention Annex B.1.1.5 assumes so a decoder reading past the last
    /// real bit sees the all-ones pattern rather than a spurious zero code.
    pub(crate) fn flush_to_byte(&mut self) {
        if self.bit_cnt > 0 {
            let pad = 8 - self.bit_cnt;
            self.bit_buf = (self.bit_buf << pad) | ((1u32 << pad) - 1);
            self.push_byte((self.bit_buf & 0xFF) as u8);
            self.bit_cnt = 0;
            self.bit_buf = 0;
        }
    }

    /// Raw bytes, unstuffed markers included verbatim (callers write those
    /// through [`EntropyWriter::raw_marker`], never through `put_bits`).
    pub(crate) fn raw_marker(&mut self, bytes: &[u8]) {
        self.out.extend_from_slice(bytes);
    }

    #[must_use]
    pub(crate) fn finish(self) -> Vec<u8> {
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extend_matches_table_c2_at_every_size() {
        // size=1: {0,1} -> {-1,1}. size=3: {0..7} -> {-7,-6,-5,-4,4,5,6,7}.
        assert_eq!(extend(0, 1), -1);
        assert_eq!(extend(1, 1), 1);
        assert_eq!(extend(0, 3), -7);
        assert_eq!(extend(3, 3), -4);
        assert_eq!(extend(4, 3), 4);
        assert_eq!(extend(7, 3), 7);
        assert_eq!(extend(0, 0), 0);
    }

    #[test]
    fn a_stuffed_ff_reads_back_as_ff_and_data_continues() {
        let data = [0xFFu8, 0x00, 0xAB];
        let mut r = EntropyReader::new(&data, 0);
        assert_eq!(r.get_bits(8), 0xFF);
        assert_eq!(r.get_bits(8), 0xAB);
        assert!(r.marker().is_none());
    }

    #[test]
    fn a_real_marker_stops_the_reader_without_consuming_it() {
        let data = [0b1010_1010u8, 0xFF, 0xD9];
        let mut r = EntropyReader::new(&data, 0);
        assert_eq!(r.get_bits(8), 0b1010_1010);
        // Nothing left before the marker: further reads degrade to zero.
        assert_eq!(r.get_bits(4), 0);
        assert_eq!(r.marker(), Some(0xD9));
        assert_eq!(r.pos(), 1);
    }

    #[test]
    fn writer_round_trips_through_the_reader() {
        let mut w = EntropyWriter::new();
        w.put_bits(3, 0b101);
        w.put_bits(9, 0x1FF); // forces a 0xFF byte to be emitted and stuffed
        w.flush_to_byte();
        let bytes = w.finish();
        assert!(bytes.windows(2).any(|w| w == [0xFF, 0x00]));

        let mut r = EntropyReader::new(&bytes, 0);
        assert_eq!(r.get_bits(3), 0b101);
        assert_eq!(r.get_bits(9), 0x1FF);
    }

    #[test]
    fn truncated_input_never_panics() {
        for n in 0..3 {
            let data = [0xFFu8, 0x00, 0xAB];
            let slice = data.get(..n).unwrap_or(&[]);
            let mut r = EntropyReader::new(slice, 0);
            let _ = r.get_bits(32);
        }
    }
}
