//! PES packet headers, in both syntaxes a program stream can carry
//! (ISO/IEC 11172-1 §2.4.3.7, ISO/IEC 13818-1 §2.4.3.6/.7).
//!
//! # Two envelopes, not one
//!
//! MPEG-TS only ever carries the MPEG-2 PES envelope: a flags byte with
//! `'10'` in its top two bits, then a `PES_header_data_length` byte, then
//! that many bytes of optional fields. A program stream can carry *either*
//! that envelope, or the older MPEG-1 one (ISO/IEC 11172-1 §2.4.3.7): no
//! flags byte and no length byte at all — optional stuffing (`0xFF`, up to
//! 16 bytes), an optional STD buffer scale/size field, then the timestamp
//! fields directly, with the choice signalled by a marker nibble rather than
//! a bit pair. Measured on `ffmpeg -f mpeg` output: the `mpeg`/`vcd` muxers
//! write the MPEG-1 envelope; `vob`/`svcd`/`dvd` write the MPEG-2 one. A
//! demuxer that assumes MPEG-2 syntax unconditionally — reasonable if this
//! only ever saw MPEG-TS — misframes every packet in an `mpeg`/`vcd` file
//! by at least three bytes.
//!
//! This module is **not** shared with `vaco-demux-mpegts`'s `pes` module.
//! Plan 18 §8.3 names a `vaco-format-mpeg-common` crate as PES's eventual
//! single home; that crate does not exist yet (see this crate's docs file
//! for why it was not created here). Everything in this module is written
//! independently from the cited specifications, not copied from the sibling
//! crate.

use vaco_core::Timestamp;

/// `packet_start_code_prefix`.
pub const START_CODE: [u8; 3] = [0x00, 0x00, 0x01];
/// The fixed part of every PES header: start code, `stream_id`, `PES_packet_length`.
pub const PES_PREFIX_LEN: usize = 6;

/// `stream_id` of a padding stream: payload is stuffing, never emitted.
pub const SID_PADDING: u8 = 0xBE;
/// `stream_id` of `program_stream_map`.
pub const SID_PROGRAM_STREAM_MAP: u8 = 0xBC;
/// `stream_id` of `private_stream_1`: AC-3, DTS, LPCM and DVD subtitles ride
/// here, distinguished by a one-byte sub-stream id at the front of the
/// payload — see [`crate::substream`].
pub const SID_PRIVATE_1: u8 = 0xBD;
/// `stream_id` of `private_stream_2`.
pub const SID_PRIVATE_2: u8 = 0xBF;
/// `stream_id` of `program_stream_directory`.
pub const SID_PROGRAM_STREAM_DIRECTORY: u8 = 0xFF;
/// `stream_id` of an ECM stream.
pub const SID_ECM: u8 = 0xF0;
/// `stream_id` of an EMM stream.
pub const SID_EMM: u8 = 0xF1;
/// `stream_id` of a DSM-CC stream.
pub const SID_DSMCC: u8 = 0xF2;
/// `stream_id` of an ITU-T Rec. H.222.1 type E stream.
pub const SID_H222_TYPE_E: u8 = 0xF8;

/// Whether `stream_id` carries no optional header at all in *either* syntax
/// (ISO/IEC 13818-1 Table 2-18's exclusion set; ISO/IEC 11172-1 predates the
/// DSM-CC/H.222.1-E entries, but a program stream carrying either always
/// uses the MPEG-2 table, so both are excluded here unconditionally).
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

/// Decode a 33-bit timestamp from a five-byte `PTS`/`DTS` field. Marker bits
/// are not checked — real muxers get them wrong and the reference reads the
/// value anyway.
#[must_use]
pub fn decode_timestamp(b: &[u8]) -> Option<i64> {
    let f = b.get(..5)?;
    let (b0, b1, b2, b3, b4) = (*f.first()?, *f.get(1)?, *f.get(2)?, *f.get(3)?, *f.get(4)?);
    let v = (u64::from(b0 & 0x0E) << 29)
        | (u64::from(b1) << 22)
        | (u64::from(b2 & 0xFE) << 14)
        | (u64::from(b3) << 7)
        | (u64::from(b4) >> 1);
    Some(v.cast_signed())
}

