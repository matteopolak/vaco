//! A bounds-checked binary cursor. `indexing_slicing` is denied and every one
//! of these formats is attacker-controlled, so every accessor returns
//! [`Result`] instead of using `[]`.

use vaco_core::{Error, Result};

pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub(crate) fn seek(&mut self, pos: usize) -> Result<()> {
        if pos > self.data.len() {
            return Err(Error::UnexpectedEof);
        }
        self.pos = pos;
        Ok(())
    }

    pub(crate) fn u8(&mut self) -> Result<u8> {
        let b = self.data.get(self.pos).copied().ok_or(Error::UnexpectedEof)?;
        self.pos += 1;
        Ok(b)
    }

    pub(crate) fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(Error::UnexpectedEof)?;
        let out = self.data.get(self.pos..end).ok_or(Error::UnexpectedEof)?;
        self.pos = end;
        Ok(out)
    }

    pub(crate) fn u16_le(&mut self) -> Result<u16> {
        let b = self.bytes(2)?;
        let arr: [u8; 2] = b.try_into().map_err(|_| Error::UnexpectedEof)?;
        Ok(u16::from_le_bytes(arr))
    }

    pub(crate) fn u32_le(&mut self) -> Result<u32> {
        let b = self.bytes(4)?;
        let arr: [u8; 4] = b.try_into().map_err(|_| Error::UnexpectedEof)?;
        Ok(u32::from_le_bytes(arr))
    }

    pub(crate) fn i32_le(&mut self) -> Result<i32> {
        Ok(self.u32_le()?.cast_signed())
    }

    pub(crate) fn u16_be(&mut self) -> Result<u16> {
        let b = self.bytes(2)?;
        let arr: [u8; 2] = b.try_into().map_err(|_| Error::UnexpectedEof)?;
        Ok(u16::from_be_bytes(arr))
    }

    pub(crate) fn u32_be(&mut self) -> Result<u32> {
        let b = self.bytes(4)?;
        let arr: [u8; 4] = b.try_into().map_err(|_| Error::UnexpectedEof)?;
        Ok(u32::from_be_bytes(arr))
    }
}
