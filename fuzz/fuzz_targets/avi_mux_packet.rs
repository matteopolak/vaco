//! `AviMuxer::write_packet` over an arbitrary H.264 access unit.
//!
//! Every packet payload is written verbatim, whichever framing its stream
//! declared (see `vaco-mux-avi::mux::StreamOut::config_record`'s doc
//! comment for the measurement behind that) — so the untrusted-input surface
//! this target actually exercises is the 600 Hz slot grid's empty-chunk
//! backfill, which runs unconditionally, and the `strf` config-record write
//! that a length-prefixed stream's `add_stream` call triggers once.
//!
//! Properties asserted beyond "does not panic":
//!
//! * **Output growth is bounded.** A packet is written byte for byte, so the
//!   output cannot grow faster than the payload plus a small fixed overhead.
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

/// Stands in for a real `AVCDecoderConfigurationRecord`. This crate never
/// looks inside `extradata` — it writes it into `strf` verbatim — so the
/// bytes' own structure is not this target's concern (that is
/// `vaco-parse-h264`'s and, for the container framing, `avi_demux`'s); this
/// exists only so a length-prefixed stream has *something* non-empty to
/// satisfy `add_stream`'s "needs its avcC/hvcC extradata" check, so the
/// `strf`/`config_record` write path actually runs.
const FAKE_EXTRADATA: [u8; 4] = [1, 0x64, 0, 0x0A];

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
        extradata: length_prefixed.then(|| FAKE_EXTRADATA.to_vec()),
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
        // The RIFF/hdrl header — dominated by the three fixed `JUNK`
        // reservations (4120 + 260 + 1016 bytes, plus their own tag/size
        // headers) `write_header`/`write_strl` always write regardless of
        // payload — plus `strf`'s own `video_extradata` write, `idx1`, the
        // payload written verbatim plus a small fixed per-chunk overhead,
        // and one 8-byte placeholder chunk plus one 16-byte index entry per
        // grid slot this packet's own `pts` skipped.
        let header_budget = 8192;
        let grid_budget = usize::from(pts_ticks).saturating_mul(8 + 16);
        let payload_budget = payload
            .len()
            .saturating_add(16)
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
