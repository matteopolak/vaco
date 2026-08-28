#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::missing_errors_doc,
    clippy::panic,
    clippy::cast_possible_wrap,
    reason = "test code"
)]
//! Mux one packet into each of the nine formats, then demux it back and
//! check the container round-trips: stream count, sample rate, channel
//! count, and the payload bytes themselves.
//!
//! This is the test that actually exercises every mux/demux pair end to
//! end — [`vaco_format_audio_simple::pcm`]'s own unit tests cover the shared
//! packet-framing machinery in isolation, but only this file drives each
//! format's real header writer against its own real header reader.

use vaco_chlayout::ChannelLayout;
use vaco_codec_core::{CodecId, CodecParameters};
use vaco_format_core::vacoraw::MemorySink;
use vaco_format_core::{Demuxer, Muxer};
use vaco_io::MemorySource;
use vaco_sampfmt::SampleFmt;

use vaco_format_audio_simple::{aiff, au, caf, ircam, rso, sox, voc, w64, wav};

const SAMPLE_RATE: u32 = 8000;

fn audio_params(format: SampleFmt, channels: u32) -> CodecParameters {
    let mut p = CodecParameters::audio();
    // The codec, not only the decoded sample format. These muxers need it:
    // `pcm_s24le` decodes to `s32`, and A-law decodes to `s16` while storing
    // one byte per sample, so a header written from the sample format alone
    // mislabels the width and corrupts the stream. `add_stream` now says so
    // rather than guessing, which is what these fixtures were exercising
    // before — every one of them passed while the muxers were writing files
    // the reference could not read back.
    p.codec_id = Some(match format {
        SampleFmt::U8 => CodecId::PcmU8,
        SampleFmt::S32 => CodecId::PcmS32le,
        SampleFmt::F32 => CodecId::PcmF32le,
        SampleFmt::F64 => CodecId::PcmF64le,
        // Covers `SampleFmt::S16` and every other (e.g. planar) format.
        _ => CodecId::PcmS16le,
    });
    if let Some(a) = p.audio.as_mut() {
        a.sample_rate = SAMPLE_RATE;
        a.format = Some(format);
        a.layout = ChannelLayout::default_for(channels);
    }
    p
}

/// Mux `payload` as one packet through `open_mux`, then demux the bytes
/// produced with `open_demux`, and assert the payload and basic stream
/// facts survive the round trip.
#[allow(clippy::too_many_arguments)]
fn roundtrip(
    name: &str,
    format: SampleFmt,
    channels: u32,
    payload: &[u8],
    open_mux: impl FnOnce(Box<dyn vaco_io::MediaSink>) -> vaco_core::Result<Box<dyn Muxer>>,
    open_demux: impl FnOnce(Box<dyn vaco_io::MediaSource>) -> vaco_core::Result<Box<dyn Demuxer>>,
) {
    roundtrip_with_params(
        name,
        &audio_params(format, channels),
        channels,
        payload,
        open_mux,
        open_demux,
    );
}

/// As [`roundtrip`], but the caller supplies `CodecParameters` directly
/// instead of letting [`audio_params`] pick a codec from the sample format.
/// `.au` needs this: it has no little-endian PCM encoding at all, so its
/// test payload and codec must agree on a big-endian one specifically,
/// which the shared little-endian default `audio_params` picks does not.
fn roundtrip_with_params(
    name: &str,
    params: &CodecParameters,
    channels: u32,
    payload: &[u8],
    open_mux: impl FnOnce(Box<dyn vaco_io::MediaSink>) -> vaco_core::Result<Box<dyn Muxer>>,
    open_demux: impl FnOnce(Box<dyn vaco_io::MediaSource>) -> vaco_core::Result<Box<dyn Demuxer>>,
) {
    let sink = MemorySink::new();
    let written = sink.shared();
    let mut mux = open_mux(Box::new(sink)).unwrap_or_else(|e| panic!("{name}: open muxer: {e}"));
    let idx = mux
        .add_stream(params)
        .unwrap_or_else(|e| panic!("{name}: add_stream: {e}"));
    mux.write_header()
        .unwrap_or_else(|e| panic!("{name}: write_header: {e}"));

    let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::permissive());
    let mut pkt = vaco_packet::Packet::from_slice(&mut budget, payload)
        .unwrap_or_else(|e| panic!("{name}: alloc packet: {e}"));
    pkt.stream_index = idx;
    pkt.pts = vaco_core::Timestamp::ZERO;
    pkt.dts = vaco_core::Timestamp::ZERO;
    pkt.flags = vaco_packet::PacketFlags::KEY;
    mux.write_packet(&pkt)
        .unwrap_or_else(|e| panic!("{name}: write_packet: {e}"));
    mux.write_trailer()
        .unwrap_or_else(|e| panic!("{name}: write_trailer: {e}"));

    let bytes = written.snapshot();
    assert!(!bytes.is_empty(), "{name}: muxer produced no bytes");

    let src = Box::new(MemorySource::new(bytes));
    let mut demux = open_demux(src).unwrap_or_else(|e| panic!("{name}: open demuxer: {e}"));
    assert_eq!(
        demux.streams().len(),
        1,
        "{name}: expected exactly one stream"
    );
    let audio = demux.streams()[0]
        .params
        .audio
        .as_ref()
        .unwrap_or_else(|| panic!("{name}: stream is not audio"));
    assert_eq!(audio.sample_rate, SAMPLE_RATE, "{name}: sample rate");
    assert_eq!(
        audio.layout.as_ref().map(|l| l.channels),
        Some(channels),
        "{name}: channel count"
    );

    let mut got = Vec::new();
    loop {
        match demux.read_packet() {
            Ok(p) => got.extend_from_slice(p.payload()),
            Err(vaco_core::Error::Eof) => break,
            Err(e) => panic!("{name}: read_packet: {e}"),
        }
    }
    assert_eq!(got, payload, "{name}: payload did not round-trip");
}

