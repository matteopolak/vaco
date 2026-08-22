//! PES packet headers (ISO/IEC 13818-1 §2.4.3.6 and §2.4.3.7).
//!
//! A PES packet is the envelope an elementary stream travels in. Its header
//! carries the only timestamps MPEG-TS has for presentation: 33-bit PTS and DTS
//! at 90 kHz.
//!
//! # The two shapes
//!
//! Most `stream_id` values introduce the *full* header — flags, a
//! `PES_header_data_length`, and optional fields. Eight of them do not: a
//! padding stream, a program stream map, `private_stream_2` and the CA and
//! directory streams carry their payload immediately after the six-byte
//! prefix. Getting that wrong shifts every payload by at least three bytes,
//! which looks like a codec bug rather than a framing one.
//!
//! # `PES_packet_length == 0`
//!
//! Legal for video, and universal in practice. It means "until the next PES
//! packet on this PID", which is why a video packet is exactly one
//! header-to-next-header span and why the last one is only complete at end of
//! input.

use vaco_core::Timestamp;

/// `packet_start_code_prefix`.
pub const START_CODE: [u8; 3] = [0x00, 0x00, 0x01];

/// The fixed part of every PES header.
pub const PES_PREFIX_LEN: usize = 6;

/// `stream_id` of a padding stream: payload is stuffing, never emitted.
pub const SID_PADDING: u8 = 0xBE;
/// `stream_id` of `program_stream_map`.
pub const SID_PROGRAM_STREAM_MAP: u8 = 0xBC;
/// `stream_id` of `private_stream_2` — used by DVB for some data services.
pub const SID_PRIVATE_2: u8 = 0xBF;
/// `stream_id` of an ECM stream.
pub const SID_ECM: u8 = 0xF0;
/// `stream_id` of an EMM stream.
pub const SID_EMM: u8 = 0xF1;
/// `stream_id` of a DSM-CC stream.
pub const SID_DSMCC: u8 = 0xF2;
/// `stream_id` of an ITU-T H.222.1 type E stream.
pub const SID_H222_TYPE_E: u8 = 0xF8;
/// `stream_id` of `program_stream_directory`.
pub const SID_PROGRAM_STREAM_DIRECTORY: u8 = 0xFF;

/// Whether `stream_id` introduces the full optional header.
///
/// The list is §2.4.3.7's exclusion set, verbatim.
#[must_use]
pub const fn has_optional_header(stream_id: u8) -> bool {
    !matches!(
        stream_id,
        SID_PROGRAM_STREAM_MAP
            | SID_PADDING
            | SID_PRIVATE_2
            | SID_ECM
            | SID_EMM
            | SID_PROGRAM_STREAM_DIRECTORY
            | SID_DSMCC
            | SID_H222_TYPE_E
    )
}

/// Decode a 33-bit timestamp from the five-byte `PTS`/`DTS` field.
///
/// The value is split across three runs of bits separated by marker bits,
/// which is a transmission-robustness device and not something to be clever
/// about: the marker bits are *not* checked, because real muxers get them
/// wrong and the reference reads the timestamp anyway.
#[must_use]
pub fn decode_timestamp(b: &[u8]) -> Option<i64> {
    let f = b.get(..5)?;
    let (b0, b1, b2, b3, b4) = (*f.first()?, *f.get(1)?, *f.get(2)?, *f.get(3)?, *f.get(4)?);
    let v = (u64::from(b0 & 0x0E) << 29)
        | (u64::from(b1) << 22)
        | (u64::from(b2 & 0xFE) << 14)
        | (u64::from(b3) << 7)
        | (u64::from(b4) >> 1);
    // 33 bits: always representable.
    Some(v.cast_signed())
}

/// A decoded PES header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PesHeader {
    pub stream_id: u8,
    /// `PES_packet_length`, in bytes following the six-byte prefix. Zero means
    /// unbounded.
    pub packet_length: u16,
    /// Offset of the payload from the start of the PES packet.
    pub payload_offset: usize,
    pub pts: Timestamp,
    pub dts: Timestamp,
    /// `data_alignment_indicator`: the payload starts at a codec-defined
    /// alignment point.
    pub data_alignment: bool,
    /// `PES_scrambling_control`: non-zero means the payload is CA-scrambled
    /// even though the transport layer was not.
    pub scrambling: u8,
}

