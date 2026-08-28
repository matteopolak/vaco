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
//! * **A video timestamp does not blow past the bound its own gap implies.**
//!   Every packet lands on the 600 Hz slot grid, backfilling every unused
//!   slot since the last real one with a placeholder chunk — an
//!   attacker-controlled `pts` gap, capped here so one fuzz iteration cannot
//!   spend its time writing an unbounded number of them.
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

/// The largest grid gap one fuzz iteration is allowed to ask for. Wide
/// enough to exercise the backfill loop and its index growth repeatedly;
/// capped so a single iteration cannot spend its time writing an unbounded
/// run of placeholder chunks. The budget rejection path for a gap past what
/// any real recording needs is covered by a unit test instead, where an
/// exact input is worth more than a slow corpus entry.
const MAX_GRID_GAP: u16 = 4096;

fuzz_target!(|data: &[u8]| {
    let Some((&flags_byte, rest)) = data.split_first() else {
        return;
    };
    let Some((&pts_lo, rest)) = rest.split_first() else {
        return;
    };
    let Some((&pts_hi, rest)) = rest.split_first() else {
        return;
    };
    let pts_ticks = u16::from_le_bytes([pts_lo, pts_hi]) % MAX_GRID_GAP;
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
    pkt.pts = Timestamp::new(i64::from(pts_ticks));
    pkt.dts = Timestamp::new(i64::from(pts_ticks));
    if flags_byte & 0x04 != 0 {
        pkt.flags |= PacketFlags::KEY;
    }

    let write_ok = mux.write_packet(&pkt).is_ok();
    let _ = mux.write_trailer();
    drop(mux);

    let bytes = mirror.take();

    if write_ok {
        // The RIFF/hdrl header, `idx1`, a bounded multiple of the payload,
        // and one 8-byte placeholder chunk plus one 16-byte index entry per
        // grid slot this packet's own `pts` skipped.
        let header_budget = 4096;
        let grid_budget = usize::from(pts_ticks).saturating_mul(8 + 16);
        let payload_budget = payload
            .len()
            .saturating_mul(MAX_GROWTH_NUMERATOR)
            .div_ceil(MAX_GROWTH_DENOMINATOR)
            .saturating_add(header_budget)
            .saturating_add(grid_budget);
        assert!(
            bytes.len() <= payload_budget,
            "output {} bytes exceeds the amplification bound {payload_budget} for a {}-byte payload with a {pts_ticks}-slot gap",
            bytes.len(),
            payload.len(),
        );
    }
});
