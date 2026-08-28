//! RFC 3550 §5.1 RTP packet header — parse and build.
//!
//! Every field here is attacker-controlled: a hostile or merely compromised
//! RTSP/RTP source chooses every byte of every packet this module parses.
//! [`RtpPacket::parse`] never indexes past a bound it has not already
//! checked and never panics on a header claiming more CSRCs or a longer
//! extension than the buffer actually holds — both are refused with
//! [`vaco_core::Error::InvalidData`], which is exactly the "15 CSRCs in a
//! 12-byte buffer" shape the fuzz target for this module exists to find.

use vaco_core::{Error, Result};

/// The only version this module (or RFC 3550) speaks.
pub const RTP_VERSION: u8 = 2;

/// The fixed 12-byte header plus the variable CSRC list and optional
/// extension header, RFC 3550 §5.1. Does not own the payload — see
/// [`RtpPacket`] for the parsed view that borrows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpHeader {
    pub version: u8,
    pub padding: bool,
    pub extension: bool,
    pub marker: bool,
    pub payload_type: u8,
    pub sequence_number: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    /// Number of CSRC identifiers, `0..=15` (the field is 4 bits).
    pub csrc_count: u8,
}

/// One parsed RTP packet: the fixed header, any CSRC list and header
/// extension, and a payload slice with RFC 3550's trailing padding already
/// stripped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpPacket<'a> {
    pub header: RtpHeader,
    /// `header.csrc_count` contributing source identifiers, network order.
    pub csrc: &'a [u8],
    /// The extension profile-defined value and payload, when `header.extension`.
    pub extension: Option<RtpExtension<'a>>,
    /// The media payload, with any RFC 3550 §5.1 padding already removed.
    pub payload: &'a [u8],
}

/// A generic (RFC 3550 §5.3.1) RTP header extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpExtension<'a> {
    /// The profile-defined identifier, interpreted by whatever RTP profile
    /// negotiated the session (e.g. RFC 8285's one-byte/two-byte header
    /// extensions use `0xBEDE`/`0x100X`). This module does not interpret it.
    pub profile: u16,
    pub data: &'a [u8],
}

const FIXED_HEADER_LEN: usize = 12;

impl RtpPacket<'_> {
    /// Parse one RTP packet from a complete UDP datagram / interleaved-TCP
    /// frame payload.
    ///
    /// # Errors
    /// [`Error::InvalidData`] if `buf` is shorter than the fixed header, the
    /// declared CSRC list or extension runs past the end of `buf`, the
    /// version is not 2, or the declared padding count exceeds what remains
    /// of the payload.
    pub fn parse(buf: &[u8]) -> Result<RtpPacket<'_>> {
        let first = *buf
            .first()
            .ok_or(Error::InvalidData("RTP packet is empty"))?;
        let second = *buf
            .get(1)
            .ok_or(Error::InvalidData("RTP packet shorter than fixed header"))?;
        let version = first >> 6;
        if version != RTP_VERSION {
            return Err(Error::InvalidData("RTP header version is not 2"));
        }
        let padding = first & 0x20 != 0;
        let extension = first & 0x10 != 0;
        let csrc_count = first & 0x0F;
        let marker = second & 0x80 != 0;
        let payload_type = second & 0x7F;

        if buf.len() < FIXED_HEADER_LEN {
            return Err(Error::InvalidData("RTP packet shorter than fixed header"));
        }
        let sequence_number = u16::from_be_bytes(read2(buf, 2)?);
        let timestamp = u32::from_be_bytes(read4(buf, 4)?);
        let ssrc = u32::from_be_bytes(read4(buf, 8)?);

        let csrc_len = usize::from(csrc_count) * 4;
        let after_fixed = FIXED_HEADER_LEN;
        let after_csrc = after_fixed
            .checked_add(csrc_len)
            .ok_or(Error::InvalidData("RTP CSRC list length overflows"))?;
        let csrc = buf
            .get(after_fixed..after_csrc)
            .ok_or(Error::InvalidData("RTP CSRC list runs past the packet"))?;

        let mut cursor = after_csrc;
        let ext = if extension {
            let profile = u16::from_be_bytes(read2(buf, cursor)?);
            let len_words = usize::from(u16::from_be_bytes(read2(buf, cursor + 2)?));
            let ext_start = cursor
                .checked_add(4)
                .ok_or(Error::InvalidData("RTP extension header overflows"))?;
            let ext_len = len_words
                .checked_mul(4)
                .ok_or(Error::InvalidData("RTP extension length overflows"))?;
            let ext_end = ext_start
                .checked_add(ext_len)
                .ok_or(Error::InvalidData("RTP extension length overflows"))?;
            let data = buf
                .get(ext_start..ext_end)
                .ok_or(Error::InvalidData("RTP extension runs past the packet"))?;
            cursor = ext_end;
            Some(RtpExtension { profile, data })
        } else {
            None
        };

        let mut payload = buf.get(cursor..).ok_or(Error::InvalidData(
            "RTP payload offset runs past the packet",
        ))?;

        if padding {
            let pad_count = usize::from(*payload.last().ok_or(Error::InvalidData(
                "RTP padding bit set on an empty payload",
            ))?);
            if pad_count == 0 || pad_count > payload.len() {
                return Err(Error::InvalidData(
                    "RTP padding count exceeds the payload length",
                ));
            }
            let keep = payload.len() - pad_count;
            payload = payload
                .get(..keep)
                .ok_or(Error::InvalidData("RTP padding arithmetic is inconsistent"))?;
        }

        Ok(RtpPacket {
            header: RtpHeader {
                version,
                padding,
                extension,
                marker,
                payload_type,
                sequence_number,
                timestamp,
                ssrc,
                csrc_count,
            },
            csrc,
            extension: ext,
            payload,
        })
    }
}