/// Which envelope a PES header used, for diagnostics and for the mux side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PesSyntax {
    /// ISO/IEC 11172-1 §2.4.3.7.
    Mpeg1,
    /// ISO/IEC 13818-1 §2.4.3.7.
    Mpeg2,
}

/// A decoded PES header, either syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PsPesHeader {
    pub stream_id: u8,
    /// `PES_packet_length`, in bytes following the six-byte prefix. Zero
    /// means unbounded — legal for video, and terminated only by the next
    /// start code. A caller must bound how many bytes it accumulates before
    /// one arrives; see `vaco-limits::Budget` at the call site.
    pub packet_length: u16,
    /// Offset of the payload from the start of the PES packet.
    pub payload_offset: usize,
    pub pts: Timestamp,
    pub dts: Timestamp,
    pub syntax: PesSyntax,
    /// `data_alignment_indicator`. Always `false` under [`PesSyntax::Mpeg1`],
    /// which has no such field.
    pub data_alignment: bool,
}

impl PsPesHeader {
    /// Parse the header at the start of `buf`.
    ///
    /// Returns `None` when the start code is missing, `stream_id` is absent,
    /// or the declared optional-header length runs past what is present — a
    /// caller holding a partial packet gets `None` and waits for more.
    #[must_use]
    pub fn parse(buf: &[u8]) -> Option<Self> {
        let prefix = buf.get(..PES_PREFIX_LEN)?;
        if prefix.get(..3)? != START_CODE {
            return None;
        }
        let stream_id = *prefix.get(3)?;
        let packet_length = u16::from_be_bytes([*prefix.get(4)?, *prefix.get(5)?]);
        let me = Self {
            stream_id,
            packet_length,
            payload_offset: PES_PREFIX_LEN,
            pts: Timestamp::NONE,
            dts: Timestamp::NONE,
            syntax: PesSyntax::Mpeg2,
            data_alignment: false,
        };
        if !has_optional_header(stream_id) {
            return Some(me);
        }
        let ext = buf.get(PES_PREFIX_LEN..)?;
        let f0 = *ext.first()?;
        if f0 & 0xC0 == 0x80 {
            me.parse_mpeg2_optional(ext)
        } else {
            me.parse_mpeg1_optional(ext)
        }
    }

    /// MPEG-2 optional header: a flags byte pair then `PES_header_data_length`.
    fn parse_mpeg2_optional(mut self, ext: &[u8]) -> Option<Self> {
        let f0 = *ext.first()?;
        self.data_alignment = f0 & 0x04 != 0;
        let f1 = *ext.get(1)?;
        let header_len = usize::from(*ext.get(2)?);
        let optional = ext.get(3..3usize.checked_add(header_len)?)?;
        self.payload_offset = PES_PREFIX_LEN.checked_add(3)?.checked_add(header_len)?;
        match f1 >> 6 {
            0b10 => self.pts = decode_timestamp(optional).map_or(Timestamp::NONE, Timestamp::new),
            0b11 => {
                self.pts = decode_timestamp(optional).map_or(Timestamp::NONE, Timestamp::new);
                self.dts = optional
                    .get(5..)
                    .and_then(decode_timestamp)
                    .map_or(Timestamp::NONE, Timestamp::new);
            }
            // '01' is forbidden and '00' means neither; both leave absent.
            _ => {}
        }
        Some(self)
    }

