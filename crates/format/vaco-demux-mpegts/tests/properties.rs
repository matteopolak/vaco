//! Properties of the PES layer and of demuxing as a whole.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::cast_possible_wrap,
    clippy::panic,
    reason = "test code"
)]

use proptest::prelude::*;
use vaco_demux_mpegts::MpegTsDemuxer;
use vaco_demux_mpegts::pes::{PesHeader, decode_timestamp, has_optional_header};
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::{Demuxer, FormatOptions};
use vaco_format_mpegts_tables::packet::{PacketStride, SYNC_BYTE, TsPacket};
use vaco_io::MemorySource;

/// Encode a 33-bit timestamp into the five-byte PES field.
fn encode_ts(prefix: u8, v: i64) -> [u8; 5] {
    let v = v as u64;
    [
        (prefix << 4) | ((((v >> 30) as u8) & 0x07) << 1) | 1,
        ((v >> 22) & 0xFF) as u8,
        ((((v >> 15) & 0x7F) as u8) << 1) | 1,
        ((v >> 7) & 0xFF) as u8,
        (((v & 0x7F) as u8) << 1) | 1,
    ]
}

proptest! {
    /// The 33-bit field survives the marker bits it is interleaved with.
    ///
    /// The encoding scatters the value across three runs separated by marker
    /// bits; a shift that is off by one is invisible for small values and
    /// wrong for large ones, which is exactly the bug that only shows up
    /// twenty-six hours into a recording.
    #[test]
    fn a_timestamp_round_trips_across_the_whole_range(v in 0i64..(1i64 << 33)) {
        let f = encode_ts(0b0010, v);
        prop_assert_eq!(decode_timestamp(&f), Some(v));
    }

    /// The marker bits are deliberately *not* validated, so clearing them
    /// must not change the decoded value. Real muxers get them wrong and the
    /// reference reads the timestamp anyway.
    #[test]
    fn marker_bits_do_not_affect_the_value(v in 0i64..(1i64 << 33)) {
        let mut f = encode_ts(0b0010, v);
        f[0] &= !0x01;
        f[2] &= !0x01;
        f[4] &= !0x01;
        prop_assert_eq!(decode_timestamp(&f), Some(v));
    }

    /// A PES header's payload never escapes the buffer it was parsed from,
    /// whatever the declared header length says. This is the one
    /// attacker-controlled offset in the packet layer.
    #[test]
    fn a_pes_payload_stays_inside_its_buffer(
        stream_id in any::<u8>(),
        header_len in any::<u8>(),
        flags in any::<u8>(),
        tail in proptest::collection::vec(any::<u8>(), 0..60),
    ) {
        let mut buf = vec![0x00, 0x00, 0x01, stream_id, 0x00, 0x00, 0x80, flags, header_len];
        buf.extend_from_slice(&tail);
        if let Some(h) = PesHeader::parse(&buf) {
            prop_assert!(h.payload_offset <= buf.len());
            prop_assert!(h.payload(&buf).len() <= buf.len());
            if !has_optional_header(stream_id) {
                prop_assert_eq!(h.payload_offset, 6);
            }
        }
    }

    /// Every 188-byte window is either rejected for its sync byte or yields a
    /// packet whose payload lies inside it.
    #[test]
    fn a_transport_packet_never_reports_a_payload_it_does_not_have(
        body in proptest::collection::vec(any::<u8>(), 187..188),
        pusi in any::<bool>(),
    ) {
        let mut buf = vec![SYNC_BYTE];
        buf.extend_from_slice(&body);
        buf.resize(188, 0);
        buf[1] = (buf[1] & !0x40) | if pusi { 0x40 } else { 0 };
        let pkt = TsPacket::parse(&buf).unwrap();
        prop_assert!(pkt.payload.len() <= 184);
        if let Some(af) = pkt.adaptation {
            prop_assert!(af.total_len + pkt.payload.len() <= 184);
        }
        prop_assert_eq!(pkt.header.payload_unit_start, pusi);
    }

    /// Opening arbitrary bytes never panics and never invents a stream.
    ///
    /// The bytes are shaped to look like a transport stream — a sync byte
    /// every 188 — because pure noise is rejected in the first kilobyte and
    /// never reaches the interesting code.
    #[test]
    fn arbitrary_ts_shaped_bytes_open_or_fail_cleanly(
        packets in proptest::collection::vec(
            proptest::collection::vec(any::<u8>(), 187..188),
            12..40,
        ),
    ) {
        let mut bytes = Vec::new();
        for p in &packets {
            bytes.push(SYNC_BYTE);
            bytes.extend_from_slice(p);
        }
        let src = Box::new(MemorySource::new(bytes));
        let Ok(mut d) = MpegTsDemuxer::open(src, &NoParsers, &FormatOptions::default()) else {
            return Ok(());
        };
        prop_assert_eq!(d.stride(), PacketStride::Ts);
        let n = d.streams().len();
        for (i, s) in d.streams().iter().enumerate() {
            prop_assert_eq!(s.index as usize, i);
            prop_assert!(s.id.is_some_and(|id| (0..=0x1FFF).contains(&id)));
        }
        for p in d.programs() {
            for &i in &p.stream_indices {
                prop_assert!((i as usize) < n);
            }
        }
        let mut read = 0;
        while let Ok(p) = d.read_packet() {
            prop_assert!((p.stream_index as usize) < n);
            read += 1;
            prop_assert!(read < 10_000, "reading did not terminate");
        }
    }
}
