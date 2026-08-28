//! A bounds-checked cursor over a byte slice, plus the whitespace/`#`-comment
//! tokenizer every classic PNM header (P1-P6) and PFM/PHM header shares.
//!
//! `indexing_slicing` is denied and the input is attacker-controlled, so every
//! accessor returns [`Result`] instead of using `[]`.

use vaco_core::{Error, Result};

pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    pub(crate) fn u8(&mut self) -> Result<u8> {
        let b = self.peek().ok_or(Error::UnexpectedEof)?;
        self.pos += 1;
        Ok(b)
    }

    pub(crate) fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(Error::UnexpectedEof)?;
        let out = self.data.get(self.pos..end).ok_or(Error::UnexpectedEof)?;
        self.pos = end;
        Ok(out)
    }

    /// Skip whitespace and `#`-to-end-of-line comments, the way every PNM
    /// header separates its fields.
    pub(crate) fn skip_whitespace_and_comments(&mut self) {
        loop {
            match self.peek() {
                Some(b) if b.is_ascii_whitespace() => {
                    self.pos += 1;
                }
                Some(b'#') => {
                    while !matches!(self.peek(), Some(b'\n') | None) {
                        self.pos += 1;
                    }
                }
                _ => return,
            }
        }
    }

    /// One whitespace-delimited token, comments already skipped before and
    /// (for the case of a comment immediately following) after it.
    ///
    /// # Errors
    /// [`Error::UnexpectedEof`] if no token remains.
    pub(crate) fn token(&mut self) -> Result<&'a [u8]> {
        self.skip_whitespace_and_comments();
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b.is_ascii_whitespace() {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start {
            return Err(Error::UnexpectedEof);
        }
        self.data
            .get(start..self.pos)
            .ok_or(Error::UnexpectedEof)
    }

    /// A decimal token, parsed as `u32`.
    pub(crate) fn decimal(&mut self) -> Result<u32> {
        let tok = self.token()?;
        let s = std::str::from_utf8(tok).map_err(|_| Error::InvalidData("pnm: non-ASCII number"))?;
        s.parse().map_err(|_| Error::InvalidData("pnm: bad number"))
    }

    /// Exactly one whitespace byte, the mandatory separator between a raw
    /// header's last field and the raster (the netpbm spec requires exactly
    /// one, but every encoder in practice emits `\n`; a decoder must accept
    /// any single whitespace byte since that is what the spec guarantees).
    pub(crate) fn single_whitespace(&mut self) -> Result<()> {
        let b = self.u8()?;
        if !b.is_ascii_whitespace() {
            return Err(Error::InvalidData("pnm: missing whitespace before raster"));
        }
        Ok(())
    }
}
