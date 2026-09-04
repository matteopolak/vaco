//! Mux, then read back with the sibling demuxer.
//!
//! This is the strongest test either Ogg crate has: not "did we write bytes
//! that look right" but "does `vaco-demux-ogg`, developed independently
//! against RFC 3533 and measured `ffprobe` behaviour, agree with what this
//! crate wrote". Uses `vaco_format_core::vacoraw::MemorySink` — a seekable
//! in-memory `MediaSink` that already exists for exactly this kind of test
//! (see its own docs for why it lives there rather than in `vaco-io`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code"
)]

use vaco_codec_core::{AudioParameters, CodecId, CodecParameters};
use vaco_core::{Duration, Timestamp};
use vaco_demux_ogg::OggDemuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::vacoraw::MemorySink;
use vaco_format_core::{Demuxer, FormatOptions, Muxer};
use vaco_io::MemorySource;
use vaco_limits::Budget;
use vaco_mux_ogg::OggMuxer;
use vaco_packet::{Packet, PacketFlags};

fn opus_packet(budget: &mut Budget, stream: u32, pts: i64, dur: i64, payload: &[u8]) -> Packet {
    let mut pkt = Packet::from_slice(budget, payload).unwrap();
    pkt.stream_index = stream;
    pkt.pts = Timestamp::new(pts);
    pkt.dts = pkt.pts;
    // Duration ticks are 1/48000 here, matching the stream time base this
    // muxer declares for Opus.
    pkt.duration = Duration::from_micros(dur * 1_000_000 / 48_000);
    pkt.flags = PacketFlags::KEY;
    pkt
}

#[test]
fn opus_round_trips_through_the_sibling_demuxer() {
    let sink = Box::new(MemorySink::new());
    let bytes_handle = sink.shared();
    let mut mux = OggMuxer::new(sink).unwrap();

    let mut params = CodecParameters::new(vaco_core::MediaType::Audio).with_codec(CodecId::Opus);
    params.extradata = Some(vaco_format_fixtures::opus::HEAD_MONO.to_vec());
    params.audio = Some(AudioParameters {
        sample_rate: 48_000,
        initial_padding: 312,
        ..AudioParameters::default()
    });
    let idx = mux.add_stream(&params).unwrap();
    mux.write_header().unwrap();

    let mut budget = Budget::new(vaco_limits::Limits::permissive());
    // A 20 ms (960-sample) TOC-only Opus packet: config 31 (fullband CELT,
    // 20 ms), mono, code 0 — one frame, no further framing bytes needed.
    let payload = [0xFCu8, 0x01, 0x02, 0x03];
    let mut pts = -312i64;
    for _ in 0..30 {
        let pkt = opus_packet(&mut budget, idx, pts, 960, &payload);
        mux.write_packet(&pkt).unwrap();
        pts += 960;
    }
    mux.write_trailer().unwrap();

    let written = bytes_handle.snapshot();
    assert!(!written.is_empty());
    assert_eq!(&written[0..4], b"OggS");

    let mut demux = OggDemuxer::open(
        Box::new(MemorySource::new(written)),
        &NoParsers,
        &FormatOptions::default(),
    )
    .expect("the sibling demuxer must accept what this crate wrote");

    assert_eq!(demux.streams().len(), 1);
    let stream = &demux.streams()[0];
    assert_eq!(stream.params.codec_id, Some(CodecId::Opus));
    assert_eq!(
        stream.params.audio.as_ref().map(|a| a.sample_rate),
        Some(48_000)
    );

    let mut count = 0u32;
    let mut first_pts = None;
    let mut last_pts = None;
    while let Ok(p) = demux.read_packet() {
        if first_pts.is_none() {
            first_pts = Some(p.pts.ticks());
        }
        last_pts = Some(p.pts.ticks());
        assert_eq!(
            p.payload(),
            payload,
            "packet {count} payload must survive intact"
        );
        count += 1;
    }
    assert_eq!(count, 30, "every packet written must be read back");
    assert_eq!(first_pts, Some(Some(-312)));
    assert_eq!(last_pts, Some(Some(-312 + 29 * 960)));
}

/// A minimal, valid-enough Vorbis identification packet: type `0x01` +
/// `"vorbis"` + a 4-byte zero version + 1-byte channel count + a 4-byte LE
/// sample rate + three 4-byte-each zero bitrate fields + a blocksize byte
/// (`blocksize_0` exponent in the low nibble, `blocksize_1` in the high
/// one) + a framing bit — 30 bytes, the same length `codec::parse_vorbis_ident`
/// expects at minimum and a real `ffmpeg -c:a vorbis` encode also produces.
fn vorbis_ident(channels: u8, sample_rate: u32, blocksize_0_exp: u8) -> Vec<u8> {
    let mut h = vec![0x01];
    h.extend_from_slice(b"vorbis");
    h.extend_from_slice(&0u32.to_le_bytes()); // vorbis_version
    h.push(channels);
    h.extend_from_slice(&sample_rate.to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes()); // bitrate_maximum
    h.extend_from_slice(&0u32.to_le_bytes()); // bitrate_nominal
    h.extend_from_slice(&0u32.to_le_bytes()); // bitrate_minimum
    h.push(blocksize_0_exp | (blocksize_0_exp << 4)); // blocksize_0 == blocksize_1
    h.push(0x01); // framing bit
    h
}

