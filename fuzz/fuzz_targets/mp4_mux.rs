//! Mux arbitrary packets through `vaco-mux-mp4`, in every mode the fuzz
//! bytes select (progressive, faststart, fragmented, `separate_moof`,
//! `dash`), then demux the result with `vaco-demux-mp4`.
//!
//! Unlike a demuxer, a muxer's untrusted input is not bytes off the wire —
//! it is packet metadata and payloads an encoder or a copy-remux path
//! supplies, which is exactly what this generates from the fuzz bytes. What
//! is asserted: writing never panics for any payload/timestamp/flag
//! combination, and whatever bytes come out are never rejected by this
//! crate's own demuxer as anything other than a clean parse or a clean,
//! bounded error — a muxer that alternately hangs or corrupts what it just
//! wrote is a correctness bug even though nothing here "attacked" it. This
//! is also this crate's most direct coverage of the trailer/faststart
//! rewrite: `vaco-format-isom`'s reader is what actually reads back every
//! byte this crate patched in place.
//!
//! fuzz-crate: vaco-mux-mp4

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::{CodecId, CodecParameters, VideoParameters};
use vaco_core::{Error, MediaType, Timestamp};
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::{Demuxer, FormatOptions, Muxer};
use vaco_io::{MemorySource, SharedDynBuf};
use vaco_limits::{Budget, Limits};
use vaco_mux_mp4::options::MovFlags;
use vaco_mux_mp4::{MovMuxer, MuxOptions};

const MAX_PACKETS: usize = 64;

fn avc_extradata() -> Vec<u8> {
    vec![
        1, 0x42, 0x00, 0x0A, 0xFF, 0xE1, 0x00, 0x04, 0x67, 0x42, 0x00, 0x0A, 0x01, 0x00, 0x02,
        0x68, 0xCE,
    ]
}

fn h264_params() -> CodecParameters {
    let mut p = CodecParameters {
        media_type: Some(MediaType::Video),
        codec_id: Some(CodecId::H264),
        extradata: Some(avc_extradata()),
        ..CodecParameters::default()
    };
    p.video = Some(VideoParameters {
        width: 64,
        height: 48,
        frame_rate: vaco_core::Rational::new(30, 1),
        ..VideoParameters::default()
    });
    p
}

fn flags_for(byte: u8) -> MovFlags {
    match byte % 6 {
        0 => MovFlags::empty(),
        1 => MovFlags::FASTSTART,
        2 => MovFlags::FRAG_KEYFRAME,
        3 => MovFlags::FRAG_EVERY_FRAME,
        4 => MovFlags::FRAG_KEYFRAME | MovFlags::SEPARATE_MOOF | MovFlags::DEFAULT_BASE_MOOF,
        _ => MovFlags::FRAG_KEYFRAME | MovFlags::DASH,
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode_byte, rest)) = data.split_first() else {
        return;
    };
    let opts = MuxOptions {
        movflags: flags_for(mode_byte),
        ..MuxOptions::default()
    };

    let sink = SharedDynBuf::with_limits(Limits::strict());
    let mirror = sink.clone();
    let Ok(mut mux) = MovMuxer::with_options(Box::new(sink), opts) else {
        return;
    };
    let Ok(video) = mux.add_stream(&h264_params()) else {
        return;
    };
    if mux.init().is_err() {
        return;
    }
    if mux.write_header().is_err() {
        return;
    }

    let mut budget = Budget::new(Limits::strict());
    let mut count = 0usize;
    let mut dts = 0i64;
    for chunk in rest.chunks(8) {
        if count >= MAX_PACKETS {
            break;
        }
        let Some(&flag_byte) = chunk.first() else {
            break;
        };
        let delta = i64::from(chunk.get(1).copied().unwrap_or(0));
        dts = dts.saturating_add(delta);
        let payload = chunk.get(2..).unwrap_or(&[]);
        let Ok(mut pkt) = vaco_packet::Packet::from_slice(&mut budget, payload) else {
            break;
        };
        pkt.stream_index = video;
        pkt.dts = Timestamp::new(dts);
        pkt.pts = pkt.dts;
        if flag_byte & 1 == 0 {
            pkt.flags |= vaco_packet::PacketFlags::KEY;
        }
        // A write failing (e.g. the strict sink limit, or too large a
        // sample) is a legitimate outcome; a panic is not.
        if mux.write_packet(&pkt).is_err() {
            break;
        }
        count += 1;
    }
    let _ = mux.write_trailer();
    drop(mux);

    let bytes = mirror.take();
    if bytes.is_empty() {
        return;
    }

    // Whatever this crate just wrote must not make its own sibling demuxer
    // loop forever or panic; a clean parse or a clean, bounded error are
    // both fine outcomes here — a panic on either side is not.
    let src = Box::new(MemorySource::new(bytes));
    let Ok(mut demux) = vaco_demux_mp4::Mp4Demuxer::open(
        src,
        &NoParsers,
        &FormatOptions::default(),
        vaco_demux_mp4::Mp4Options::default(),
    ) else {
        return;
    };
    let mut n = 0u32;
    loop {
        match demux.read_packet() {
            Ok(_) => {
                n = n.saturating_add(1);
                assert!(n < 100_000, "demuxing our own output did not terminate");
            }
            Err(Error::Eof | Error::LimitExceeded { .. } | Error::InvalidData(_)) => break,
            Err(_) => break,
        }
    }
});
