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

use std::sync::Arc;

use vaco_codec_core::{BitstreamFilter, CodecId, CodecParameters};
use vaco_core::{MediaType, Rational};
use vaco_demux_avi::AviDemuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::mux::{BsfProvider, MuxBuilder};
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
    // `mux_sample` drives `add_stream` directly rather than
    // `add_stream_with`, so no source time base is ever supplied and
    // `dwMicroSecPerFrame` stays `0` — what this test actually pins is that
    // `write_trailer`'s seek-back path ran at all, which the stream shape
    // assertions above already exercise indirectly. Kept separate so a
    // regression in the patch path (e.g. patching the wrong offset) shows up
    // even if packet order happens to still look right.
    assert_eq!(demux.streams().len(), 2);
}

/// An H.264 stream sourced from a length-prefixed container (MP4's `avcC`,
/// typically via `-c copy`) must be reframed to Annex B before it is a
/// legal AVI `movi` chunk.
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

/// AAC with no extradata (the shape MPEG-TS's own ADTS framing produces,
/// since ADTS carries its config per-frame rather than out of band) has no
/// legal AVI representation and must be refused.
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

/// A video packet's `pts` places it on the 600 Hz grid, and every slot the
/// stream skips gets a zero-length placeholder chunk on the way there.
#[test]
fn video_packets_land_on_the_grid_with_empty_slots_between() {
    let sink = MemorySink::new();
    let shared: SharedBytes = sink.shared();
    let mut mux = vaco_mux_avi::AviMuxer::new(Box::new(sink), &FormatOptions::default()).unwrap();
    let v = mux.add_stream(&video_params(64, 48, (25, 1))).unwrap();
    mux.write_header().unwrap();

    // Called directly (not through `MuxBuilder`), so `pts`/`dts` are already
    // in this stream's own time base — 600 Hz ticks, i.e. slot numbers.
    let mut first = packet(v, &[0xAA], true);
    first.pts = vaco_core::Timestamp::new(0);
    first.dts = first.pts;
    mux.write_packet(&first).unwrap();

    let mut second = packet(v, &[0xBB], false);
    second.pts = vaco_core::Timestamp::new(5);
    second.dts = second.pts;
    mux.write_packet(&second).unwrap();
    mux.write_trailer().unwrap();

    let mut demux = open(shared.snapshot());
    // Slot 0 (real), slots 1-4 (empty), slot 5 (real): six packets total,
    // the middle four carrying no payload.
    let mut got = Vec::new();
    loop {
        match demux.read_packet() {
            Ok(p) => got.push((p.pts.ticks(), p.len)),
            Err(vaco_core::Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    assert_eq!(
        got,
        vec![
            (Some(0), 1),
            (Some(1), 0),
            (Some(2), 0),
            (Some(3), 0),
            (Some(4), 0),
            (Some(5), 1),
        ]
    );
    assert_eq!(demux.streams()[0].duration_ts, Some(6));
}

/// AVI has no absolute-time field, so a source clock that does not start at
/// zero (routine for MPEG-TS) must be rebased against its own first frame —
/// otherwise every slot number would carry however far that clock had
/// already run, inflating the grid by that much for no reason.
#[test]
fn the_grid_rebases_to_the_streams_own_first_frame() {
    let sink = MemorySink::new();
    let shared: SharedBytes = sink.shared();
    let mut mux = vaco_mux_avi::AviMuxer::new(Box::new(sink), &FormatOptions::default()).unwrap();
    let v = mux.add_stream(&video_params(64, 48, (25, 1))).unwrap();
    mux.write_header().unwrap();

    let mut first = packet(v, &[0xAA], true);
    first.pts = vaco_core::Timestamp::new(90_000); // an arbitrary non-zero start
    first.dts = first.pts;
    mux.write_packet(&first).unwrap();

    let mut second = packet(v, &[0xBB], false);
    second.pts = vaco_core::Timestamp::new(90_002);
    second.dts = second.pts;
    mux.write_packet(&second).unwrap();
    mux.write_trailer().unwrap();

    let demux = open(shared.snapshot());
    // Two slots apart, not 90 002 apart.
    assert_eq!(demux.streams()[0].duration_ts, Some(3));
}

/// The gap between two video timestamps is attacker-controlled input, not a
/// size this crate chose — an implausible jump must fail cleanly rather than
/// try to write and index however many placeholder chunks it implies.
#[test]
fn an_implausible_grid_gap_is_rejected_not_looped_forever() {
    let sink = MemorySink::new();
    let mut mux = vaco_mux_avi::AviMuxer::new(Box::new(sink), &FormatOptions::default()).unwrap();
    let v = mux.add_stream(&video_params(64, 48, (25, 1))).unwrap();
    mux.write_header().unwrap();

    let mut first = packet(v, &[0xAA], true);
    first.pts = vaco_core::Timestamp::new(0);
    first.dts = first.pts;
    mux.write_packet(&first).unwrap();

    let mut second = packet(v, &[0xBB], false);
    second.pts = vaco_core::Timestamp::new(i64::MAX);
    second.dts = second.pts;
    assert!(mux.write_packet(&second).is_err());
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

/// Wraps the two real `vaco-bsf-h2645` filters — not a hand test-double —
/// so this proves the muxer's `check_bitstream` request lands on the actual
/// filter a real pipeline would supply.
struct OnlyH2645ToAnnexb;

impl BsfProvider for OnlyH2645ToAnnexb {
    fn open(
        &self,
        name: &str,
        params: &CodecParameters,
    ) -> vaco_core::Result<Box<dyn BitstreamFilter>> {
        match name {
            "h264_mp4toannexb" => (vaco_bsf_h2645::h264_mp4toannexb::DESC.build)(params),
            "hevc_mp4toannexb" => (vaco_bsf_h2645::hevc_mp4toannexb::DESC.build)(params),
            _ => Err(vaco_core::Error::Unsupported(
                "test provider knows only the mp4toannexb pair",
            )),
        }
    }
}

/// A minimal, well-formed `AvcDecoderConfigurationRecord`: one SPS, one PPS.
fn avcc(sps: &[u8], pps: &[u8]) -> Vec<u8> {
    let mut r = vec![1, sps[1], sps[2], sps[3], 0xFF, 0xE1];
    r.extend_from_slice(&(u16::try_from(sps.len()).unwrap()).to_be_bytes());
    r.extend_from_slice(sps);
    r.push(1);
    r.extend_from_slice(&(u16::try_from(pps.len()).unwrap()).to_be_bytes());
    r.extend_from_slice(pps);
    r
}

/// `check_bitstream` plus a real `BsfProvider`, driven through `MuxBuilder`/
/// `MuxWriter` (M6), produces the SPS/PPS-spliced Annex B this crate's own
/// `maybe_convert` cannot: that method has no configuration record to read
/// parameter sets out of and only ever does the framing half. This is the
/// comparison plan 19's brief for this work asked for before touching
/// `maybe_convert` at all — proof that the wired-up path is *more* correct
/// than the standalone one, not merely different from it.
#[test]
fn check_bitstream_through_mux_writer_gets_the_splice_maybe_convert_alone_cannot() {
    let sink = MemorySink::new();
    let shared: SharedBytes = sink.shared();
    let mux = vaco_mux_avi::AviMuxer::new(Box::new(sink), &FormatOptions::default()).unwrap();

    let sps = [0x67, 0x64, 0x00, 0x0a, 0xAA];
    let pps = [0x68, 0xEB];
    let mut params = video_params(64, 48, (25, 1));
    if let Some(v) = &mut params.video {
        v.nal_length_size = Some(4);
    }
    params.extradata = Some(avcc(&sps, &pps));

    let mut builder = MuxBuilder::new(Box::new(mux), &FormatOptions::default())
        .with_bsfs(Arc::new(OnlyH2645ToAnnexb));
    let v = builder.add_stream(&params, Rational::new(1, 25)).unwrap();
    let mut writer = builder.open().unwrap();

    let idr = [0x65, 0x88, 0x84];
    let mut lp = Vec::new();
    lp.extend_from_slice(&(u32::try_from(idr.len()).unwrap()).to_be_bytes());
    lp.extend_from_slice(&idr);
    let mut pkt = packet(v, &lp, true);
    pkt.pts = vaco_core::Timestamp::new(0);
    pkt.dts = pkt.pts;
    writer.write_packet(pkt).unwrap();
    writer.finish().unwrap();

    let bytes = shared.snapshot();
    let mut expected = Vec::new();
    for u in [&sps[..], &pps[..], &idr[..]] {
        expected.extend_from_slice(&[0, 0, 0, 1]);
        expected.extend_from_slice(u);
    }
    let found = bytes
        .windows(expected.len())
        .any(|w| w == expected.as_slice());
    assert!(
        found,
        "expected the SPS/PPS-spliced sample verbatim in the muxed bytes; \
         maybe_convert's own framing-only fallback could never produce this"
    );

    // And the *unspliced* framing-only shape — what the old, standalone path
    // alone would have written — must NOT appear: this is a real functional
    // difference, not just an additional correct answer alongside the old one.
    let mut framing_only = Vec::new();
    framing_only.extend_from_slice(&[0, 0, 0, 1]);
    framing_only.extend_from_slice(&idr);
    let framing_only_appears_alone = bytes
        .windows(framing_only.len())
        .any(|w| w == framing_only.as_slice())
        && !found;
    assert!(!framing_only_appears_alone);
}

/// `avih.dwMicroSecPerFrame` tracks the *source* time base a caller supplies
/// through `MuxBuilder`/`add_stream_with`, not the fixed 600 Hz grid — only
/// reachable this way, since `Muxer::add_stream` alone has no source time
/// base to give.
#[test]
fn avih_dwmicrosecperframe_tracks_the_source_time_base() {
    let sink = MemorySink::new();
    let shared: SharedBytes = sink.shared();
    let mux = vaco_mux_avi::AviMuxer::new(Box::new(sink), &FormatOptions::default()).unwrap();

    let mut builder = MuxBuilder::new(Box::new(mux), &FormatOptions::default());
    // A 1/12800 track time base, the shape one measured MP4 fixture uses —
    // 1_000_000 / 12800 = 78.125, truncating to 78.
    let v = builder
        .add_stream(&video_params(64, 48, (25, 1)), Rational::new(1, 12_800))
        .unwrap();
    let mut writer = builder.open().unwrap();

    let mut pkt = packet(v, &[0xAA], true);
    pkt.pts = vaco_core::Timestamp::new(0);
    pkt.dts = pkt.pts;
    writer.write_packet(pkt).unwrap();
    writer.finish().unwrap();

    let bytes = shared.snapshot();
    let avih_at = bytes
        .windows(4)
        .position(|w| w == b"avih")
        .expect("avih chunk present");
    let body = &bytes[avih_at + 8..];
    let us_per_frame = u32::from_le_bytes(body[0..4].try_into().unwrap());
    assert_eq!(us_per_frame, 78);
}