/// A minimal, valid Vorbis comment header: type `0x03` + `"vorbis"` + a
/// zero-length vendor string + zero user comments + the framing bit.
fn vorbis_comment() -> Vec<u8> {
    let mut h = vec![0x03];
    h.extend_from_slice(b"vorbis");
    h.extend_from_slice(&0u32.to_le_bytes()); // vendor length
    h.extend_from_slice(&0u32.to_le_bytes()); // user comment count
    h.push(0x01); // framing bit
    h
}

#[test]
fn vorbis_round_trips_through_the_sibling_demuxer() {
    let sink = Box::new(MemorySink::new());
    let bytes_handle = sink.shared();
    let mut mux = OggMuxer::new(sink).unwrap();

    let ident = vorbis_ident(1, 44_100, 10); // blocksize 2^10 = 1024
    let comment = vorbis_comment();
    // The setup header is opaque codebooks to both crates -- any bytes
    // round-trip identically, which is the property this test checks.
    let setup = vec![0x05u8, b'v', b'o', b'r', b'b', b'i', b's', 0xAB, 0xCD, 0xEF];
    let extradata =
        vaco_demux_ogg::codec::pack_xiph_headers(&[ident.clone(), comment.clone(), setup.clone()]);

    let mut params = CodecParameters::new(vaco_core::MediaType::Audio).with_codec(CodecId::Vorbis);
    params.extradata = Some(extradata);
    params.audio = Some(AudioParameters {
        sample_rate: 44_100,
        ..AudioParameters::default()
    });
    let idx = mux.add_stream(&params).unwrap();
    mux.write_header().unwrap();

    let mut budget = Budget::new(vaco_limits::Limits::permissive());
    let payload = [0x11u8, 0x22, 0x33];
    for i in 0..5u32 {
        let mut pkt = Packet::from_slice(&mut budget, &payload).unwrap();
        pkt.stream_index = idx;
        pkt.pts = Timestamp::new(i64::from(i) * 1024);
        pkt.dts = pkt.pts;
        pkt.duration = Duration::from_micros(1024 * 1_000_000 / 44_100);
        pkt.flags = PacketFlags::KEY;
        mux.write_packet(&pkt).unwrap();
    }
    mux.write_trailer().unwrap();

    let written = bytes_handle.snapshot();
    let mut demux = OggDemuxer::open(
        Box::new(MemorySource::new(written)),
        &NoParsers,
        &FormatOptions::default(),
    )
    .expect("the sibling demuxer must accept what this crate wrote");

    assert_eq!(demux.streams().len(), 1);
    let stream = &demux.streams()[0];
    assert_eq!(stream.params.codec_id, Some(CodecId::Vorbis));
    assert_eq!(
        stream.params.audio.as_ref().map(|a| a.sample_rate),
        Some(44_100)
    );
    // The three original header packets must come back exactly, not just
    // the identification one -- the whole point of packing all three.
    let unpacked = vaco_demux_ogg::codec::split_xiph_headers(
        stream.params.extradata.as_ref().expect("extradata"),
    )
    .expect("packed extradata");
    assert_eq!(unpacked, vec![ident, comment, setup]);

    let mut count = 0u32;
    while let Ok(p) = demux.read_packet() {
        assert_eq!(p.payload(), payload);
        count += 1;
    }
    assert_eq!(count, 5, "every packet written must be read back");
}

#[test]
fn vorbis_is_refused_without_three_packed_headers() {
    let sink = Box::new(MemorySink::new());
    let mut mux = OggMuxer::new(sink).unwrap();
    let mut params = CodecParameters::new(vaco_core::MediaType::Audio).with_codec(CodecId::Vorbis);
    // The pre-fix shape: just the identification packet, not Xiph-packed.
    params.extradata = Some(vorbis_ident(1, 44_100, 10));
    params.audio = Some(AudioParameters {
        sample_rate: 44_100,
        ..AudioParameters::default()
    });
    assert!(
        mux.add_stream(&params).is_err(),
        "a lone identification packet must not be silently treated as three packed headers"
    );
}