impl PesHeader {
    /// Parse the header at the start of `buf`.
    ///
    /// Returns `None` when the start code is missing or the declared header
    /// length runs past what is present. A caller holding a partially received
    /// PES packet gets `None` and must wait for more, which is why this never
    /// guesses.
    #[must_use]
    pub fn parse(buf: &[u8]) -> Option<Self> {
        let prefix = buf.get(..PES_PREFIX_LEN)?;
        if prefix.get(..3)? != START_CODE {
            return None;
        }
        let stream_id = *prefix.get(3)?;
        let packet_length = u16::from_be_bytes([*prefix.get(4)?, *prefix.get(5)?]);
        let mut me = Self {
            stream_id,
            packet_length,
            payload_offset: PES_PREFIX_LEN,
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            data_alignment: false,
            scrambling: 0,
        };
        if !has_optional_header(stream_id) {
            return Some(me);
        }
        let ext = buf.get(PES_PREFIX_LEN..)?;
        let f0 = *ext.first()?;
        // The two most significant bits are '10'. A packet that fails this is
        // MPEG-1 system syntax, which does not appear inside a transport
        // stream; rejecting it stops a mis-framed payload being read as a
        // header.
        if f0 & 0xC0 != 0x80 {
            return None;
        }
        me.scrambling = (f0 >> 4) & 0x03;
        me.data_alignment = f0 & 0x04 != 0;
        let f1 = *ext.get(1)?;
        let header_len = usize::from(*ext.get(2)?);
        let optional = ext.get(3..3usize.checked_add(header_len)?)?;
        me.payload_offset = PES_PREFIX_LEN.checked_add(3)?.checked_add(header_len)?;
        match f1 >> 6 {
            // '10': PTS only.
            0b10 => me.pts = decode_timestamp(optional).map_or(Timestamp::NONE, Timestamp::new),
            // '11': PTS then DTS.
            0b11 => {
                me.pts = decode_timestamp(optional).map_or(Timestamp::NONE, Timestamp::new);
                me.dts = optional
                    .get(5..)
                    .and_then(decode_timestamp)
                    .map_or(Timestamp::NONE, Timestamp::new);
            }
            // '01' is forbidden — DTS without PTS is meaningless — and '00'
            // means neither. Both leave the timestamps absent.
            _ => {}
        }
        Some(me)
    }

    /// Total PES packet size, when the header declares one.
    #[must_use]
    pub fn total_len(&self) -> Option<usize> {
        if self.packet_length == 0 {
            None
        } else {
            PES_PREFIX_LEN.checked_add(usize::from(self.packet_length))
        }
    }

    /// Whether this is a padding stream, whose payload is discarded.
    #[must_use]
    pub const fn is_padding(&self) -> bool {
        self.stream_id == SID_PADDING
    }

    /// The payload inside a complete PES packet.
    #[must_use]
    pub fn payload<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        buf.get(self.payload_offset..).unwrap_or(&[])
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;

    /// Encode a 33-bit timestamp into the five-byte field.
    fn encode_ts(prefix: u8, v: i64) -> [u8; 5] {
        let v = v as u64;
        [
            (prefix << 4) | (((v >> 30) as u8) & 0x07) << 1 | 1,
            ((v >> 22) & 0xFF) as u8,
            ((((v >> 15) & 0x7F) as u8) << 1) | 1,
            ((v >> 7) & 0xFF) as u8,
            (((v & 0x7F) as u8) << 1) | 1,
        ]
    }

