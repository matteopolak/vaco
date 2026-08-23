//! PES packet header encoding (ISO/IEC 13818-1 §2.4.3.6/§2.4.3.7).
//!
//! Written independently of [`vaco_demux_mpegts::pes`]'s parser — the two are
//! checked against each other by this crate's tests, which is a stronger
//! oracle than either one re-reading itself. The 33-bit PTS/DTS field with its
//! interleaved marker bits is the one part of this whole muxer most likely to
//! be silently wrong (see the crate docs), so a proptest round-trips every
//! value through [`vaco_demux_mpegts::pes::decode_timestamp`] directly.

/// `stream_id` for a video elementary stream (the low nibble is unused; every
/// video stream shares `0xE0`, since this muxer never needs the four-bit
/// stream-number extension that `0xE0..=0xEF` allows for).
pub const SID_VIDEO: u8 = 0xE0;
/// `stream_id` for an MPEG audio elementary stream.
pub const SID_AUDIO: u8 = 0xC0;
/// `stream_id` of `private_stream_1`: AC-3, E-AC-3, DTS, `TrueHD`, PCM and DVB
/// subtitles all travel under this one id in a transport stream, and are told
/// apart by PID and PMT `stream_type`/descriptor rather than by `stream_id`.
pub const SID_PRIVATE_1: u8 = 0xBD;

/// Encode a 33-bit PTS/DTS value with the four-bit marker `prefix` ISO/IEC
/// 13818-1 Table 2-21 puts in front of it (`0010` for PTS-only, `0011` for
/// PTS-with-DTS's PTS, `0001` for PTS-with-DTS's DTS).
///
/// The three marker bits interleaved with the payload (one after each of the
/// three fields) are always `1`; a real muxer never varies them, and neither
/// does a real demuxer check them — [`vaco_demux_mpegts::pes::decode_timestamp`]
/// says so explicitly.
#[must_use]
fn encode_timestamp(prefix: u8, ticks: i64) -> [u8; 5] {
    // 33 bits: the field cannot hold more, so higher bits are simply dropped.
    // A muxer that receives a timestamp already reduced modulo 2^33 (as every
    // stream time base in this crate is) never exercises this in practice;
    // `ticks as u64` is well-defined for negative inputs too (two's complement
    // truncation), so nothing here can panic.
    let v = (ticks as u64) & ((1u64 << 33) - 1);
    [
        (prefix << 4) | (((v >> 30) as u8 & 0x07) << 1) | 1,
        ((v >> 22) & 0xFF) as u8,
        ((((v >> 15) & 0x7F) as u8) << 1) | 1,
        ((v >> 7) & 0xFF) as u8,
        (((v & 0x7F) as u8) << 1) | 1,
    ]
}

/// What timestamps a PES header carries, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PesTimestamps {
    None,
    PtsOnly(i64),
    PtsDts(i64, i64),
}

/// A PES header ready to write, everything already decided.
#[derive(Debug, Clone, Copy)]
pub struct PesHeaderOut {
    pub stream_id: u8,
    pub timestamps: PesTimestamps,
    /// `data_alignment_indicator`: set for every stream this muxer writes —
    /// each PES packet here always starts on an access-unit boundary.
    pub data_alignment: bool,
    /// `PES_packet_length`. `None` means write `0`, the "unbounded, ends at
    /// the next PES packet on this PID" convention §2.4.3.7 permits and every
    /// video stream in practice uses (measured: `-omit_video_pes_length`
    /// defaults to `true`). A non-video stream's length is always known and
    /// short enough to state, so this muxer states it.
    pub packet_length: Option<u16>,
}

