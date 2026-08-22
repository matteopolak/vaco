//! Frame headers, one shape per major version.
//!
//! `ID3v2.2.0` §3.3: `id:3  size:u24(BE, plain)` — six bytes, **no flags at
//! all**. `ID3v2.3.0` §4 and `ID3v2.4.0` §4: `id:4  size:u32  flags:u16` — ten
//! bytes — but the two disagree on whether `size` is synchsafe; see
//! `crate::synchsafe` for the byte-level proof. Frame *flags* also differ in
//! bit position between v2.3 and v2.4, which is why [`Id3FrameFlags`] is
//! version-agnostic on the outside and decoded by two different functions.

use vaco_bitstream::ByteReader;

use crate::synchsafe;

/// Bytes in a v2.2 frame header.
pub const LEN_V2: usize = 6;
/// Bytes in a v2.3/v2.4 frame header.
pub const LEN_V34: usize = 10;

/// A parsed v2.2 frame header (id + size only; v2.2 has no frame flags).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeaderV2 {
    pub id: [u8; 3],
    pub size: u32,
}

impl FrameHeaderV2 {
    /// Parse a six-byte v2.2 frame header from the start of `data`.
    ///
    /// Returns `None` — not an error — when the id is all zero bytes, which
    /// is what padding looks like: reaching padding ends the frame walk
    /// rather than failing it. Also `None` on fewer than six bytes.
    #[must_use]
    pub fn parse(data: &[u8]) -> Option<Self> {
        let mut r = ByteReader::new(data);
        let id_bytes = r.bytes(3);
        let id = <[u8; 3]>::try_from(id_bytes).ok()?;
        let size = r.be24();
        if r.overrun() {
            return None;
        }
        if id == [0, 0, 0] {
            return None;
        }
        Some(Self { id, size })
    }
}

/// A parsed v2.3/v2.4 frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeaderV34 {
    pub id: [u8; 4],
    pub size: u32,
    pub flags: Id3FrameFlags,
}

impl FrameHeaderV34 {
    /// Parse a ten-byte frame header for major version `major` (`3` or `4`),
    /// which determines whether `size` is read as plain binary or synchsafe.
    ///
    /// `None` — not an error — for an all-zero id (padding reached) or fewer
    /// than ten bytes.
    #[must_use]
    pub fn parse(major: u8, data: &[u8]) -> Option<Self> {
        let mut r = ByteReader::new(data);
        let id_bytes = r.bytes(4);
        let id = <[u8; 4]>::try_from(id_bytes).ok()?;
        let size_bytes = <[u8; 4]>::try_from(r.bytes(4)).ok()?;
        let raw_flags = r.be16();
        if r.overrun() {
            return None;
        }
        if id == [0, 0, 0, 0] {
            return None;
        }
        let size = if major >= 4 {
            synchsafe::decode(size_bytes)
        } else {
            u32::from_be_bytes(size_bytes)
        };
        let flags = if major >= 4 {
            Id3FrameFlags::from_v4(raw_flags)
        } else {
            Id3FrameFlags::from_v3(raw_flags)
        };
        Some(Self { id, size, flags })
    }
}

bitflags::bitflags! {
    /// Frame flags, normalised to one shape regardless of which version's
    /// bit layout produced them — see [`Id3FrameFlags::from_v3`] and
    /// [`Id3FrameFlags::from_v4`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct Id3FrameFlags: u16 {
        const TAG_ALTER_PRESERVATION = 0x0001;
        const FILE_ALTER_PRESERVATION = 0x0002;
        const READ_ONLY = 0x0004;
        /// zlib-compressed content. This crate does not decompress — see
        /// `crate::frames` for how a compressed or encrypted frame is
        /// handled.
        const COMPRESSION = 0x0008;
        const ENCRYPTION = 0x0010;
        /// A one-byte group identifier is prepended to the content.
        const GROUPING = 0x0020;
        /// v2.4 only: this frame's content is independently
        /// unsynchronised, on top of (or instead of) the tag-wide flag.
        const UNSYNCHRONISATION = 0x0040;
        /// v2.4 only: a four-byte synchsafe length is prepended to the
        /// content, giving its size after undoing
        /// unsynchronisation/compression.
        const DATA_LENGTH_INDICATOR = 0x0080;
    }
}