#[test]
fn flac_round_trips_through_the_sibling_demuxer() {
    let sink = Box::new(MemorySink::new());
    let bytes_handle = sink.shared();
    let mut mux = OggMuxer::new(sink).unwrap();

    // A minimal STREAMINFO: 44100 Hz, mono, 16 bits — the packed region is
    // the only part this demuxer's parser reads; the fixed block/frame-size
    // fields ahead of it can stay zero for this test.
    let mut streaminfo = vec![0u8; 34];
    let packed: u64 = (44_100u64 << 44) | (15u64 << 36);
    streaminfo[10..18].copy_from_slice(&packed.to_be_bytes());

    let mut params = CodecParameters::new(vaco_core::MediaType::Audio).with_codec(CodecId::Flac);
    params.extradata = Some(streaminfo);
    params.audio = Some(AudioParameters {
        sample_rate: 44_100,
        ..AudioParameters::default()
    });
    let idx = mux.add_stream(&params).unwrap();
    mux.write_header().unwrap();

    let mut budget = Budget::new(vaco_limits::Limits::permissive());
    let payload = [0xFFu8, 0xF8, 0x69, 0x18, 0x00, 0x00];
    for i in 0..5u32 {
        let mut pkt = Packet::from_slice(&mut budget, &payload).unwrap();
        pkt.stream_index = idx;
        pkt.pts = Timestamp::new(i64::from(i) * 4608);
        pkt.dts = pkt.pts;
        pkt.duration = Duration::from_micros(4608 * 1_000_000 / 44_100);
        pkt.flags = PacketFlags::KEY;
        mux.write_packet(&pkt).unwrap();
    }
    mux.write_trailer().unwrap();

    let written = bytes_handle.snapshot();
    let mut demux = OggDemuxer::open(
        Box::new(MemorySource::new(written)),
        &NoParsers,
        &FormatOptions::default(),
    )
    .expect("the sibling demuxer must accept what this crate wrote");

    assert_eq!(demux.streams().len(), 1);
    let stream = &demux.streams()[0];
    assert_eq!(stream.params.codec_id, Some(CodecId::Flac));
    assert_eq!(
        stream.params.audio.as_ref().map(|a| a.sample_rate),
        Some(44_100)
    );

    let mut count = 0u32;
    while let Ok(p) = demux.read_packet() {
        assert_eq!(p.payload(), payload);
        count += 1;
    }
    assert_eq!(count, 5);
}

/// A packet with no stated duration (the ordinary `-c copy` case out of a
/// demuxer that reports none) must not contribute zero to `granule_cursor`
/// — the file's only seekable timeline. `write_packet` used to accumulate
/// `packet.duration.to_ticks(...).unwrap_or(0)` verbatim, so a duration-less
/// packet's own span vanished from every page granule after it, worst on
/// the very last packet, where it left the whole file one frame short.
///
/// FLAC's granule is a plain, unscaled sample count (measured, see
/// `vaco-demux-ogg::granule`'s module docs) with no bitstream-derived
/// self-correction the way Opus has, so this is checked directly against
/// `Stream::duration_ts` — the final page's own raw granule value — rather
/// than through any codec-specific reconstruction.
#[test]
fn a_packet_with_no_stated_duration_still_advances_the_granule() {
    const FRAME_SAMPLES: i64 = 4608;
    const COUNT: i64 = 5;

    let sink = Box::new(MemorySink::new());
    let bytes_handle = sink.shared();
    let mut mux = OggMuxer::new(sink).unwrap();

    let mut streaminfo = vec![0u8; 34];
    let packed: u64 = (44_100u64 << 44) | (15u64 << 36);
    streaminfo[10..18].copy_from_slice(&packed.to_be_bytes());

    let mut params = CodecParameters::new(vaco_core::MediaType::Audio).with_codec(CodecId::Flac);
    params.extradata = Some(streaminfo);
    params.audio = Some(AudioParameters {
        sample_rate: 44_100,
        ..AudioParameters::default()
    });
    let idx = mux.add_stream(&params).unwrap();
    mux.write_header().unwrap();

    let mut budget = Budget::new(vaco_limits::Limits::permissive());
    let payload = [0xFFu8, 0xF8, 0x69, 0x18, 0x00, 0x00];
    for i in 0..COUNT {
        let mut pkt = Packet::from_slice(&mut budget, &payload).unwrap();
        pkt.stream_index = idx;
        pkt.pts = Timestamp::new(i * FRAME_SAMPLES);
        pkt.dts = pkt.pts;
        // Every packet but the last states a real duration; the last one
        // (the shape `91c15a2b` fixed in vaco-mux-mp4) states none.
        pkt.duration = if i < COUNT - 1 {
            Duration::from_micros(FRAME_SAMPLES * 1_000_000 / 44_100)
        } else {
            Duration::ZERO
        };
        pkt.flags = PacketFlags::KEY;
        mux.write_packet(&pkt).unwrap();
    }
    mux.write_trailer().unwrap();

    let written = bytes_handle.snapshot();
    let demux = OggDemuxer::open(
        Box::new(MemorySource::new(written)),
        &NoParsers,
        &FormatOptions::default(),
    )
    .expect("the sibling demuxer must accept what this crate wrote");

    // Five real frames' worth of samples, including the unstated last one
    // (falling back to the previous frame's own duration), not four.
    assert_eq!(
        demux.streams()[0].duration_ts,
        Some(COUNT * FRAME_SAMPLES),
        "the unstated last packet's duration must fall back to the previous \
         frame's, not drop out of the granule entirely"
    );
}
