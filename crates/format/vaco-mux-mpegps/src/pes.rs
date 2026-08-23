//! PES packet header encoding, both syntaxes.
//!
//! Mirrors `vaco-demux-mpegps::pes`'s parser, written independently (see
//! this crate's docs file for why the two `vaco-*-mpegps` crates do not
//! share a `vaco-format-mpeg-common` crate that does not yet exist).

/// `stream_id` of `private_stream_1`.
pub const SID_PRIVATE_1: u8 = 0xBD;
/// `stream_id` of a padding stream.
pub const SID_PADDING: u8 = 0xBE;

/// Which PES envelope to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuxPesSyntax {
    /// ISO/IEC 11172-1 §2.4.3.7: no flags byte, a marker nibble instead.
    Mpeg1,
    /// ISO/IEC 13818-1 §2.4.3.7: a flags byte pair plus header-data length.
    Mpeg2,
}

/// Encode a 33-bit PTS/DTS field with the given 4-bit marker prefix.
fn encode_timestamp(prefix: u8, ticks: i64) -> [u8; 5] {
    let v = ticks as u64 & ((1u64 << 33) - 1);
    [
        (prefix << 4) | (((v >> 30) as u8 & 0x07) << 1) | 1,
        ((v >> 22) & 0xFF) as u8,
        ((((v >> 15) & 0x7F) as u8) << 1) | 1,
        ((v >> 7) & 0xFF) as u8,
        (((v & 0x7F) as u8) << 1) | 1,
    ]
}

/// Build a complete PES packet: header plus `payload`.
///
/// `pts`/`dts` are 90 kHz tick counts; `None` omits the corresponding field
/// (and if `pts` is `None`, `dts` is ignored too — DTS without PTS is not
/// representable in either syntax).
#[must_use]
pub fn encode_pes(
    syntax: MuxPesSyntax,
    stream_id: u8,
    pts: Option<i64>,
    dts: Option<i64>,
    payload: &[u8],
) -> Vec<u8> {
    let mut optional = Vec::new();
    match syntax {
        MuxPesSyntax::Mpeg2 => {
            let mut flags1 = 0u8;
            match (pts, dts) {
                (Some(p), Some(d)) => {
                    flags1 |= 0xC0;
                    optional.extend_from_slice(&encode_timestamp(0b0011, p));
                    optional.extend_from_slice(&encode_timestamp(0b0001, d));
                }
                (Some(p), None) => {
                    flags1 |= 0x80;
                    optional.extend_from_slice(&encode_timestamp(0b0010, p));
                }
                (None, _) => {}
            }
            let mut header = vec![0x80, flags1, optional.len() as u8];
            header.extend_from_slice(&optional);
            let total = header.len() + payload.len();
            let mut v = vec![0x00, 0x00, 0x01, stream_id];
            v.extend_from_slice(&(total as u16).to_be_bytes());
            v.extend_from_slice(&header);
            v.extend_from_slice(payload);
            v
        }
        MuxPesSyntax::Mpeg1 => {
            match (pts, dts) {
                (Some(p), Some(d)) => {
                    optional.extend_from_slice(&encode_timestamp(0b0011, p));
                    optional.extend_from_slice(&encode_timestamp(0b0001, d));
                }
                (Some(p), None) => {
                    optional.extend_from_slice(&encode_timestamp(0b0010, p));
                }
                (None, _) => optional.push(0x0F),
            }
            let total = optional.len() + payload.len();
            let mut v = vec![0x00, 0x00, 0x01, stream_id];
            v.extend_from_slice(&(total as u16).to_be_bytes());
            v.extend_from_slice(&optional);
            v.extend_from_slice(payload);
            v
        }
    }
}

/// Encode a padding-stream PES packet whose total on-wire size is exactly
/// `total_len` bytes (used to pad a fixed-size pack).
///
/// # Panics
/// Never; returns a best-effort (possibly empty-payload) packet if
/// `total_len` is smaller than the six-byte prefix.
#[must_use]
pub fn encode_padding(total_len: usize) -> Vec<u8> {
    let payload_len = total_len.saturating_sub(6);
    let mut v = vec![0x00, 0x00, 0x01, SID_PADDING];
    v.extend_from_slice(&(payload_len as u16).to_be_bytes());
    v.extend(std::iter::repeat_n(0xFFu8, payload_len));
    v
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn mpeg2_pts_only_has_the_right_flag_bits() {
        let buf = encode_pes(MuxPesSyntax::Mpeg2, 0xE0, Some(90_000), None, b"x");
        assert_eq!(&buf[..4], &[0x00, 0x00, 0x01, 0xE0]);
        assert_eq!(buf[6], 0x80);
        assert_eq!(buf[7] >> 6, 0b10);
        assert_eq!(buf[8], 5); // header_data_length
        assert_eq!(&buf[14..], b"x");
    }

    #[test]
    fn mpeg2_pts_and_dts_has_the_right_flag_bits() {
        let buf = encode_pes(MuxPesSyntax::Mpeg2, 0xC0, Some(1), Some(2), b"y");
        assert_eq!(buf[7] >> 6, 0b11);
        assert_eq!(buf[8], 10);
    }

    #[test]
    fn mpeg1_no_timestamp_writes_the_bare_marker_byte() {
        let buf = encode_pes(MuxPesSyntax::Mpeg1, 0xE0, None, None, b"z");
        assert_eq!(buf[6], 0x0F);
        assert_eq!(&buf[7..], b"z");
    }

    #[test]
    fn mpeg1_pts_only_starts_with_the_0010_nibble() {
        let buf = encode_pes(MuxPesSyntax::Mpeg1, 0xE0, Some(42), None, b"w");
        assert_eq!(buf[6] >> 4, 0b0010);
        assert_eq!(&buf[11..], b"w");
    }

    #[test]
    fn padding_packet_has_the_declared_total_length() {
        let buf = encode_padding(64);
        assert_eq!(buf.len(), 64);
        assert_eq!(buf[3], SID_PADDING);
    }

    #[test]
    fn padding_below_the_prefix_length_does_not_panic() {
        let buf = encode_padding(2);
        assert!(buf.len() >= 6);
    }
}