fn read2(buf: &[u8], at: usize) -> Result<[u8; 2]> {
    buf.get(at..at + 2)
        .and_then(|s| s.try_into().ok())
        .ok_or(Error::InvalidData("RTP field runs past the packet"))
}

fn read4(buf: &[u8], at: usize) -> Result<[u8; 4]> {
    buf.get(at..at + 4)
        .and_then(|s| s.try_into().ok())
        .ok_or(Error::InvalidData("RTP field runs past the packet"))
}

/// Serialise a fixed RTP header (no CSRC list, no extension) followed by
/// `payload`, into a fresh buffer ready to send.
///
/// Used by `vaco-mux-rtp`'s packetisers, and by this crate's own
/// round-trip tests. Deliberately does not support building a CSRC list or
/// an extension header — nothing in this workspace mixes RTP through a
/// mixer/translator, which is the only case that needs one.
#[must_use]
pub fn build_basic(header: &RtpHeader, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut first = RTP_VERSION << 6;
    if header.padding {
        first |= 0x20;
    }
    if header.extension {
        first |= 0x10;
    }
    first |= header.csrc_count & 0x0F;
    out.push(first);
    let mut second = header.payload_type & 0x7F;
    if header.marker {
        second |= 0x80;
    }
    out.push(second);
    out.extend_from_slice(&header.sequence_number.to_be_bytes());
    out.extend_from_slice(&header.timestamp.to_be_bytes());
    out.extend_from_slice(&header.ssrc.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "tests")]
mod tests {
    use super::*;

    fn sample_header() -> RtpHeader {
        RtpHeader {
            version: RTP_VERSION,
            padding: false,
            extension: false,
            marker: true,
            payload_type: 96,
            sequence_number: 4242,
            timestamp: 90000,
            ssrc: 0xDEAD_BEEF,
            csrc_count: 0,
        }
    }

    #[test]
    fn round_trips_a_basic_packet() {
        let built = build_basic(&sample_header(), b"payload-bytes");
        let parsed = RtpPacket::parse(&built).unwrap();
        assert_eq!(parsed.header, sample_header());
        assert_eq!(parsed.payload, b"payload-bytes");
        assert!(parsed.csrc.is_empty());
        assert!(parsed.extension.is_none());
    }

    #[test]
    fn rejects_truncated_fixed_header() {
        assert!(RtpPacket::parse(&[0x80, 0x60, 0x01]).is_err());
    }

    #[test]
    fn rejects_wrong_version() {
        // Version bits = 0, everything else zero: 12-byte buffer.
        let buf = [0u8; 12];
        assert!(RtpPacket::parse(&buf).is_err());
    }

    #[test]
    fn rejects_csrc_count_the_buffer_cannot_hold() {
        // csrc_count = 15 (0x0F) but nothing follows the fixed header.
        let mut buf = vec![0x8Fu8, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        buf.truncate(12);
        assert!(RtpPacket::parse(&buf).is_err());
    }

    #[test]
    fn rejects_padding_count_larger_than_payload() {
        let mut buf = vec![0xA0u8, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        buf.push(5); // one-byte payload claiming 5 bytes of padding
        assert!(RtpPacket::parse(&buf).is_err());
    }

    #[test]
    fn strips_padding_correctly() {
        let mut buf = vec![0xA0u8, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        buf.extend_from_slice(b"data");
        buf.push(3); // 3 bytes of padding including the count byte itself
        let parsed = RtpPacket::parse(&buf).unwrap();
        assert_eq!(parsed.payload, b"da");
    }

    #[test]
    fn parses_extension_header() {
        let mut buf = vec![0x90u8, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        buf.extend_from_slice(&0xBEDEu16.to_be_bytes());
        buf.extend_from_slice(&1u16.to_be_bytes()); // 1 word = 4 bytes
        buf.extend_from_slice(&[1, 2, 3, 4]);
        buf.extend_from_slice(b"payload");
        let parsed = RtpPacket::parse(&buf).unwrap();
        let ext = parsed.extension.unwrap();
        assert_eq!(ext.profile, 0xBEDE);
        assert_eq!(ext.data, &[1, 2, 3, 4]);
        assert_eq!(parsed.payload, b"payload");
    }

    proptest::proptest! {
        #[test]
        fn parse_never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..256)) {
            let _ = RtpPacket::parse(&bytes);
        }

        #[test]
        fn build_then_parse_round_trips(
            seq in proptest::prelude::any::<u16>(),
            ts in proptest::prelude::any::<u32>(),
            ssrc in proptest::prelude::any::<u32>(),
            pt in 0u8..128,
            marker in proptest::prelude::any::<bool>(),
            payload in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..64),
        ) {
            let header = RtpHeader {
                version: RTP_VERSION,
                padding: false,
                extension: false,
                marker,
                payload_type: pt,
                sequence_number: seq,
                timestamp: ts,
                ssrc,
                csrc_count: 0,
            };
            let built = build_basic(&header, &payload);
            let parsed = RtpPacket::parse(&built).unwrap();
            proptest::prop_assert_eq!(parsed.header, header);
            proptest::prop_assert_eq!(parsed.payload, payload.as_slice());
        }
    }
}
