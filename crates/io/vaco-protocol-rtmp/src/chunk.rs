//! One chunk's basic header, message header, and extended timestamp —
//! stateless codecs. [`crate::message`] holds the per-chunk-stream state
//! (delta compression, message reassembly) these build on.
//!
//! Adobe's RTMP specification §5.3 (`adobe-rtmp-spec-1.0`, `Vaco-Spec-Ref`
//! carries this in the commit that adds this file).

use vaco_protocol_core::{ProtocolError, Result};

/// The sentinel timestamp/delta value that means "the real value is in a
/// following 4-byte extended timestamp field", per §5.3.1.3.
pub const EXTENDED_TIMESTAMP: u32 = 0x00ff_ffff;

const SCHEME: &str = "rtmp";

fn malformed(detail: &'static str) -> ProtocolError {
    ProtocolError::Malformed {
        scheme: SCHEME,
        detail,
    }
}

fn u24_be(bytes: [u8; 3]) -> u32 {
    let [b0, b1, b2] = bytes;
    (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2)
}

fn write_u24_be(v: u32) -> [u8; 3] {
    // `v` is always masked to 24 bits by the caller before this is used, so
    // the top byte of the big-endian u32 is always zero.
    let [_, b1, b2, b3] = v.to_be_bytes();
    [b1, b2, b3]
}

/// A chunk's basic header: which of the four message-header shapes follows,
/// and which chunk stream this chunk belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BasicHeader {
    /// 0-3, selects the message-header shape.
    pub fmt: u8,
    /// Chunk stream ID. 2 and up in the one-byte form; the two- and
    /// three-byte forms extend the range for higher IDs.
    pub csid: u32,
}

/// Encode a basic header (1, 2 or 3 bytes depending on `csid`).
///
/// # Errors
/// [`ProtocolError::Malformed`] if `fmt` is not in `0..=3` or `csid` is 0,
/// 1 (reserved to select the wider forms) or too large for the three-byte
/// form.
pub fn encode_basic_header(fmt: u8, csid: u32, out: &mut Vec<u8>) -> Result<()> {
    if fmt > 3 {
        return Err(malformed("chunk fmt must be 0..=3"));
    }
    let fmt_bits = fmt << 6;
    match csid {
        0 | 1 => Err(malformed(
            "chunk stream id 0 and 1 are reserved selectors, not values",
        )),
        2..=63 => {
            out.push(fmt_bits | u8::try_from(csid).unwrap_or(0));
            Ok(())
        }
        64..=319 => {
            out.push(fmt_bits);
            out.push(u8::try_from(csid - 64).unwrap_or(0));
            Ok(())
        }
        320..=65_599 => {
            let rel = csid - 64;
            out.push(fmt_bits | 1);
            out.push(u8::try_from(rel & 0xff).unwrap_or(0));
            out.push(u8::try_from(rel >> 8).unwrap_or(0));
            Ok(())
        }
        _ => Err(malformed(
            "chunk stream id exceeds the three-byte form's range",
        )),
    }
}

/// Decode a basic header from the start of `input`.
///
/// # Errors
/// [`ProtocolError::Malformed`] never — returns `Ok(None)` instead when
/// `input` does not yet hold a complete basic header, so a caller reading
/// from a live stream can simply ask for more bytes.
///
/// # Returns
/// `Ok(Some((header, bytes_consumed)))`.
pub fn decode_basic_header(input: &[u8]) -> Result<Option<(BasicHeader, usize)>> {
    let Some(&first) = input.first() else {
        return Ok(None);
    };
    let fmt = first >> 6;
    let low6 = first & 0x3f;
    match low6 {
        0 => {
            let Some(&second) = input.get(1) else {
                return Ok(None);
            };
            Ok(Some((
                BasicHeader {
                    fmt,
                    csid: u32::from(second) + 64,
                },
                2,
            )))
        }
        1 => {
            let (Some(&b1), Some(&b2)) = (input.get(1), input.get(2)) else {
                return Ok(None);
            };
            let csid = u32::from(b1) + (u32::from(b2) << 8) + 64;
            Ok(Some((BasicHeader { fmt, csid }, 3)))
        }
        csid => Ok(Some((
            BasicHeader {
                fmt,
                csid: u32::from(csid),
            },
            1,
        ))),
    }
}