impl Id3FrameFlags {
    /// `ID3v2.3.0` §4.1: byte 0 is `%abc00000`, byte 1 is `%ijk00000`.
    /// v2.3 has no per-frame unsynchronisation or data-length-indicator.
    #[must_use]
    pub fn from_v3(raw: u16) -> Self {
        let b0 = (raw >> 8) as u8;
        let b1 = raw as u8;
        let mut flags = Self::empty();
        flags.set(Self::TAG_ALTER_PRESERVATION, b0 & 0x80 != 0);
        flags.set(Self::FILE_ALTER_PRESERVATION, b0 & 0x40 != 0);
        flags.set(Self::READ_ONLY, b0 & 0x20 != 0);
        flags.set(Self::COMPRESSION, b1 & 0x80 != 0);
        flags.set(Self::ENCRYPTION, b1 & 0x40 != 0);
        flags.set(Self::GROUPING, b1 & 0x20 != 0);
        flags
    }

    /// `ID3v2.4.0` §4.1: byte 0 is `%0abc0000`, byte 1 is `%0h00kmnp`.
    #[must_use]
    pub fn from_v4(raw: u16) -> Self {
        let b0 = (raw >> 8) as u8;
        let b1 = raw as u8;
        let mut flags = Self::empty();
        flags.set(Self::TAG_ALTER_PRESERVATION, b0 & 0x40 != 0);
        flags.set(Self::FILE_ALTER_PRESERVATION, b0 & 0x20 != 0);
        flags.set(Self::READ_ONLY, b0 & 0x10 != 0);
        flags.set(Self::GROUPING, b1 & 0x40 != 0);
        flags.set(Self::COMPRESSION, b1 & 0x08 != 0);
        flags.set(Self::ENCRYPTION, b1 & 0x04 != 0);
        flags.set(Self::UNSYNCHRONISATION, b1 & 0x02 != 0);
        flags.set(Self::DATA_LENGTH_INDICATOR, b1 & 0x01 != 0);
        flags
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn v2_header_reads_plain_u24_size() {
        let data = [b'T', b'T', b'2', 0x00, 0x00, 0x0A];
        let h = FrameHeaderV2::parse(&data).unwrap();
        assert_eq!(h.id, *b"TT2");
        assert_eq!(h.size, 10);
    }

    #[test]
    fn v2_all_zero_id_is_padding_not_a_frame() {
        assert!(FrameHeaderV2::parse(&[0, 0, 0, 0, 0, 0]).is_none());
    }

    #[test]
    fn v3_header_reads_plain_u32_size() {
        // The probed TXXX header from -id3v2_version 3: 00 00 00 D2.
        let mut data = b"TXXX".to_vec();
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0xD2]);
        data.extend_from_slice(&[0x00, 0x00]);
        let h = FrameHeaderV34::parse(3, &data).unwrap();
        assert_eq!(h.size, 210);
    }

    #[test]
    fn v4_header_reads_synchsafe_size() {
        // The probed TXXX header from -id3v2_version 4: 00 00 01 52.
        let mut data = b"TXXX".to_vec();
        data.extend_from_slice(&[0x00, 0x00, 0x01, 0x52]);
        data.extend_from_slice(&[0x00, 0x00]);
        let h = FrameHeaderV34::parse(4, &data).unwrap();
        assert_eq!(h.size, 210);
    }

    #[test]
    fn v34_all_zero_id_is_padding_not_a_frame() {
        let data = [0u8; 10];
        assert!(FrameHeaderV34::parse(3, &data).is_none());
        assert!(FrameHeaderV34::parse(4, &data).is_none());
    }

    #[test]
    fn v3_flags_decode_compression_and_grouping() {
        let flags = Id3FrameFlags::from_v3(0x00A0);
        assert!(flags.contains(Id3FrameFlags::COMPRESSION));
        assert!(flags.contains(Id3FrameFlags::GROUPING));
        assert!(!flags.contains(Id3FrameFlags::ENCRYPTION));
    }

    #[test]
    fn v4_flags_decode_unsync_and_data_length_indicator() {
        let flags = Id3FrameFlags::from_v4(0x0003);
        assert!(flags.contains(Id3FrameFlags::UNSYNCHRONISATION));
        assert!(flags.contains(Id3FrameFlags::DATA_LENGTH_INDICATOR));
        assert!(!flags.contains(Id3FrameFlags::COMPRESSION));
    }

    #[test]
    fn truncated_header_is_none_not_a_panic() {
        assert!(FrameHeaderV2::parse(b"TT").is_none());
        assert!(FrameHeaderV34::parse(3, b"TIT2\x00\x00").is_none());
    }
}