fn tone_s16(frames: usize, channels: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..frames {
        let sample = i16::try_from((i as i64 * 37) % 2000 - 1000).unwrap_or(0);
        for _ in 0..channels {
            out.extend_from_slice(&sample.to_le_bytes());
        }
    }
    out
}

fn tone_u8(frames: usize) -> Vec<u8> {
    (0..frames)
        .map(|i| u8::try_from(i * 3 % 256).unwrap_or(0))
        .collect()
}

fn tone_s32be(frames: usize, channels: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..frames {
        let sample = i32::try_from((i as i64 * 12345) % 1_000_000).unwrap_or(0);
        for _ in 0..channels {
            out.extend_from_slice(&sample.to_be_bytes());
        }
    }
    out
}

#[test]
fn wav_round_trips_stereo_s16() {
    roundtrip(
        "wav",
        SampleFmt::S16,
        2,
        &tone_s16(64, 2),
        |s| Ok(Box::new(wav::WavMuxer::new(s)?)),
        |s| {
            Ok(Box::new(wav::WavDemuxer::open(
                s,
                &vaco_format_core::FormatOptions::default(),
            )?))
        },
    );
}

#[test]
fn w64_round_trips_mono_s16() {
    roundtrip(
        "w64",
        SampleFmt::S16,
        1,
        &tone_s16(64, 1),
        |s| Ok(Box::new(w64::W64Muxer::new(s)?)),
        |s| {
            Ok(Box::new(w64::W64Demuxer::open(
                s,
                &vaco_format_core::FormatOptions::default(),
            )?))
        },
    );
}

#[test]
fn aiff_round_trips_mono_s16() {
    roundtrip(
        "aiff",
        SampleFmt::S16,
        1,
        &tone_s16(64, 1),
        |s| Ok(Box::new(aiff::AiffMuxer::new(s)?)),
        |s| {
            Ok(Box::new(aiff::AiffDemuxer::open(
                s,
                &vaco_format_core::FormatOptions::default(),
            )?))
        },
    );
}

#[test]
fn caf_round_trips_stereo_s16() {
    roundtrip(
        "caf",
        SampleFmt::S16,
        2,
        &tone_s16(64, 2),
        |s| Ok(Box::new(caf::CafMuxer::new(s)?)),
        |s| {
            Ok(Box::new(caf::CafDemuxer::open(
                s,
                &vaco_format_core::FormatOptions::default(),
            )?))
        },
    );
}

#[test]
fn au_round_trips_mono_s32() {
    // `.au` is big-endian only (no little-endian PCM encoding exists in the
    // format at all — see `au::codec_to_encoding`), so this needs its own
    // `CodecParameters` rather than `audio_params`'s little-endian default:
    // the payload below is genuinely big-endian (`tone_s32be`), and the
    // codec must say so for `AuMuxer::add_stream` to accept it.
    let mut params = audio_params(SampleFmt::S32, 1);
    params.codec_id = Some(CodecId::PcmS32be);
    roundtrip_with_params(
        "au",
        &params,
        1,
        &tone_s32be(64, 1),
        |s| Ok(Box::new(au::AuMuxer::new(s)?)),
        |s| {
            Ok(Box::new(au::AuDemuxer::open(
                s,
                &vaco_format_core::FormatOptions::default(),
            )?))
        },
    );
}

#[test]
fn voc_round_trips_mono_s16() {
    roundtrip(
        "voc",
        SampleFmt::S16,
        1,
        &tone_s16(64, 1),
        |s| Ok(Box::new(voc::VocMuxer::new(s)?)),
        |s| {
            Ok(Box::new(voc::VocDemuxer::open(
                s,
                &vaco_format_core::FormatOptions::default(),
            )?))
        },
    );
}

#[test]
fn sox_round_trips_stereo_s32() {
    roundtrip(
        "sox",
        SampleFmt::S32,
        2,
        &tone_s32be(64, 2), // native endianness does not matter for a byte-for-byte round trip
        |s| Ok(Box::new(sox::SoxMuxer::new(s)?)),
        |s| {
            Ok(Box::new(sox::SoxDemuxer::open(
                s,
                &vaco_format_core::FormatOptions::default(),
            )?))
        },
    );
}

#[test]
fn ircam_round_trips_mono_s16() {
    roundtrip(
        "ircam",
        SampleFmt::S16,
        1,
        &tone_s16(64, 1),
        |s| Ok(Box::new(ircam::IrcamMuxer::new(s)?)),
        |s| {
            Ok(Box::new(ircam::IrcamDemuxer::open(
                s,
                &vaco_format_core::FormatOptions::default(),
            )?))
        },
    );
}

#[test]
fn rso_round_trips_mono_u8() {
    roundtrip(
        "rso",
        SampleFmt::U8,
        1,
        &tone_u8(64),
        |s| Ok(Box::new(rso::RsoMuxer::new(s)?)),
        |s| {
            Ok(Box::new(rso::RsoDemuxer::open(
                s,
                &vaco_format_core::FormatOptions::default(),
            )?))
        },
    );
}