    fn pes(stream_id: u8, len: u16, flags: u8, optional: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut v = vec![0x00, 0x00, 0x01, stream_id];
        v.extend_from_slice(&len.to_be_bytes());
        v.push(0x80);
        v.push(flags);
        v.push(optional.len() as u8);
        v.extend_from_slice(optional);
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn pts_only() {
        let ts = encode_ts(0b0010, 126_000);
        let buf = pes(0xE0, 0, 0x80, &ts, b"payload");
        let h = PesHeader::parse(&buf).unwrap();
        assert_eq!(h.stream_id, 0xE0);
        assert_eq!(h.pts.ticks(), Some(126_000));
        assert!(h.dts.is_none());
        assert_eq!(h.payload(&buf), b"payload");
        assert_eq!(h.total_len(), None);
    }

    #[test]
    fn pts_and_dts() {
        let mut opt = encode_ts(0b0011, 9_000).to_vec();
        opt.extend_from_slice(&encode_ts(0b0001, 5_400));
        let buf = pes(0xE0, 0, 0xC0, &opt, b"x");
        let h = PesHeader::parse(&buf).unwrap();
        assert_eq!(h.pts.ticks(), Some(9_000));
        assert_eq!(h.dts.ticks(), Some(5_400));
    }

    #[test]
    fn the_full_thirty_three_bit_range_round_trips() {
        for v in [
            0i64,
            1,
            90_000,
            (1 << 32) - 1,
            1 << 32,
            (1 << 33) - 1,
            (1 << 33) - 2,
        ] {
            let f = encode_ts(0b0010, v);
            assert_eq!(decode_timestamp(&f), Some(v), "value {v}");
        }
    }

    #[test]
    fn a_declared_length_gives_a_total() {
        let ts = encode_ts(0b0010, 1);
        let buf = pes(0xC0, 20, 0x80, &ts, b"abcdefgh");
        let h = PesHeader::parse(&buf).unwrap();
        assert_eq!(h.total_len(), Some(26));
    }

    #[test]
    fn the_eight_special_stream_ids_have_no_optional_header() {
        for id in [0xBC, 0xBE, 0xBF, 0xF0, 0xF1, 0xF2, 0xF8, 0xFF] {
            assert!(!has_optional_header(id), "{id:#x}");
            let mut buf = vec![0x00, 0x00, 0x01, id, 0x00, 0x04];
            buf.extend_from_slice(b"data");
            let h = PesHeader::parse(&buf).unwrap();
            assert_eq!(h.payload_offset, PES_PREFIX_LEN);
            assert_eq!(h.payload(&buf), b"data");
        }
        assert!(has_optional_header(0xE0));
        assert!(has_optional_header(0xC0));
    }

    #[test]
    fn a_missing_start_code_is_refused() {
        let mut buf = pes(0xE0, 0, 0x00, &[], b"x");
        buf[2] = 0x02;
        assert!(PesHeader::parse(&buf).is_none());
    }

    #[test]
    fn a_header_length_past_the_buffer_is_refused() {
        let mut buf = pes(0xE0, 0, 0x80, &[0; 5], b"x");
        buf[8] = 200;
        assert!(PesHeader::parse(&buf).is_none());
    }

    #[test]
    fn mpeg1_syntax_is_refused_rather_than_mis_framed() {
        let mut buf = pes(0xE0, 0, 0x00, &[], b"x");
        buf[6] = 0x0F;
        assert!(PesHeader::parse(&buf).is_none());
    }

    #[test]
    fn a_truncated_header_yields_none_at_every_length() {
        let ts = encode_ts(0b0010, 42);
        let full = pes(0xE0, 0, 0x80, &ts, b"payload");
        for n in 0..full.len() {
            let h = PesHeader::parse(&full[..n]);
            if n < 14 {
                assert!(h.is_none(), "length {n} should not parse");
            }
        }
        assert!(PesHeader::parse(&full).is_some());
    }

    #[test]
    fn a_dts_flag_pattern_of_01_is_ignored() {
        let ts = encode_ts(0b0001, 5);
        let buf = pes(0xE0, 0, 0x40, &ts, b"x");
        let h = PesHeader::parse(&buf).unwrap();
        assert!(h.pts.is_none());
        assert!(h.dts.is_none());
    }

    #[test]
    fn scrambling_and_alignment_are_reported() {
        let mut buf = pes(0xE0, 0, 0x00, &[], b"x");
        buf[6] = 0x80 | 0x20 | 0x04;
        let h = PesHeader::parse(&buf).unwrap();
        assert_eq!(h.scrambling, 2);
        assert!(h.data_alignment);
    }
}
