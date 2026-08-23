//! `MpegTsMuxer::write_packet` over an arbitrary H.264 access unit.
//!
//! The muxer's only real untrusted-input surface: everything else it does is
//! driven by caller-chosen options (PIDs, flags, service names), but
//! `write_packet`'s payload is exactly what an untrusted, possibly malformed
//! upstream file's elementary stream looks like once it reaches a remux
//! pipeline. With `nal_length_size` set, every packet also exercises
//! [`vaco_format_nalu::convert::length_prefixed_to_annexb`]'s NAL-unit walk
//! over the fuzzer's bytes before a single byte reaches the transport
//! packetiser — the length-prefix parsing this crate depends on but does not
//! own.
//!
//! Properties asserted beyond "does not panic":
//!
//! * **Output is always a whole number of 188-byte transport packets.** A
//!   malformed length prefix must never leave a partial cell on the wire.
//! * **Output growth is bounded.** The Annex-B conversion can grow the
//!   payload by at most a handful of bytes per NAL unit; this asserts a
//!   generous linear bound rather than trusting that by construction, since
//!   that bound is exactly what an amplification bug would blow through.
//!
//! fuzz-crate: vaco-mux-mpegts

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::{CodecId, CodecParameters, VideoParameters};
use vaco_core::{MediaType, Timestamp};
use vaco_format_core::Muxer;
use vaco_io::{MediaSink, SharedDynBuf};
use vaco_limits::{Budget, Limits};
use vaco_mux_mpegts::mux::MpegTsMuxer;
use vaco_packet::{Packet, PacketFlags};

/// Caps how much of the fuzzer's input becomes one packet's payload, so a
/// huge input spends fuzzing time on many small mutations rather than one
/// slow giant allocation.
const MAX_PAYLOAD: usize = 16_384;

/// Worst-case bytes the Annex-B conversion can add per byte of input: a
/// pathological one-byte-per-NAL length prefix turns every five input bytes
/// (4-byte length + 1 byte of unit) into a nine-byte Annex-B unit (4-byte
/// start code + 1 byte). Generous on purpose — this is a sanity bound on
/// amplification, not a tight one.
const MAX_GROWTH_NUMERATOR: usize = 9;
const MAX_GROWTH_DENOMINATOR: usize = 5;

fuzz_target!(|data: &[u8]| {
    let Some((&flags_byte, rest)) = data.split_first() else {
        return;
    };
    let payload = rest.get(..rest.len().min(MAX_PAYLOAD)).unwrap_or(rest);

    let sink = SharedDynBuf::new();
    let mirror = sink.clone();
    let mut mux = MpegTsMuxer::new(Box::new(sink) as Box<dyn MediaSink>);

    let length_prefixed = flags_byte & 0x01 != 0;
    let params = CodecParameters {
        media_type: Some(MediaType::Video),
        codec_id: Some(CodecId::H264),
        video: Some(VideoParameters {
            nal_length_size: length_prefixed.then_some(4),
            ..VideoParameters::default()
        }),
        ..CodecParameters::new(MediaType::Video)
    };
    let Ok(video) = mux.add_stream(&params) else {
        return;
    };
    if mux.init().is_err() || mux.write_header().is_err() {
        return;
    }

    let mut budget = Budget::new(Limits::permissive());
    let Ok(mut pkt) = Packet::from_slice(&mut budget, payload) else {
        return;
    };
    pkt.stream_index = video;
    // Exercise both timestamp shapes the PES header can take.
    pkt.pts = Timestamp::new(0);
    pkt.dts = if flags_byte & 0x02 != 0 {
        Timestamp::new(0)
    } else {
        Timestamp::new(-3600)
    };
    if flags_byte & 0x04 != 0 {
        pkt.flags |= PacketFlags::KEY;
    }

    let write_ok = mux.write_packet(&pkt).is_ok();
    let _ = mux.write_trailer();
    drop(mux);

    let bytes = mirror.take();
    assert_eq!(bytes.len() % 188, 0, "output must be whole 188-byte cells");

    if write_ok {
        // The PAT/PMT/SDT header plus a bounded multiple of the payload.
        let header_budget = 20 * 188;
        let payload_budget = payload
            .len()
            .saturating_mul(MAX_GROWTH_NUMERATOR)
            .div_ceil(MAX_GROWTH_DENOMINATOR)
            .saturating_add(header_budget);
        assert!(
            bytes.len() <= payload_budget,
            "output {} bytes exceeds the amplification bound {payload_budget} for a {}-byte payload",
            bytes.len(),
            payload.len(),
        );
    }
});