    /// MPEG-1 optional header: up to 16 stuffing bytes, an optional STD
    /// buffer scale/size field (marker `'01'`), then either `'0010'` + PTS,
    /// `'0011'` + PTS + DTS, or the single byte `0x0F` for neither.
    fn parse_mpeg1_optional(mut self, ext: &[u8]) -> Option<Self> {
        self.syntax = PesSyntax::Mpeg1;
        let mut i = 0usize;
        // Up to 16 stuffing bytes (0xFF); a well-formed stream never needs
        // more, and refusing past that bounds the scan on hostile input.
        while i < 16 && *ext.get(i)? == 0xFF {
            i += 1;
        }
        let b = *ext.get(i)?;
        if b & 0xC0 == 0x40 {
            // STD_buffer_scale/size: 2 bytes, no timestamp info.
            i = i.checked_add(2)?;
        }
        let b = *ext.get(i)?;
        match b >> 4 {
            0b0010 => {
                let ts = ext.get(i..i.checked_add(5)?)?;
                self.pts = decode_timestamp(ts).map_or(Timestamp::NONE, Timestamp::new);
                i = i.checked_add(5)?;
            }
            0b0011 => {
                let ts = ext.get(i..i.checked_add(5)?)?;
                self.pts = decode_timestamp(ts).map_or(Timestamp::NONE, Timestamp::new);
                i = i.checked_add(5)?;
                let ts = ext.get(i..i.checked_add(5)?)?;
                self.dts = decode_timestamp(ts).map_or(Timestamp::NONE, Timestamp::new);
                i = i.checked_add(5)?;
            }
            _ => {
                if b != 0x0F {
                    return None;
                }
                i = i.checked_add(1)?;
            }
        }
        self.payload_offset = PES_PREFIX_LEN.checked_add(i)?;
        Some(self)
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

    fn pes_mpeg2(stream_id: u8, flags: u8, optional: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut v = vec![0x00, 0x00, 0x01, stream_id, 0x00, 0x00];
        v.push(0x80);
        v.push(flags);
        v.push(optional.len() as u8);
        v.extend_from_slice(optional);
        v.extend_from_slice(payload);
        v
    }

    /// Byte-for-byte the video PES header captured from `ffmpeg -f mpeg`
    /// (MPEG-1 syntax), 2026-08-23: `00 00 01 e0 07 dc 31 00 03 7b b1 11 00
    /// 03 5f 91 ...` — PTS-and-DTS nibble `0011` directly, no flags byte.
    #[test]
    fn a_measured_mpeg1_pes_header_parses() {
        let buf = [
            0x00, 0x00, 0x01, 0xe0, 0x07, 0xdc, 0x31, 0x00, 0x03, 0x7b, 0xb1, 0x11, 0x00, 0x03,
            0x5f, 0x91, 0xAB, 0xCD,
        ];
        let h = PsPesHeader::parse(&buf).unwrap();
        assert_eq!(h.syntax, PesSyntax::Mpeg1);
        assert_eq!(h.stream_id, 0xe0);
        assert!(h.pts.ticks().is_some());
        assert!(h.dts.ticks().is_some());
        assert_eq!(h.payload(&buf), &[0xAB, 0xCD]);
    }

    #[test]
    fn an_mpeg2_pts_only_header_parses() {
        let ts = encode_ts(0b0010, 126_000);
        let buf = pes_mpeg2(0xE0, 0x80, &ts, b"payload");
        let h = PsPesHeader::parse(&buf).unwrap();
        assert_eq!(h.syntax, PesSyntax::Mpeg2);
        assert_eq!(h.pts.ticks(), Some(126_000));
        assert!(h.dts.is_none());
        assert_eq!(h.payload(&buf), b"payload");
    }

    #[test]
    fn an_mpeg1_no_timestamp_marker_is_recognised() {
        let mut buf = vec![0x00, 0x00, 0x01, 0xE0, 0x00, 0x00];
        buf.push(0x0F);
        buf.extend_from_slice(b"x");
        let h = PsPesHeader::parse(&buf).unwrap();
        assert!(h.pts.is_none());
        assert!(h.dts.is_none());
        assert_eq!(h.payload(&buf), b"x");
    }

    #[test]
    fn an_mpeg1_header_with_stuffing_skips_it() {
        let ts = encode_ts(0b0010, 42);
        let mut buf = vec![0x00, 0x00, 0x01, 0xE0, 0x00, 0x00];
        buf.extend(std::iter::repeat_n(0xFFu8, 5));
        buf.extend_from_slice(&ts);
        buf.extend_from_slice(b"y");
        let h = PsPesHeader::parse(&buf).unwrap();
        assert_eq!(h.pts.ticks(), Some(42));
        assert_eq!(h.payload(&buf), b"y");
    }

    #[test]
    fn the_eight_special_stream_ids_have_no_optional_header() {
        for id in [0xBC, 0xBE, 0xBF, 0xF0, 0xF1, 0xFF] {
            assert!(!has_optional_header(id), "{id:#x}");
            let mut buf = vec![0x00, 0x00, 0x01, id, 0x00, 0x04];
            buf.extend_from_slice(b"data");
            let h = PsPesHeader::parse(&buf).unwrap();
            assert_eq!(h.payload_offset, PES_PREFIX_LEN);
            assert_eq!(h.payload(&buf), b"data");
        }
        assert!(has_optional_header(0xE0));
        assert!(has_optional_header(0xBD));
    }

    #[test]
    fn a_missing_start_code_is_refused() {
        let mut buf = pes_mpeg2(0xE0, 0x00, &[], b"x");
        buf[2] = 0x02;
        assert!(PsPesHeader::parse(&buf).is_none());
    }

    #[test]
    fn a_truncated_header_yields_none_at_every_length() {
        let ts = encode_ts(0b0010, 42);
        let full = pes_mpeg2(0xE0, 0x80, &ts, b"payload");
        for n in 0..full.len() {
            let h = PsPesHeader::parse(&full[..n]);
            if n < 14 {
                assert!(h.is_none(), "length {n} should not parse");
            }
        }
        assert!(PsPesHeader::parse(&full).is_some());
    }

    #[test]
    fn zero_packet_length_means_unbounded() {
        let buf = pes_mpeg2(0xE0, 0x00, &[], b"x");
        let h = PsPesHeader::parse(&buf).unwrap();
        assert_eq!(h.total_len(), None);
    }

    #[test]
    fn a_declared_length_gives_a_total() {
        let ts = encode_ts(0b0010, 1);
        let buf = pes_mpeg2(0xC0, 0x80, &ts, b"abcdefgh");
        let mut buf = buf;
        let len = (buf.len() - PES_PREFIX_LEN) as u16;
        buf[4..6].copy_from_slice(&len.to_be_bytes());
        let h = PsPesHeader::parse(&buf).unwrap();
        assert_eq!(h.total_len(), Some(buf.len()));
    }

    proptest::proptest! {
        /// The 33-bit timestamp codec round-trips for every representable
        /// value, mirroring the property `vaco-demux-mpegts::pes` proves for
        /// its own (independently written) copy of the same bit layout.
        #[test]
        fn every_33_bit_value_round_trips(v in 0i64..(1i64 << 33)) {
            let f = encode_ts(0b0010, v);
            proptest::prop_assert_eq!(decode_timestamp(&f), Some(v));
        }

        /// An MPEG-2 PES header with a PTS-only flag always reports the PTS
        /// it was built with, for any 33-bit value and any payload length.
        #[test]
        fn mpeg2_pts_only_round_trips(v in 0i64..(1i64 << 33), len in 0usize..64) {
            let ts = encode_ts(0b0010, v);
            let payload = vec![0xABu8; len];
            let buf = pes_mpeg2(0xE0, 0x80, &ts, &payload);
            let h = PsPesHeader::parse(&buf).unwrap();
            proptest::prop_assert_eq!(h.pts.ticks(), Some(v));
            proptest::prop_assert_eq!(h.payload(&buf), payload.as_slice());
        }
    }
}
