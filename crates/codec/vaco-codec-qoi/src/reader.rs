//! A bounds-checked cursor over a byte slice.
//!
//! Every accessor returns [`Result`] instead of indexing, because
//! `indexing_slicing` is denied workspace-wide and the input here is
//! attacker-controlled: a truncated file must produce
//! [`vaco_core::Error::UnexpectedEof`], never a panic.

use vaco_core::{Error, Result};

/// A forward-only cursor over `data`.
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// A cursor starting at the beginning of `data`.
    pub(crate) const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// One byte.
    ///
    /// # Errors
    /// [`Error::UnexpectedEof`] if the cursor is at the end.
    pub(crate) fn u8(&mut self) -> Result<u8> {
        let b = self.data.get(self.pos).copied().ok_or(Error::UnexpectedEof)?;
        self.pos += 1;
        Ok(b)
    }

    /// `n` bytes, as a sub-slice.
    ///
    /// # Errors
    /// [`Error::UnexpectedEof`] if fewer than `n` bytes remain.
    pub(crate) fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(Error::UnexpectedEof)?;
        let out = self.data.get(self.pos..end).ok_or(Error::UnexpectedEof)?;
        self.pos = end;
        Ok(out)
    }

    /// A big-endian `u32`.
    ///
    /// # Errors
    /// [`Error::UnexpectedEof`] if fewer than four bytes remain.
    pub(crate) fn u32_be(&mut self) -> Result<u32> {
        let b = self.bytes(4)?;
        let arr: [u8; 4] = b.try_into().map_err(|_| Error::UnexpectedEof)?;
        Ok(u32::from_be_bytes(arr))
    }
}