/// A chunk message header, one of the four shapes named by the basic
/// header's `fmt`. Timestamps here are exactly what was on the wire (an
/// absolute value for `Type0`, a delta for `Type1`/`Type2`) — resolving
/// deltas against previous chunks on the same stream is
/// [`crate::message::Dechunker`]'s job, not this module's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageHeader {
    /// 11 bytes: full header, used at the start of a new message whose
    /// stream ID or type could differ from the chunk stream's last one.
    Type0 {
        timestamp: u32,
        message_length: u32,
        message_type_id: u8,
        message_stream_id: u32,
    },
    /// 7 bytes: message stream ID unchanged from the last `Type0`/`Type1`
    /// on this chunk stream.
    Type1 {
        timestamp_delta: u32,
        message_length: u32,
        message_type_id: u8,
    },
    /// 3 bytes: only the timestamp delta changes.
    Type2 { timestamp_delta: u32 },
    /// 0 bytes: every field (including the timestamp/delta) repeats.
    Type3,
}

impl MessageHeader {
    /// The `fmt` value this shape is encoded under.
    #[must_use]
    pub const fn fmt(&self) -> u8 {
        match self {
            Self::Type0 { .. } => 0,
            Self::Type1 { .. } => 1,
            Self::Type2 { .. } => 2,
            Self::Type3 => 3,
        }
    }

    /// Encode this header's fixed-size fields (not the basic header, not
    /// any extended timestamp — [`crate::message`] adds those).
    pub fn encode(&self, out: &mut Vec<u8>) {
        match *self {
            Self::Type0 {
                timestamp,
                message_length,
                message_type_id,
                message_stream_id,
            } => {
                out.extend_from_slice(&write_u24_be(timestamp.min(EXTENDED_TIMESTAMP)));
                out.extend_from_slice(&write_u24_be(message_length & 0x00ff_ffff));
                out.push(message_type_id);
                // The one little-endian field in an otherwise big-endian
                // protocol — Adobe's spec §5.3.1.2.1 gives it that way, not
                // a transcription slip.
                out.extend_from_slice(&message_stream_id.to_le_bytes());
            }
            Self::Type1 {
                timestamp_delta,
                message_length,
                message_type_id,
            } => {
                out.extend_from_slice(&write_u24_be(timestamp_delta.min(EXTENDED_TIMESTAMP)));
                out.extend_from_slice(&write_u24_be(message_length & 0x00ff_ffff));
                out.push(message_type_id);
            }
            Self::Type2 { timestamp_delta } => {
                out.extend_from_slice(&write_u24_be(timestamp_delta.min(EXTENDED_TIMESTAMP)));
            }
            Self::Type3 => {}
        }
    }

