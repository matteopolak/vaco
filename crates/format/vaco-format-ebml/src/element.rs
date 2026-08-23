//! The element header, and the two length caps an `EBML` header may adjust.

use vaco_core::{Error, Result};

use crate::vint::{MAX_ID_LEN, MAX_SIZE_LEN, Size};

/// How deeply a master element may nest before a parse is abandoned.
///
/// This is a caller-visible ceiling, not part of RFC 8794 itself: a format
/// built on EBML may nest recursively (Matroska's `SimpleTag` and
/// `ChapterAtom` both do), so an attacker-chosen depth has to be turned into
/// an error rather than unbounded stack growth or allocation. Sixteen is
/// generous for every schema in the workspace today; a caller with a deeper
/// legitimate tree can track its own counter instead of using this constant.
pub const MAX_DEPTH: u8 = 16;

/// An element header: everything before the element data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub id: u32,
    pub size: Size,
    /// Byte offset of the first octet of the element ID.
    pub pos: u64,
    /// Byte offset of the first octet of the element data.
    pub data_pos: u64,
}

impl Header {
    /// One past the last octet of the element data, when the size is known.
    #[must_use]
    pub fn end(&self) -> Option<u64> {
        self.size.known().and_then(|n| self.data_pos.checked_add(n))
    }
}

/// The two length caps, as the `EBML` header declared them.
#[derive(Debug, Clone, Copy)]
pub struct Caps {
    pub max_id_len: u8,
    pub max_size_len: u8,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            max_id_len: MAX_ID_LEN,
            max_size_len: MAX_SIZE_LEN,
        }
    }
}

impl Caps {
    /// Adopt the header's declared lengths, rejecting anything above our own
    /// ceiling.
    ///
    /// RFC 8794 section 11.2.4 sets the minimum of `EBMLMaxIDLength` at 4, so
    /// a smaller declaration is treated as 4 rather than honoured — honouring
    /// it would reject a four-octet root ID on a file every other
    /// implementation reads.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] when the file asks for more than this crate
    /// will read.
    pub fn adopt(&mut self, max_id_len: u64, max_size_len: u64) -> Result<()> {
        if max_id_len > u64::from(MAX_ID_LEN) {
            return Err(Error::Unsupported("EBMLMaxIDLength above 4"));
        }
        if max_size_len > u64::from(MAX_SIZE_LEN) {
            return Err(Error::Unsupported("EBMLMaxSizeLength above 8"));
        }
        self.max_id_len = MAX_ID_LEN;
        self.max_size_len = if max_size_len == 0 {
            MAX_SIZE_LEN
        } else {
            max_size_len as u8
        };
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn a_header_asking_for_more_than_we_read_is_refused() {
        let mut caps = Caps::default();
        assert!(caps.adopt(5, 8).is_err());
        assert!(caps.adopt(4, 9).is_err());
        assert!(caps.adopt(4, 8).is_ok());
    }

    #[test]
    fn a_declared_max_id_length_below_four_is_still_four() {
        let mut caps = Caps::default();
        caps.adopt(1, 8).unwrap();
        assert_eq!(caps.max_id_len, 4);
    }
}
