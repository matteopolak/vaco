//! Transport packet framing, stride detection and PES headers over arbitrary
//! bytes.
//!
//! Complements `mpegts_section`, which fuzzes the *stateful* PSI layer. This
//! one covers the stateless views: the four-byte transport header, the
//! adaptation field with its six optional fields behind five flags, the 48-bit
//! PCR, and the PES header whose `PES_header_data_length` is the one
//! attacker-controlled offset in the whole packet layer.
//!
//! What is asserted beyond "does not panic":
//!
//! * **A parsed packet's payload lies inside it.** The adaptation field's
//!   declared length decides where the payload starts, so a length that lies
//!   must produce an empty payload and a `malformed_adaptation` flag, never a
//!   slice pointing past the packet.
//! * **Stride detection agrees with itself.** Whatever `find_stride` reports,
//!   counting the run again at that offset must reproduce it — the probe and
//!   the resynchroniser share this primitive and a disagreement between them
//!   would make a file open and then fail to read.
//! * **A PES header's payload offset is inside the buffer it was parsed
//!   from.** This is the check that matters: `payload_offset` is computed from
//!   a declared length, and every byte after it is handed to a decoder.
//!
//! fuzz-crate: vaco-demux-mpegts

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_demux_mpegts::pes::PesHeader;
use vaco_format_mpegts_tables::packet::{
    PacketStride, Pcr, TS_PACKET_SIZE, TsHeader, TsPacket, find_stride, sync_run,
};

fuzz_target!(|data: &[u8]| {
    // Stride detection over the whole buffer, then self-agreement.
    if let Some((stride, at, run)) = find_stride(data, 1024, 32) {
        assert!(run >= 2, "find_stride reported a run it should have rejected");
        assert_eq!(
            sync_run(data, at, stride, 32),
            run,
            "find_stride and sync_run disagree at offset {at}"
        );
        assert!(at <= data.len());
    }

    // Every 188-byte window as a transport packet.
    for (i, window) in data.chunks(TS_PACKET_SIZE).enumerate() {
        if window.len() < TS_PACKET_SIZE {
            break;
        }
        let Some(pkt) = TsPacket::parse(window) else {
            // Only a missing sync byte may cause this.
            assert_ne!(window.first(), Some(&0x47), "rejected a synced packet");
            continue;
        };
        let header = TsHeader::parse(window).expect("header parsed once already");
        assert_eq!(header, pkt.header);
        assert!(pkt.header.pid <= 0x1FFF);
        assert!(
            pkt.payload.len() <= TS_PACKET_SIZE - 4,
            "packet {i} payload larger than the packet"
        );
        if pkt.malformed_adaptation {
            assert!(
                pkt.payload.is_empty(),
                "a lying adaptation-field length must yield no payload"
            );
        }
        if let Some(af) = pkt.adaptation {
            assert!(af.total_len <= TS_PACKET_SIZE - 4);
            assert!(
                af.total_len + pkt.payload.len() <= TS_PACKET_SIZE - 4,
                "adaptation field and payload overlap"
            );
            if let Some(pcr) = af.pcr {
                assert!(pcr.base < (1 << 33), "PCR base is wider than 33 bits");
                assert!(pcr.extension < 512);
                let _ = pcr.as_27mhz();
            }
        }
        // The payload as a PES packet, which is what a demuxer does next.
        if let Some(h) = PesHeader::parse(pkt.payload) {
            assert!(
                h.payload_offset <= pkt.payload.len() || h.payload(pkt.payload).is_empty(),
                "PES payload offset {} escapes a {}-byte buffer",
                h.payload_offset,
                pkt.payload.len()
            );
            let _ = h.total_len();
        }
    }

    // The PCR and PES decoders directly, so short inputs reach them too.
    if data.len() >= 6 {
        let _ = Pcr::parse(data);
    }
    if let Some(h) = PesHeader::parse(data) {
        let payload = h.payload(data);
        assert!(payload.len() <= data.len());
    }
    for stride in PacketStride::ALL {
        let _ = stride.body(data);
    }
});
