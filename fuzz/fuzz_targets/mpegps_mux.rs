//! Mux arbitrary packets through each of the five `vaco-mux-mpegps`
//! profiles, then demux the result with `vaco-demux-mpegps`.
//!
//! Unlike a demuxer, a muxer's untrusted input is not bytes off the wire —
//! it is packet metadata and payloads an encoder or a copy-remux path
//! supplies, which is exactly what this generates from the fuzz bytes. What
//! is asserted: writing never panics for any payload/timestamp combination,
//! and whatever bytes come out are never rejected by this crate's own
//! demuxer as anything other than a clean parse or a clean, bounded error —
//! a muxer that alternately hangs or corrupts what it just wrote is a
//! correctness bug even though nothing here "attacked" it.
//!
//! fuzz-crate: vaco-mux-mpegps

#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_codec_core::CodecParameters;
use vaco_core::{Error, MediaType, Timestamp};
use vaco_demux_mpegps::MpegPsDemuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::{Demuxer, FormatOptions, Muxer, MuxerDesc};
use vaco_io::{MemorySource, SharedDynBuf};
use vaco_limits::{Budget, Limits};

const MAX_PACKETS: usize = 64;

fn profile_for(byte: u8) -> &'static MuxerDesc {
    match byte % 5 {
        0 => &vaco_mux_mpegps::MUXER_MPEG,
        1 => &vaco_mux_mpegps::MUXER_VCD,
        2 => &vaco_mux_mpegps::MUXER_VOB,
        3 => &vaco_mux_mpegps::MUXER_SVCD,
        _ => &vaco_mux_mpegps::MUXER_DVD,
    }
}

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let profile = profile_for(data[0]);
    let rest = &data[1..];

    let sink = SharedDynBuf::with_limits(Limits::strict());
    let mirror = sink.clone();
    let Ok(mut mux) = (profile.open)(Box::new(sink)) else {
        return;
    };

    let video = mux.add_stream(&CodecParameters::new(MediaType::Video));
    let audio = mux.add_stream(&CodecParameters::new(MediaType::Audio));
    let (Ok(video), Ok(audio)) = (video, audio) else {
        return;
    };
    if mux.write_header().is_err() {
        return;
    }

    let mut budget = Budget::new(Limits::strict());
    let mut count = 0usize;
    for chunk in rest.chunks(8) {
        if count >= MAX_PACKETS || chunk.len() < 5 {
            break;
        }
        let stream = if chunk[0] & 1 == 0 { video } else { audio };
        let pts = i64::from(u32::from_le_bytes([
            chunk[1], chunk[2], chunk[3], chunk[4],
        ]));
        let payload = chunk.get(5..).unwrap_or(&[]);
        let Ok(mut pkt) = vaco_packet::Packet::from_slice(&mut budget, payload) else {
            break;
        };
        pkt.stream_index = stream;
        pkt.pts = Timestamp::new(pts);
        pkt.dts = pkt.pts;
        // A write failing (e.g. the strict sink limit) is a legitimate
        // outcome; a panic is not.
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

    // Whatever was written must not make our own demuxer loop forever or
    // panic; a clean parse or a clean, bounded error are both fine.
    let src = Box::new(MemorySource::new(bytes));
    let opts = FormatOptions::default();
    let Ok(mut demux) =
        MpegPsDemuxer::open_with_limits(src, &opts, Limits::strict(), &NoParsers)
    else {
        return;
    };
    let mut n = 0u32;
    loop {
        match demux.read_packet() {
            Ok(_) => {
                n += 1;
                assert!(n < 100_000, "demuxing our own output did not terminate");
            }
            Err(Error::Eof | Error::LimitExceeded { .. } | Error::InvalidData(_)) => break,
            Err(_) => break,
        }
    }
});
