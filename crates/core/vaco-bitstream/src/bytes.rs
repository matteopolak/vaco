//! The endian-explicit byte reader.

use crate::{BitstreamError, Result};

/// A byte cursor with explicit endianness and the same sticky-overrun model as
/// [`BitReader`](crate::BitReader).
///
/// Containers are byte-structured, not bit-structured — MP4 boxes, Matroska
/// elements, MPEG-TS sections — so they want this rather than the bit reader.
/// Truncation returns zeros (or a short slice) and sets the flag; the caller
/// checks once per box with [`check`](ByteReader::check).
///
/// There is no native-endian accessor by design. Every media container declares
/// its byte order in its specification, so a read that does not say which one it
/// means is a bug waiting for someone's ARM build.
///
/// # Example
///
/// ```
/// use vaco_bitstream::ByteReader;
///
/// let mut r = ByteReader::new(&[0, 0, 0, 16, b'f', b't', b'y', b'p']);
/// assert_eq!(r.be32(), 16);
/// assert_eq!(r.bytes(4), b"ftyp");
/// r.check()?;
/// # Ok::<(), vaco_bitstream::BitstreamError>(())
/// ```
#[derive(Debug, Clone)]
pub struct ByteReader<'a> {
    data: &'a [u8],
    pos: usize,
    overrun: bool,
}

impl<'a> ByteReader<'a> {
    /// Read `data` from the start.
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            overrun: false,
        }
    }

    /// Take `N` bytes, zero-filled and flagged on truncation.
    #[inline]
    fn take<const N: usize>(&mut self) -> [u8; N] {
        if let Some(c) = self.data.get(self.pos..).and_then(<[u8]>::first_chunk::<N>) {
            self.pos += N;
            *c
        } else {
            self.overrun = true;
            self.pos = self.data.len();
            [0; N]
        }
    }

    /// One byte.
    #[inline]
    pub fn u8(&mut self) -> u8 {
        let [b] = self.take::<1>();
        b
    }

    /// One byte as a signed value.
    #[inline]
    pub fn i8(&mut self) -> i8 {
        self.u8().cast_signed()
    }

    /// Big-endian `u16`.
    #[inline]
    pub fn be16(&mut self) -> u16 {
        u16::from_be_bytes(self.take::<2>())
    }

    /// Little-endian `u16`.
    #[inline]
    pub fn le16(&mut self) -> u16 {
        u16::from_le_bytes(self.take::<2>())
    }

    /// Big-endian 24-bit value, zero-extended.
    #[inline]
    pub fn be24(&mut self) -> u32 {
        let [a, b, c] = self.take::<3>();
        u32::from_be_bytes([0, a, b, c])
    }

    /// Little-endian 24-bit value, zero-extended.
    #[inline]
    pub fn le24(&mut self) -> u32 {
        let [a, b, c] = self.take::<3>();
        u32::from_le_bytes([a, b, c, 0])
    }

    /// Big-endian `u32`.
    #[inline]
    pub fn be32(&mut self) -> u32 {
        u32::from_be_bytes(self.take::<4>())
    }

    /// Little-endian `u32`.
    #[inline]
    pub fn le32(&mut self) -> u32 {
        u32::from_le_bytes(self.take::<4>())
    }

    /// Big-endian `u64`.
    #[inline]
    pub fn be64(&mut self) -> u64 {
        u64::from_be_bytes(self.take::<8>())
    }

    /// Little-endian `u64`.
    #[inline]
    pub fn le64(&mut self) -> u64 {
        u64::from_le_bytes(self.take::<8>())
    }

    /// Big-endian IEEE-754 `f32`.
    #[inline]
    pub fn f32_be(&mut self) -> f32 {
        f32::from_be_bytes(self.take::<4>())
    }

    /// Little-endian IEEE-754 `f32`.
    #[inline]
    pub fn f32_le(&mut self) -> f32 {
        f32::from_le_bytes(self.take::<4>())
    }

    /// Big-endian IEEE-754 `f64`.
    #[inline]
    pub fn f64_be(&mut self) -> f64 {
        f64::from_be_bytes(self.take::<8>())
    }

    /// Little-endian IEEE-754 `f64`.
    #[inline]
    pub fn f64_le(&mut self) -> f64 {
        f64::from_le_bytes(self.take::<8>())
    }

    /// Borrow the next `n` bytes.
    ///
    /// On truncation this returns everything that was there — a *short* slice,
    /// not an empty one — and sets the flag, so a parser that wants best-effort
    /// data gets it and a parser that wants correctness sees the flag.
    #[inline]
    pub fn bytes(&mut self, n: usize) -> &'a [u8] {
        if let Some(s) = self.data.get(self.pos..).and_then(|s| s.get(..n)) {
            self.pos += n;
            s
        } else {
            self.overrun = true;
            let s = self.data.get(self.pos..).unwrap_or(&[]);
            self.pos = self.data.len();
            s
        }
    }

    /// Advance `n` bytes. Flags if that runs off the end.
    #[inline]
    pub fn skip(&mut self, n: usize) {
        match self.pos.checked_add(n) {
            Some(p) if p <= self.data.len() => self.pos = p,
            _ => {
                self.overrun = true;
                self.pos = self.data.len();
            }
        }
    }

    /// Move to an absolute offset. Flags if it is out of range.
    #[inline]
    pub fn seek(&mut self, pos: usize) {
        if pos > self.data.len() {
            self.overrun = true;
            self.pos = self.data.len();
        } else {
            self.pos = pos;
        }
    }

    /// The current offset.
    #[must_use]
    pub const fn pos(&self) -> usize {
        self.pos
    }

    /// Bytes left. Zero once flagged.
    #[must_use]
    pub const fn remaining(&self) -> usize {
        if self.overrun {
            return 0;
        }
        self.data.len() - self.pos
    }

    /// The unread bytes, without consuming them.
    #[must_use]
    pub fn rest(&self) -> &'a [u8] {
        self.data.get(self.pos..).unwrap_or(&[])
    }

    /// Whether a read has run past the end.
    #[must_use]
    pub const fn overrun(&self) -> bool {
        self.overrun
    }

    /// The end-of-structure check.
    ///
    /// # Errors
    ///
    /// [`BitstreamError::Overrun`] if any read ran past the end.
    pub const fn check(&self) -> Result<()> {
        if self.overrun {
            return Err(BitstreamError::Overrun);
        }
        Ok(())
    }

    /// A reader over a sub-range, for a sized element inside a container.
    ///
    /// The sub-reader cannot see past its window however malformed its contents,
    /// which is what stops a nested box from reading its parent's siblings.
    /// Advances this reader past the window.
    #[must_use]
    pub fn sub(&mut self, n: usize) -> Self {
        Self::new(self.bytes(n))
    }
}