/// The fixed six-byte prefix plus the optional header, **not including the
/// payload**: `write_packet` appends payload bytes itself, so the caller
/// controls whether that copies or streams.
#[must_use]
pub fn encode_pes_header(header: &PesHeaderOut) -> Vec<u8> {
    let mut flags1 = 0x80u8; // '10', marker bits fixed by the syntax.
    if header.data_alignment {
        flags1 |= 0x04;
    }
    let mut flags2 = 0u8;
    let mut optional = Vec::new();
    match header.timestamps {
        PesTimestamps::None => {}
        PesTimestamps::PtsOnly(pts) => {
            flags2 |= 0x80;
            optional.extend_from_slice(&encode_timestamp(0b0010, pts));
        }
        PesTimestamps::PtsDts(pts, dts) => {
            flags2 |= 0xC0;
            optional.extend_from_slice(&encode_timestamp(0b0011, pts));
            optional.extend_from_slice(&encode_timestamp(0b0001, dts));
        }
    }
    let mut out = vec![0x00, 0x00, 0x01, header.stream_id];
    let payload_len = header.packet_length.unwrap_or(0);
    out.extend_from_slice(&payload_len.to_be_bytes());
    out.push(flags1);
    out.push(flags2);
    // `optional.len()` is 0, 5 or 10: always representable in one byte.
    out.push(optional.len() as u8);
    out.extend_from_slice(&optional);
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_demux_mpegts::pes::{PesHeader, decode_timestamp};

    fn full_pes(header: &PesHeaderOut, payload: &[u8]) -> Vec<u8> {
        let mut v = encode_pes_header(header);
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn pts_only_reads_back_through_the_sibling_demuxers_parser() {
        let h = PesHeaderOut {
            stream_id: SID_VIDEO,
            timestamps: PesTimestamps::PtsOnly(126_000),
            data_alignment: true,
            packet_length: None,
        };
        let buf = full_pes(&h, b"payload");
        let parsed = PesHeader::parse(&buf).unwrap();
        assert_eq!(parsed.stream_id, SID_VIDEO);
        assert_eq!(parsed.pts.ticks(), Some(126_000));
        assert!(parsed.dts.is_none());
        assert!(parsed.data_alignment);
        assert_eq!(parsed.payload(&buf), b"payload");
        assert_eq!(parsed.total_len(), None);
    }

    #[test]
    fn pts_and_dts_read_back_together() {
        let h = PesHeaderOut {
            stream_id: SID_AUDIO,
            timestamps: PesTimestamps::PtsDts(9_000, 5_400),
            data_alignment: false,
            packet_length: Some(20),
        };
        let buf = full_pes(&h, b"x");
        let parsed = PesHeader::parse(&buf).unwrap();
        assert_eq!(parsed.pts.ticks(), Some(9_000));
        assert_eq!(parsed.dts.ticks(), Some(5_400));
        assert!(!parsed.data_alignment);
        assert_eq!(parsed.total_len(), Some(26));
    }

    #[test]
    fn no_timestamps_at_all_is_legal() {
        let h = PesHeaderOut {
            stream_id: SID_PRIVATE_1,
            timestamps: PesTimestamps::None,
            data_alignment: false,
            packet_length: Some(3),
        };
        let buf = full_pes(&h, b"y");
        let parsed = PesHeader::parse(&buf).unwrap();
        assert!(parsed.pts.is_none());
        assert!(parsed.dts.is_none());
        assert_eq!(parsed.payload(&buf), b"y");
    }

    /// The property the brief calls out directly: any 33-bit-representable
    /// tick count, written here, must decode to the same value through the
    /// sibling demuxer's own parser — not a second transcription of the same
    /// bit layout, an independently written one.
    #[test]
    fn the_full_thirty_three_bit_range_round_trips_through_the_demuxer() {
        for v in [
            0i64,
            1,
            2,
            90_000,
            (1i64 << 32) - 1,
            1i64 << 32,
            (1i64 << 33) - 1,
            (1i64 << 33) - 2,
        ] {
            let field = encode_timestamp(0b0010, v);
            assert_eq!(decode_timestamp(&field), Some(v), "value {v}");
        }
    }

    proptest::proptest! {
        #[test]
        fn any_thirty_three_bit_timestamp_round_trips(v in 0i64..(1i64 << 33)) {
            let field = encode_timestamp(0b0010, v);
            proptest::prop_assert_eq!(decode_timestamp(&field), Some(v));
        }

        #[test]
        fn a_full_pes_header_round_trips_pts_and_dts(
            pts in 0i64..(1i64 << 33),
            dts in 0i64..(1i64 << 33),
            len in 0u16..40_000,
        ) {
            let h = PesHeaderOut {
                stream_id: SID_VIDEO,
                timestamps: PesTimestamps::PtsDts(pts, dts),
                data_alignment: true,
                packet_length: Some(len),
            };
            let buf = full_pes(&h, b"payload-bytes");
            let parsed = PesHeader::parse(&buf).unwrap();
            proptest::prop_assert_eq!(parsed.pts.ticks(), Some(pts));
            proptest::prop_assert_eq!(parsed.dts.ticks(), Some(dts));
            proptest::prop_assert_eq!(parsed.payload(&buf), b"payload-bytes");
        }
    }
}
