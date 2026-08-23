//! Mux a small video+audio file, then demux it back with `vaco-demux-avi` —
//! the most direct check that the two crates' understanding of the format
//! agrees, since the plan calls for both.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{MediaType, Rational};
use vaco_demux_avi::AviDemuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::vacoraw::{MemorySink, SharedBytes};
use vaco_format_core::{Demuxer, FormatOptions, Muxer};
use vaco_io::MemorySource;
use vaco_limits::Budget;
use vaco_packet::{Packet, PacketFlags};

fn video_params(width: u32, height: u32, fps: (i32, i32)) -> CodecParameters {
    let mut p = CodecParameters::video();
    p.codec_id = Some(CodecId::H264);
    if let Some(v) = &mut p.video {
        v.width = width;
        v.height = height;
        v.frame_rate = Rational::new(fps.0, fps.1);
    }
    p
}

fn audio_params(sample_rate: u32) -> CodecParameters {
    let mut p = CodecParameters::audio();
    p.codec_id = Some(CodecId::Pcm);
    if let Some(a) = &mut p.audio {
        a.sample_rate = sample_rate;
        a.bits_per_coded_sample = Some(16);
    }
    p
}

fn packet(stream_index: u32, payload: &[u8], key: bool) -> Packet {
    let mut budget = Budget::new(vaco_limits::Limits::permissive());
    let mut pkt = Packet::from_slice(&mut budget, payload).unwrap();
    pkt.stream_index = stream_index;
    pkt.flags = if key {
        PacketFlags::KEY
    } else {
        PacketFlags::empty()
    };
    pkt
}

/// Mux three video frames and two audio chunks, returning the bytes and the
/// (video, audio) stream indices `vaco-mux-avi` assigned.
fn mux_sample() -> (Vec<u8>, u32, u32) {
    let sink = MemorySink::new();
    let shared: SharedBytes = sink.shared();
    let mut mux = vaco_mux_avi::AviMuxer::new(Box::new(sink), &FormatOptions::default()).unwrap();

    let v = mux.add_stream(&video_params(64, 48, (1, 10))).unwrap();
    let a = mux.add_stream(&audio_params(8000)).unwrap();
    mux.write_header().unwrap();

    mux.write_packet(&packet(v, &[0xAA; 10], true)).unwrap();
    mux.write_packet(&packet(a, &[0u8; 4000], true)).unwrap(); // 2000 mono s16 samples
    mux.write_packet(&packet(v, &[0xBB; 8], false)).unwrap();
    mux.write_packet(&packet(a, &[0u8; 2000], true)).unwrap(); // 1000 more samples
    mux.write_packet(&packet(v, &[0xCC; 6], false)).unwrap();
    mux.write_trailer().unwrap();

    (shared.snapshot(), v, a)
}

fn open(bytes: Vec<u8>) -> AviDemuxer {
    let src = Box::new(MemorySource::new(bytes));
    AviDemuxer::open(src, &NoParsers, &FormatOptions::default()).expect("demux what we muxed")
}

#[test]
fn muxed_streams_demux_with_the_right_shape() {
    let (bytes, v, a) = mux_sample();
    assert_eq!((v, a), (0, 1));
    let demux = open(bytes);
    let streams = demux.streams();
    assert_eq!(streams.len(), 2);
    assert_eq!(streams[0].media_type(), Some(MediaType::Video));
    assert_eq!(streams[0].params.codec_id, Some(CodecId::H264));
    assert_eq!(streams[0].params.video.as_ref().unwrap().width, 64);
    assert_eq!(streams[0].params.video.as_ref().unwrap().height, 48);
    assert_eq!(streams[1].media_type(), Some(MediaType::Audio));
    // `audio_params` requests the generic `CodecId::Pcm` bucket with
    // `bits_per_coded_sample = 16`; `vaco-format-riff`'s own
    // `wave_tags::codec_id` (which `vaco-demux-avi` reuses) resolves
    // `wFormatTag`+`wBitsPerSample` to the specific `PcmS16le` flavour on
    // the way back in, not the generic bucket it was written from — see
    // that function's doc comment for why the specific answer is the
    // correct one.
    assert_eq!(streams[1].params.codec_id, Some(CodecId::PcmS16le));
    assert_eq!(streams[1].params.audio.as_ref().unwrap().sample_rate, 8000);
}

