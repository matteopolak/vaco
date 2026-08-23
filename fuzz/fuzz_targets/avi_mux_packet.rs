//! `AviMuxer::write_packet` over an arbitrary H.264 access unit.
//!
//! Finding 16 (`planning/CONFORMANCE-FINDINGS.md`) gave this crate its first
//! real untrusted-input parsing surface: with `nal_length_size` set, every
//! packet now runs through
//! [`vaco_format_nalu::convert::length_prefixed_to_annexb`]'s NAL-unit walk
//! before a single byte reaches the `movi` chunk — the same conversion
//! `vaco-mux-mpegts`'s own `mpegts_mux_packet` fuzz target already exercises,
//! mirrored here because the parsing code is a dependency both crates call
//! into, not code either one owns.
//!
//! Properties asserted beyond "does not panic":
//!
//! * **Output growth is bounded.** A pathological length prefix must not let
//!   the conversion amplify the payload without limit.
//!
//! fuzz-crate: vaco-mux-avi

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::{CodecId, CodecParameters, VideoParameters};
use vaco_core::{MediaType, Timestamp};
use vaco_format_core::{FormatOptions, Muxer};
use vaco_io::{MediaSink, SharedDynBuf};
use vaco_limits::{Budget, Limits};
use vaco_mux_avi::AviMuxer;
use vaco_packet::{Packet, PacketFlags};

/// Caps how much of the fuzzer's input becomes one packet's payload, so a
/// huge input spends fuzzing time on many small mutations rather than one
/// slow giant allocation.
const MAX_PAYLOAD: usize = 16_384;

/// Worst-case bytes the Annex-B conversion can add per byte of input — see
/// `mpegts_mux_packet`'s identical constant for the arithmetic.
const MAX_GROWTH_NUMERATOR: usize = 9;
const MAX_GROWTH_DENOMINATOR: usize = 5;

fuzz_target!(|data: &[u8]| {
    let Some((&flags_byte, rest)) = data.split_first() else {
        return;
    };
    let payload = rest.get(..rest.len().min(MAX_PAYLOAD)).unwrap_or(rest);

    let sink = SharedDynBuf::new();
    let mirror = sink.clone();
    let Ok(mut mux) = AviMuxer::new(Box::new(sink) as Box<dyn MediaSink>, &FormatOptions::default())
    else {
        return;
    };

    let length_prefixed = flags_byte & 0x01 != 0;
    let params = CodecParameters {
        media_type: Some(MediaType::Video),
        codec_id: Some(CodecId::H264),
        video: Some(VideoParameters {
            nal_length_size: length_prefixed.then_some(4),
            frame_rate: vaco_core::Rational::new(25, 1),
            ..VideoParameters::default()
        }),
        ..CodecParameters::new(MediaType::Video)
    };
    let Ok(video) = mux.add_stream(&params) else {
        return;
    };
    if mux.write_header().is_err() {
        return;
    }

    let mut budget = Budget::new(Limits::permissive());
    let Ok(mut pkt) = Packet::from_slice(&mut budget, payload) else {
        return;
    };
    pkt.stream_index = video;
    pkt.pts = Timestamp::new(0);
    pkt.dts = Timestamp::new(0);
    if flags_byte & 0x04 != 0 {
        pkt.flags |= PacketFlags::KEY;
    }

    let write_ok = mux.write_packet(&pkt).is_ok();
    let _ = mux.write_trailer();
    drop(mux);

    let bytes = mirror.take();

    if write_ok {
        // The RIFF/hdrl header plus `idx1` plus a bounded multiple of the
        // payload.
        let header_budget = 4096;
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
