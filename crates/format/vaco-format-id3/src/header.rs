//! The `ID3v2` header and footer.
//!
//! `ID3v2.2.0` §3.1 / `ID3v2.3.0` §3.1 / `ID3v2.4.0` §3.1:
//!
//! ```text
//! "ID3"  major:u8  revision:u8  flags:u8  size:synchsafe32
//! ```
//!
//! `size` counts everything **after** the ten-byte header — every frame,
//! padding, and the extended header if present — but explicitly excludes
//! the header itself and, per `ID3v2.4.0` §3.1, the footer too. The footer
//! (`ID3v2.4.0` §3.4, present only when
//! [`Flags::FOOTER_PRESENT`] is set) is the same ten bytes with `"3DI"` in
//! place of `"ID3"`, appended after the frames — a bidirectional-search
//! convenience so a reader scanning from the end of a file can find the tag
//! without knowing where it starts.

use vaco_bitstream::ByteReader;
use vaco_core::{Error, Result};

use crate::synchsafe;

/// Bytes in a header or footer.
pub const LEN: usize = 10;

bitflags::bitflags! {
    /// The header's flags byte. Bit positions per `ID3v2.4.0` §3.1; `ID3v2.3.0`
    /// defines the same three high bits and none of the rest.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Flags: u8 {
        /// Whole-tag unsynchronisation (`ID3v2.3.0` and `ID3v2.4.0` both). In
        /// v2.4 a frame can *also* set its own per-frame unsynchronisation
        /// flag independently of this one; see `crate::frame_header`.
        const UNSYNCHRONISATION = 0x80;
        /// An extended header follows this one.
        const EXTENDED_HEADER   = 0x40;
        const EXPERIMENTAL      = 0x20;
        /// `ID3v2.4.0` only: a footer follows the last frame.
        const FOOTER_PRESENT    = 0x10;
    }
}

/// A parsed `ID3v2` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Id3v2Header {
    pub major_version: u8,
    /// Called "revision" in the spec; always `0` in every version this crate
    /// targets (2.2.0, 2.3.0, 2.4.0).
    pub revision: u8,
    pub flags: Flags,
    /// Bytes of tag data following this header: every frame plus padding,
    /// plus the extended header if present. Excludes the header itself and
    /// any footer.
    pub size: u32,
}

impl Id3v2Header {
    /// Parse a ten-byte header from the start of `data`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the first three bytes are not `"ID3"`, or
    /// fewer than ten bytes are present.
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < LEN {
            return Err(Error::InvalidData("id3: header shorter than 10 bytes"));
        }
        let mut r = ByteReader::new(data);
        let magic = r.bytes(3);
        if magic != b"ID3" {
            return Err(Error::InvalidData("id3: missing ID3 signature"));
        }
        let major_version = r.u8();
        let revision = r.u8();
        let flags = Flags::from_bits_truncate(r.u8());
        let size_bytes = <[u8; 4]>::try_from(r.bytes(4)).unwrap_or([0; 4]);
        r.check()?;
        Ok(Self {
            major_version,
            revision,
            flags,
            size: synchsafe::decode(size_bytes),
        })
    }

    /// Total tag size including this header, and the footer if present:
    /// what a caller should skip to move past the whole tag.
    #[must_use]
    pub const fn total_len(&self) -> u64 {
        let footer = if self.flags.contains(Flags::FOOTER_PRESENT) {
            LEN as u64
        } else {
            0
        };
        LEN as u64 + self.size as u64 + footer
    }
}

/// The `ID3v2.4` footer: a ten-byte mirror of the header with `"3DI"` in place
/// of `"ID3"`, appended after the last frame when
/// [`Flags::FOOTER_PRESENT`] is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Id3v2Footer {
    pub major_version: u8,
    pub revision: u8,
    pub flags: Flags,
    /// Identical to the header's `size` field: it is a mirror, not an
    /// independent count.
    pub size: u32,
}

impl Id3v2Footer {
    /// Parse a ten-byte footer from the start of `data`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidData`] if the first three bytes are not `"3DI"`, or
    /// fewer than ten bytes are present.
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < LEN {
            return Err(Error::InvalidData("id3: footer shorter than 10 bytes"));
        }
        let mut r = ByteReader::new(data);
        let magic = r.bytes(3);
        if magic != b"3DI" {
            return Err(Error::InvalidData("id3: missing 3DI footer signature"));
        }
        let major_version = r.u8();
        let revision = r.u8();
        let flags = Flags::from_bits_truncate(r.u8());
        let size_bytes = <[u8; 4]>::try_from(r.bytes(4)).unwrap_or([0; 4]);
        r.check()?;
        Ok(Self {
            major_version,
            revision,
            flags,
            size: synchsafe::decode(size_bytes),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    fn raw_header(major: u8, flags: u8, size: u32) -> Vec<u8> {
        let mut out = b"ID3".to_vec();
        out.push(major);
        out.push(0);
        out.push(flags);
        out.push(((size >> 21) & 0x7f) as u8);
        out.push(((size >> 14) & 0x7f) as u8);
        out.push(((size >> 7) & 0x7f) as u8);
        out.push((size & 0x7f) as u8);
        out
    }

    #[test]
    fn parses_the_probed_v23_header() {
        // ID3 03 00 00 / 00 00 01 36 -> size 182, matching the fixed short
        // tag captured from ffmpeg's mp3 muxer under -id3v2_version 3.
        let data = raw_header(3, 0, 182);
        let h = Id3v2Header::parse(&data).unwrap();
        assert_eq!(h.major_version, 3);
        assert_eq!(h.size, 182);
        assert!(h.flags.is_empty());
        assert_eq!(h.total_len(), 10 + 182);
    }

    #[test]
    fn footer_present_adds_ten_bytes_to_total_len() {
        let data = raw_header(4, Flags::FOOTER_PRESENT.bits(), 100);
        let h = Id3v2Header::parse(&data).unwrap();
        assert!(h.flags.contains(Flags::FOOTER_PRESENT));
        assert_eq!(h.total_len(), 10 + 100 + 10);
    }

    #[test]
    fn rejects_a_bad_signature() {
        let mut data = raw_header(3, 0, 0);
        data[0] = b'X';
        assert!(Id3v2Header::parse(&data).is_err());
    }

    #[test]
    fn footer_requires_3di() {
        let mut data = raw_header(4, 0, 50);
        data[0..3].copy_from_slice(b"3DI");
        let f = Id3v2Footer::parse(&data).unwrap();
        assert_eq!(f.size, 50);
        assert_eq!(f.major_version, 4);
    }
}