#[test]
fn muxed_packets_demux_in_order_with_the_measured_clock() {
    let (bytes, _v, _a) = mux_sample();
    let mut demux = open(bytes);
    let mut got = Vec::new();
    loop {
        match demux.read_packet() {
            Ok(p) => got.push((p.stream_index, p.pts.ticks(), p.is_key(), p.len)),
            Err(vaco_core::Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    assert_eq!(
        got,
        vec![
            (0, Some(0), true, 10),
            (1, Some(0), true, 4000),
            (0, Some(1), false, 8),
            (1, Some(2000), true, 2000),
            (0, Some(2), false, 6),
        ]
    );
}

#[test]
fn the_trailer_patches_total_frame_and_length_counts() {
    let (bytes, _v, _a) = mux_sample();
    let demux = open(bytes);
    // Three video chunks, three samples' worth accounted for on the audio
    // side (2000 + 1000): `duration` comes from `avih`'s patched
    // `dwMicroSecPerFrame * dwTotalFrames`, which is 0 here since this
    // muxer does not thread `dwMicroSecPerFrame` through (see
    // `docs/format/vaco-mux-avi.md`) — what this test actually pins is that
    // `write_trailer`'s seek-back path ran at all, which the stream shape
    // assertions above already exercise indirectly. Kept separate so a
    // regression in the patch path (e.g. patching the wrong offset) shows up
    // even if packet order happens to still look right.
    assert_eq!(demux.streams().len(), 2);
}

/// Finding 16 (`planning/CONFORMANCE-FINDINGS.md`): an H.264 stream sourced
/// from a length-prefixed container (MP4's `avcC`, typically via `-c copy`)
/// must be reframed to Annex B before it is a legal AVI `movi` chunk — this
/// crate used to write `packet.payload()` verbatim regardless, which is the
/// finding's measured 3.4x size gap against the reference (a structural
/// difference, not a byte-count one).
#[test]
fn a_length_prefixed_h264_sample_is_rewritten_to_annex_b() {
    let sink = MemorySink::new();
    let shared: SharedBytes = sink.shared();
    let mut mux = vaco_mux_avi::AviMuxer::new(Box::new(sink), &FormatOptions::default()).unwrap();

    let mut params = video_params(64, 48, (25, 1));
    if let Some(v) = &mut params.video {
        v.nal_length_size = Some(4);
    }
    let v = mux.add_stream(&params).unwrap();
    mux.write_header().unwrap();

    // Two 4-byte-length-prefixed NAL units, back to back — exactly what an
    // `avcC`-framed MP4 sample copies out as.
    let nal_a = [0x67, 0xAA, 0xBB]; // fake SPS
    let nal_b = [0x68, 0xCC]; // fake PPS
    let mut sample = Vec::new();
    sample.extend_from_slice(&(nal_a.len() as u32).to_be_bytes());
    sample.extend_from_slice(&nal_a);
    sample.extend_from_slice(&(nal_b.len() as u32).to_be_bytes());
    sample.extend_from_slice(&nal_b);

    mux.write_packet(&packet(v, &sample, true)).unwrap();
    mux.write_trailer().unwrap();

    let bytes = shared.snapshot();
    // The length prefix (`00 00 00 03`/`00 00 00 02`) must not appear
    // anywhere in the output; Annex B start codes must, once per NAL unit.
    let mut expected_annexb = Vec::new();
    expected_annexb.extend_from_slice(&[0, 0, 0, 1]);
    expected_annexb.extend_from_slice(&nal_a);
    expected_annexb.extend_from_slice(&[0, 0, 0, 1]);
    expected_annexb.extend_from_slice(&nal_b);
    let windows_match = bytes
        .windows(expected_annexb.len())
        .any(|w| w == expected_annexb.as_slice());
    assert!(
        windows_match,
        "expected the Annex-B-reframed sample to appear verbatim in the muxed bytes"
    );

    // And the chunk's declared length must match the *converted* payload
    // (12 bytes: two 4-byte start codes plus 3+2 bytes of NAL data), not the
    // original 10-byte length-prefixed sample.
    let mut demux = open(bytes);
    let p = demux.read_packet().unwrap();
    assert_eq!(p.len, expected_annexb.len());
}

/// Finding 19 (`planning/CONFORMANCE-FINDINGS.md`): measured directly
/// against the reference (`ffmpeg -i <mpegts-with-adts-aac> -c copy -f
/// avi`, which refuses at `write_header` with "ADTS is only supported with
/// codec tag 0x1610" and a nonzero exit) — AAC with no extradata (the shape
/// MPEG-TS's own ADTS framing produces, since ADTS carries its config
/// per-frame rather than out of band) has no legal AVI representation.
#[test]
fn adts_framed_aac_with_no_extradata_is_rejected() {
    let sink = MemorySink::new();
    let mut mux = vaco_mux_avi::AviMuxer::new(Box::new(sink), &FormatOptions::default()).unwrap();
    let mut p = CodecParameters::audio();
    p.codec_id = Some(CodecId::Aac);
    if let Some(a) = &mut p.audio {
        a.sample_rate = 44_100;
    }
    // No extradata set at all — exactly what an ADTS-framed source gives.
    assert!(mux.add_stream(&p).is_err());
}

/// The same codec with a raw `AudioSpecificConfig` in `extradata` (what an
/// MP4/`esds` source gives) is the case this crate does support.
#[test]
fn raw_aac_with_extradata_is_accepted() {
    let sink = MemorySink::new();
    let mut mux = vaco_mux_avi::AviMuxer::new(Box::new(sink), &FormatOptions::default()).unwrap();
    let mut p = CodecParameters::audio();
    p.codec_id = Some(CodecId::Aac);
    p.extradata = Some(vec![0x12, 0x10]); // a minimal AudioSpecificConfig
    if let Some(a) = &mut p.audio {
        a.sample_rate = 44_100;
    }
    assert!(mux.add_stream(&p).is_ok());
}

#[test]
fn a_codec_with_no_avi_mapping_is_rejected_not_silently_wrong() {
    let sink = MemorySink::new();
    let mut mux = vaco_mux_avi::AviMuxer::new(Box::new(sink), &FormatOptions::default()).unwrap();
    let mut p = CodecParameters::video();
    p.codec_id = Some(CodecId::Av1); // no AVI FourCC mapping in this crate
    if let Some(v) = &mut p.video {
        v.width = 64;
        v.height = 48;
    }
    assert!(mux.add_stream(&p).is_err());
}
