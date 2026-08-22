//! Length-prefixed NAL framing — the ISO-BMFF in-band form declared by `avcC`
//! and `hvcC`.
//!
//! Written from ISO/IEC 14496-15 §5.3.3: units are prefixed with a big-endian
//! length whose width is `lengthSizeMinusOne + 1`, i.e. 1, 2 or 4 bytes.

use crate::ByteReader;

/// An iterator over length-prefixed NAL units.
///
/// Stops at the first prefix that does not fit or that declares more bytes than
/// remain, rather than yielding a short unit — a truncated length field is a
/// structural error, not a short read. Always terminates: every step consumes at
/// least the prefix.
///
/// # Example
///
/// ```
/// use vaco_bitstream::avcc;
///
/// let sample = [0, 0, 0, 2, 0x67, 0xAA, 0, 0, 0, 1, 0x68];
/// let units: Vec<&[u8]> = avcc::nal_units(&sample, 4).collect();
/// assert_eq!(units, vec![&[0x67u8, 0xAA][..], &[0x68][..]]);
/// ```
#[derive(Debug, Clone)]
pub struct LengthPrefixedIter<'a> {
    reader: ByteReader<'a>,
    length_size: u8,
    done: bool,
}

/// Iterate the units of a length-prefixed sample.
///
/// `length_size` must be 1, 2 or 4; any other value yields nothing.
#[must_use]
pub fn nal_units(buf: &[u8], length_size: u8) -> LengthPrefixedIter<'_> {
    LengthPrefixedIter {
        reader: ByteReader::new(buf),
        length_size,
        done: !matches!(length_size, 1 | 2 | 4),
    }
}

impl<'a> Iterator for LengthPrefixedIter<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        if self.done || self.reader.remaining() < self.length_size as usize {
            return None;
        }
        let len = match self.length_size {
            1 => u32::from(self.reader.u8()),
            2 => u32::from(self.reader.be16()),
            _ => self.reader.be32(),
        };
        let len = len as usize;
        if len == 0 || len > self.reader.remaining() {
            self.done = true;
            return None;
        }
        Some(self.reader.bytes(len))
    }
}
