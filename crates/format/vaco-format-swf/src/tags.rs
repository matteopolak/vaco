//! The tag-header encoding every SWF tag shares: a `u16` packing a 10-bit
//! code and a 6-bit length, escaping to a `u32` length when the 6-bit field
//! would overflow (`0x3F`, i.e. 63).
//!
//! Measured directly off real tag headers: `VideoFrame` (code 61) with a
//! 3685-byte payload encodes as `u16` `0x0F7F` (code 61 in the high 10
//! bits, `0x3F` in the low 6) followed by a `u32` `3685`; `DefineVideoStream`
//! (code 60, 10-byte payload) fits the short form directly.

use vaco_core::{Error, Result};

/// The 6-bit length value that means "read a `u32` length next".
pub const LONG_LENGTH_MARKER: u16 = 0x3F;

/// Tag codes this crate reads or writes fields of. Every other code is
/// skipped by its declared length — see `demux.rs`'s module docs.
pub const TAG_END: u16 = 0;
pub const TAG_SHOW_FRAME: u16 = 1;
pub const TAG_SOUND_STREAM_HEAD: u16 = 18;
pub const TAG_SOUND_STREAM_BLOCK: u16 = 19;
pub const TAG_DEFINE_VIDEO_STREAM: u16 = 60;
pub const TAG_VIDEO_FRAME: u16 = 61;
pub const TAG_SOUND_STREAM_HEAD2: u16 = 45;

/// One tag's header: its code and payload length, plus how many bytes the
/// header itself took (2, or 6 for the long form).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TagHeader {
    pub code: u16,
    pub len: u32,
    pub header_len: u8,
}

impl TagHeader {
    /// Parse one tag header from the start of `buf`.
    ///
    /// # Errors
    /// [`Error::UnexpectedEof`] if `buf` is too short for the header form it
    /// declares.
    pub fn parse(buf: &[u8]) -> Result<Self> {
        let raw = u16::from_le_bytes(
            buf.get(0..2)
                .and_then(|s| s.try_into().ok())
                .ok_or(Error::UnexpectedEof)?,
        );
        let code = raw >> 6;
        let short_len = raw & 0x3F;
        if short_len == LONG_LENGTH_MARKER {
            let len = u32::from_le_bytes(
                buf.get(2..6)
                    .and_then(|s| s.try_into().ok())
                    .ok_or(Error::UnexpectedEof)?,
            );
            Ok(Self {
                code,
                len,
                header_len: 6,
            })
        } else {
            Ok(Self {
                code,
                len: u32::from(short_len),
                header_len: 2,
            })
        }
    }

    /// Serialise a tag header for a payload of `len` bytes.
    ///
    /// # Errors
    /// [`Error::InvalidData`] if `code` does not fit in 10 bits.
    pub fn write(code: u16, len: u32) -> Result<Vec<u8>> {
        if code > 0x3FF {
            return Err(Error::InvalidData("swf: tag code does not fit in 10 bits"));
        }
        let mut out = Vec::new();
        if len < u32::from(LONG_LENGTH_MARKER) {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "len is checked < 0x3F (63) just above"
            )]
            let raw = (code << 6) | (len as u16);
            out.extend_from_slice(&raw.to_le_bytes());
        } else {
            let raw = (code << 6) | LONG_LENGTH_MARKER;
            out.extend_from_slice(&raw.to_le_bytes());
            out.extend_from_slice(&len.to_le_bytes());
        }
        Ok(out)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn a_short_tag_header_round_trips() {
        let bytes = TagHeader::write(TAG_DEFINE_VIDEO_STREAM, 10).unwrap();
        let h = TagHeader::parse(&bytes).unwrap();
        assert_eq!(h.code, TAG_DEFINE_VIDEO_STREAM);
        assert_eq!(h.len, 10);
        assert_eq!(h.header_len, 2);
    }

    #[test]
    fn a_long_tag_header_round_trips() {
        let bytes = TagHeader::write(TAG_VIDEO_FRAME, 3685).unwrap();
        let h = TagHeader::parse(&bytes).unwrap();
        assert_eq!(h.code, TAG_VIDEO_FRAME);
        assert_eq!(h.len, 3685);
        assert_eq!(h.header_len, 6);
    }

    /// Exactly the measured `VideoFrame` header from the module docs:
    /// code 61, length 3685 -> `u16` 0x0F7F (`(61 << 6) | 0x3F`) then `u32`
    /// 3685.
    #[test]
    fn the_measured_video_frame_header_matches_bit_for_bit() {
        let bytes = TagHeader::write(61, 3685).unwrap();
        assert_eq!(&bytes[0..2], &0x0F7Fu16.to_le_bytes());
        assert_eq!(&bytes[2..6], &3685u32.to_le_bytes());
    }

    #[test]
    fn a_length_of_exactly_62_stays_in_the_short_form() {
        let bytes = TagHeader::write(1, 62).unwrap();
        assert_eq!(bytes.len(), 2);
        let h = TagHeader::parse(&bytes).unwrap();
        assert_eq!(h.len, 62);
    }

    #[test]
    fn a_length_of_63_forces_the_long_form() {
        let bytes = TagHeader::write(1, 63).unwrap();
        assert_eq!(bytes.len(), 6);
    }
}