    /// Decode the fixed-size portion for `fmt`, from the start of `input`.
    /// Does not consume or interpret any extended timestamp field.
    ///
    /// # Errors
    /// Never fails; returns `Ok(None)` on a short read so a stream reader
    /// can ask for more bytes.
    pub fn decode(fmt: u8, input: &[u8]) -> Result<Option<(Self, usize)>> {
        match fmt {
            0 => {
                let Some(bytes) = input.get(..11) else {
                    return Ok(None);
                };
                let Ok(arr) = <[u8; 11]>::try_from(bytes) else {
                    return Ok(None);
                };
                let [t0, t1, t2, l0, l1, l2, type_id, s0, s1, s2, s3] = arr;
                Ok(Some((
                    Self::Type0 {
                        timestamp: u24_be([t0, t1, t2]),
                        message_length: u24_be([l0, l1, l2]),
                        message_type_id: type_id,
                        message_stream_id: u32::from_le_bytes([s0, s1, s2, s3]),
                    },
                    11,
                )))
            }
            1 => {
                let Some(bytes) = input.get(..7) else {
                    return Ok(None);
                };
                let Ok(arr) = <[u8; 7]>::try_from(bytes) else {
                    return Ok(None);
                };
                let [t0, t1, t2, l0, l1, l2, type_id] = arr;
                Ok(Some((
                    Self::Type1 {
                        timestamp_delta: u24_be([t0, t1, t2]),
                        message_length: u24_be([l0, l1, l2]),
                        message_type_id: type_id,
                    },
                    7,
                )))
            }
            2 => {
                let Some(bytes) = input.get(..3) else {
                    return Ok(None);
                };
                let Ok(arr) = <[u8; 3]>::try_from(bytes) else {
                    return Ok(None);
                };
                Ok(Some((
                    Self::Type2 {
                        timestamp_delta: u24_be(arr),
                    },
                    3,
                )))
            }
            3 => Ok(Some((Self::Type3, 0))),
            _ => Err(malformed("chunk fmt must be 0..=3")),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn basic_header_one_byte_round_trips() {
        let mut buf = Vec::new();
        encode_basic_header(2, 5, &mut buf).unwrap();
        assert_eq!(buf, vec![0b1000_0101]);
        let (h, n) = decode_basic_header(&buf).unwrap().unwrap();
        assert_eq!(n, 1);
        assert_eq!(h, BasicHeader { fmt: 2, csid: 5 });
    }

    #[test]
    fn basic_header_two_byte_form_for_csid_64_to_319() {
        let mut buf = Vec::new();
        encode_basic_header(0, 64, &mut buf).unwrap();
        assert_eq!(buf, vec![0x00, 0x00]);
        encode_basic_header(0, 319, &mut buf).unwrap();
        let (h, n) = decode_basic_header(&buf[2..]).unwrap().unwrap();
        assert_eq!(n, 2);
        assert_eq!(h.csid, 319);
    }

    #[test]
    fn basic_header_three_byte_form_for_csid_320_and_up() {
        let mut buf = Vec::new();
        encode_basic_header(1, 65599, &mut buf).unwrap();
        assert_eq!(buf.len(), 3);
        let (h, n) = decode_basic_header(&buf).unwrap().unwrap();
        assert_eq!(n, 3);
        assert_eq!(
            h,
            BasicHeader {
                fmt: 1,
                csid: 65599
            }
        );
    }

    #[test]
    fn basic_header_rejects_reserved_csid_values() {
        let mut buf = Vec::new();
        assert!(encode_basic_header(0, 0, &mut buf).is_err());
        assert!(encode_basic_header(0, 1, &mut buf).is_err());
    }

    #[test]
    fn decode_basic_header_reports_short_input_as_none_not_error() {
        assert_eq!(decode_basic_header(&[]).unwrap(), None);
        // fmt bits set, low6 == 0 selects the two-byte form, second byte
        // missing.
        assert_eq!(decode_basic_header(&[0x00]).unwrap(), None);
    }

    #[test]
    fn message_header_type0_round_trips() {
        let h = MessageHeader::Type0 {
            timestamp: 12345,
            message_length: 999,
            message_type_id: 9,
            message_stream_id: 1,
        };
        let mut buf = Vec::new();
        h.encode(&mut buf);
        assert_eq!(buf.len(), 11);
        let (decoded, n) = MessageHeader::decode(0, &buf).unwrap().unwrap();
        assert_eq!(n, 11);
        assert_eq!(decoded, h);
    }

    #[test]
    fn message_header_message_stream_id_is_little_endian_on_the_wire() {
        let h = MessageHeader::Type0 {
            timestamp: 0,
            message_length: 0,
            message_type_id: 0,
            message_stream_id: 0x0102_0304,
        };
        let mut buf = Vec::new();
        h.encode(&mut buf);
        assert_eq!(&buf[7..11], &[0x04, 0x03, 0x02, 0x01]);
    }

    #[test]
    fn message_header_type3_is_zero_bytes() {
        let mut buf = Vec::new();
        MessageHeader::Type3.encode(&mut buf);
        assert!(buf.is_empty());
        let (decoded, n) = MessageHeader::decode(3, &[]).unwrap().unwrap();
        assert_eq!(n, 0);
        assert_eq!(decoded, MessageHeader::Type3);
    }

    #[test]
    fn message_header_decode_short_input_is_none() {
        assert_eq!(MessageHeader::decode(1, &[0u8; 6]).unwrap(), None);
    }

    proptest::proptest! {
        #[test]
        fn basic_header_round_trips_for_any_valid_csid(
            fmt in 0u8..=3,
            csid in 2u32..=65_599,
        ) {
            let mut buf = Vec::new();
            encode_basic_header(fmt, csid, &mut buf).unwrap();
            let (h, n) = decode_basic_header(&buf).unwrap().unwrap();
            proptest::prop_assert_eq!(n, buf.len());
            proptest::prop_assert_eq!(h, BasicHeader { fmt, csid });
        }

        #[test]
        fn type0_header_round_trips_for_any_field_values(
            timestamp in 0u32..EXTENDED_TIMESTAMP,
            message_length in 0u32..=0x00ff_ffff,
            message_type_id in 0u8..=255,
            message_stream_id in 0u32..=u32::MAX,
        ) {
            let h = MessageHeader::Type0 { timestamp, message_length, message_type_id, message_stream_id };
            let mut buf = Vec::new();
            h.encode(&mut buf);
            let (decoded, n) = MessageHeader::decode(0, &buf).unwrap().unwrap();
            proptest::prop_assert_eq!(n, 11);
            proptest::prop_assert_eq!(decoded, h);
        }
    }
}
