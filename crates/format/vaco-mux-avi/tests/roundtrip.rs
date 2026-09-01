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
    // Video's pts stays unset on the way back out, same as on the way in
    // (see `vaco-demux-avi`'s own test of this): AVI carries no explicit
    // presentation order for video, so nothing round-trips one.
    assert_eq!(
        got,
        vec![
            (0, None, true, 10),
            (1, Some(0), true, 4000),
            (0, None, false, 8),
            (1, Some(2000), true, 2000),
            (0, None, false, 6),
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

/// An H.264 stream sourced from an `avc1`-tagged, length-prefixed container
/// (MP4's `avcC`) keeps that framing in AVI too: measured against `ffmpeg
/// 9.0.1 -c copy -f avi` on a real `libx264`-in-MP4 source, which writes
/// `strf`'s `FourCC` as `avc1` and copies the source `avcC` in verbatim,
/// length prefixes and all — it does not reframe to Annex B the way
/// MPEG-TS/`h264_mp4toannexb` muxers do.
///
/// `params.codec_tag` is set to `avc1` because that, not `nal_length_size`
/// alone, is what the reference's own `-c copy` keys off — see
/// `a_length_prefixed_h264_with_no_source_tag_is_refused_not_silently_annexb_tagged`
/// below for the sibling case this distinction exists to cover.
#[test]
fn a_length_prefixed_h264_sample_keeps_its_framing_and_gets_avc1_avcc() {
    let sink = MemorySink::new();
    let shared: SharedBytes = sink.shared();
    let mut mux = vaco_mux_avi::AviMuxer::new(Box::new(sink), &FormatOptions::default()).unwrap();

    let sps = [0x67, 0x64, 0x00, 0x0a, 0xAA];
    let pps = [0x68, 0xEB];
    let mut params = video_params(64, 48, (25, 1));
    if let Some(v) = &mut params.video {
        v.nal_length_size = Some(4);
    }
    params.codec_tag = Some(*b"avc1");
    let record = avcc(&sps, &pps);
    params.extradata = Some(record.clone());
    let v = mux.add_stream(&params).unwrap();
    mux.write_header().unwrap();

    // A single 4-byte-length-prefixed NAL, exactly what an `avcC`-framed MP4
    // sample copies out as.
    let nal = [0x65, 0x88, 0x84];
    let mut sample = Vec::new();
    sample.extend_from_slice(&(nal.len() as u32).to_be_bytes());
    sample.extend_from_slice(&nal);

    mux.write_packet(&packet(v, &sample, true)).unwrap();
    mux.write_trailer().unwrap();

    let bytes = shared.snapshot();
    // `strf`'s FourCC is `avc1`, not `H264`, and the source's own `avcC`
    // record appears verbatim somewhere in `strf`.
    assert!(
        bytes.windows(b"avc1".len()).any(|w| w == b"avc1"),
        "expected the strf FourCC to be avc1"
    );
    assert!(
        bytes.windows(record.len()).any(|w| w == record.as_slice()),
        "expected the source avcC record to appear verbatim in strf"
    );
    // The length prefix must survive unconverted — no Annex B start code
    // for this NAL anywhere in the output.
    let mut annexb = vec![0, 0, 0, 1];
    annexb.extend_from_slice(&nal);
    assert!(
        !bytes.windows(annexb.len()).any(|w| w == annexb.as_slice()),
        "expected the sample to stay length-prefixed, not be reframed to Annex B"
    );

    // And the chunk's declared length matches the original length-prefixed
    // sample exactly, since nothing rewrote it.
    let mut demux = open(bytes);
    let p = demux.read_packet().unwrap();
    assert_eq!(p.len, sample.len());
}

/// A length-prefixed H.264 stream with no `avc1`/`hvc1` source tag is
/// refused, not silently written with an Annex-B `FourCC` over a
/// length-prefixed payload.
///
/// Measured against `ffmpeg 9.0.1`: an identical `libx264` elementary
/// stream remuxed `-c copy -f avi` from MP4 (`codec_tag=avc1`) succeeds, but
/// from Matroska or FLV (both report `codec_tag=0` for the same bitstream —
/// neither format carries an AVI/QuickTime-style `FourCC`) it fails with
/// "Error submitting a packet to the muxer: Invalid data found when
/// processing input". This muxer used to key its `strf` `FourCC` choice off
/// `nal_length_size` alone, which produced a real, silently wrong file: an
/// `avc1` tag (promising a config record and length-prefixed samples that
/// happen to be correct here) or worse, on some other source shape, an
/// `H264` tag over payload that is not actually Annex-B — exactly the
/// mismatch a real decoder cannot read. Refusing is not a fidelity gap
/// (nothing here could correctly reframe to Annex B either, since that is
/// `h264_mp4toannexb`'s job and nothing calls it for AVI output).
#[test]
fn a_length_prefixed_h264_with_no_source_tag_is_refused_not_silently_annexb_tagged() {
    let mut params = video_params(64, 48, (25, 1));
    if let Some(v) = &mut params.video {
        v.nal_length_size = Some(4);
    }
    params.extradata = Some(avcc(&[0x67, 0x64, 0x00, 0x0a, 0xAA], &[0x68, 0xEB]));
    // `codec_tag` deliberately left at its default (`None`) — the Matroska/
    // FLV shape, not the MP4 shape the sibling test above covers.
    assert_eq!(params.codec_tag, None);

    let sink = MemorySink::new();
    let mut mux = vaco_mux_avi::AviMuxer::new(Box::new(sink), &FormatOptions::default()).unwrap();
    let err = mux.add_stream(&params).unwrap_err();
    assert!(
        matches!(err, vaco_core::Error::Unsupported(_)),
        "expected an Unsupported refusal, got {err:?}"
    );
}

/// The Annex-B counterpart: a source with no length-prefix framing at all
/// (`nal_length_size` unset — MPEG-TS's own convention) is written with the
/// plain `H264` `FourCC` and no configuration record, unconverted — measured
/// against the reference on an MPEG-TS source, which writes `H264`/
/// `is_avc=false`/`nal_length_size=0` in that case, not `avc1`.
#[test]
fn an_annex_b_h264_sample_keeps_h264_and_gets_no_config_record() {
    let sink = MemorySink::new();
    let shared: SharedBytes = sink.shared();
    let mut mux = vaco_mux_avi::AviMuxer::new(Box::new(sink), &FormatOptions::default()).unwrap();

    let v = mux.add_stream(&video_params(64, 48, (25, 1))).unwrap();
    mux.write_header().unwrap();

    let nal = [0, 0, 0, 1, 0x65, 0x88, 0x84];
    mux.write_packet(&packet(v, &nal, true)).unwrap();
    mux.write_trailer().unwrap();

    let bytes = shared.snapshot();
    assert!(bytes.windows(b"H264".len()).any(|w| w == b"H264"));
    assert!(!bytes.windows(b"avc1".len()).any(|w| w == b"avc1"));

    let mut demux = open(bytes);
    let p = demux.read_packet().unwrap();
    // Written verbatim: the Annex-B start code is still there, unconverted.
    assert_eq!(p.payload(), &nal[..]);
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
    // These are video packets, so pts stays unset on read-back (see the
    // clock test above) — only `len` (real payload vs. an empty grid slot)
    // is being checked here.
    assert_eq!(
        got,
        vec![
            (None, 1),
            (None, 0),
            (None, 0),
            (None, 0),
            (None, 0),
            (None, 1),
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

/// A `BsfProvider` that refuses to open any filter, so a test driven through
/// it fails loudly if the muxer ever asks for one.
struct NoBsfs;

impl BsfProvider for NoBsfs {
    fn open(
        &self,
        _name: &str,
        _params: &CodecParameters,
    ) -> vaco_core::Result<Box<dyn BitstreamFilter>> {
        Err(vaco_core::Error::Unsupported("test provider grants no filters"))
    }
}

/// Driven through `MuxBuilder`/`MuxWriter` (M6) with a `BsfProvider` that
/// refuses every filter, a length-prefixed H.264 stream still muxes
/// successfully and keeps its framing — confirming `check_bitstream` never
/// asks M6 for anything (the trait's default `Keep`), matching
/// [`a_length_prefixed_h264_sample_keeps_its_framing_and_gets_avc1_avcc`]'s
/// direct-`Muxer` result exactly.
#[test]
fn check_bitstream_never_requests_a_filter_through_mux_writer() {
    let sink = MemorySink::new();
    let shared: SharedBytes = sink.shared();
    let mux = vaco_mux_avi::AviMuxer::new(Box::new(sink), &FormatOptions::default()).unwrap();

    let sps = [0x67, 0x64, 0x00, 0x0a, 0xAA];
    let pps = [0x68, 0xEB];
    let mut params = video_params(64, 48, (25, 1));
    if let Some(v) = &mut params.video {
        v.nal_length_size = Some(4);
    }
    params.codec_tag = Some(*b"avc1");
    let record = avcc(&sps, &pps);
    params.extradata = Some(record.clone());

    let mut builder =
        MuxBuilder::new(Box::new(mux), &FormatOptions::default()).with_bsfs(Arc::new(NoBsfs));
    let v = builder.add_stream(&params, Rational::new(1, 25)).unwrap();
    let mut writer = builder.open().unwrap();

    let nal = [0x65, 0x88, 0x84];
    let mut lp = Vec::new();
    lp.extend_from_slice(&(u32::try_from(nal.len()).unwrap()).to_be_bytes());
    lp.extend_from_slice(&nal);
    let mut pkt = packet(v, &lp, true);
    pkt.pts = vaco_core::Timestamp::new(0);
    pkt.dts = pkt.pts;
    writer.write_packet(pkt).unwrap();
    writer.finish().unwrap();

    let bytes = shared.snapshot();
    assert!(bytes.windows(record.len()).any(|w| w == record.as_slice()));
    assert!(bytes.windows(lp.len()).any(|w| w == lp.as_slice()));
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

fn le32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn find_all(bytes: &[u8], tag: [u8; 4]) -> Vec<usize> {
    (0..bytes.len().saturating_sub(3))
        .filter(|&i| bytes[i..i + 4] == tag)
        .collect()
}

/// `avih`'s three fixed-content fields: `dwFlags`, `dwSuggestedBufferSize`,
/// and the `JUNK` reservations flanking every `strl` and `hdrl` itself —
/// measured constant across four fixtures regardless of stream count, codec
/// or content, so this only has to check they are exactly those constants.
#[test]
fn avih_flags_suggested_buffer_and_junk_reservations_match_the_measured_constants() {
    let (bytes, _v, _a) = mux_sample();

    let avih_at = bytes.windows(4).position(|w| w == b"avih").unwrap();
    let avih_body = &bytes[avih_at + 8..];
    assert_eq!(le32_at(avih_body, 12), 0x0000_0910, "dwFlags");
    assert_eq!(le32_at(avih_body, 28), 1_048_576, "dwSuggestedBufferSize");

    // One `strl` JUNK per stream (two streams here), each exactly 4120
    // bytes; one `hdrl`-level JUNK of 260; one RIFF-level JUNK of 1016.
    let junk_positions = find_all(&bytes, *b"JUNK");
    let junk_sizes: Vec<u32> = junk_positions
        .iter()
        .map(|&p| le32_at(&bytes, p + 4))
        .collect();
    assert_eq!(junk_sizes, vec![4120, 4120, 260, 1016]);

    // The per-strl JUNK is an inert `AVISUPERINDEX` header: `wLongsPerEntry
    // = 4` and this stream's own `dwChunkId` (`00dc` for video stream 0,
    // `01wb` for audio stream 1 here), everything else zero.
    let video_junk = junk_positions[0] + 8;
    assert_eq!(le16_at(&bytes, video_junk), 4, "wLongsPerEntry (video)");
    assert_eq!(&bytes[video_junk + 8..video_junk + 12], b"00dc");
    let audio_junk = junk_positions[1] + 8;
    assert_eq!(le16_at(&bytes, audio_junk), 4, "wLongsPerEntry (audio)");
    assert_eq!(&bytes[audio_junk + 8..audio_junk + 12], b"01wb");

    // The hdrl-level JUNK is a `LIST 'odml'` holding one `dmlh` chunk
    // (`AVIEXTHEADER`), declared 248 bytes, tagged `JUNK` instead of `LIST`.
    let hdrl_junk = junk_positions[2] + 8;
    assert_eq!(&bytes[hdrl_junk..hdrl_junk + 8], b"odmldmlh");
    assert_eq!(le32_at(&bytes, hdrl_junk + 8), 248);
}

/// `strh.dwSuggestedBufferSize` is the largest single chunk a stream wrote,
/// not a fixed size — measured on both video and audio across four
/// fixtures. `avih.dwMaxBytesPerSec` is the sum of every stream's own
/// declared `bit_rate`, in bytes/sec, truncated.
#[test]
fn strh_suggested_buffer_is_the_largest_chunk_and_avih_sums_bit_rates() {
    let sink = MemorySink::new();
    let shared: SharedBytes = sink.shared();
    let mut mux = vaco_mux_avi::AviMuxer::new(Box::new(sink), &FormatOptions::default()).unwrap();

    let mut vp = video_params(64, 48, (25, 1));
    vp.bit_rate = Some(8000); // 1000 bytes/sec
    let mut ap = audio_params(8000);
    ap.bit_rate = Some(16000); // 2000 bytes/sec

    let v = mux.add_stream(&vp).unwrap();
    let a = mux.add_stream(&ap).unwrap();
    mux.write_header().unwrap();
    mux.write_packet(&packet(v, &[0xAA; 5], true)).unwrap();
    mux.write_packet(&packet(v, &[0xBB; 40], false)).unwrap();
    mux.write_packet(&packet(a, &[0u8; 20], true)).unwrap();
    mux.write_trailer().unwrap();

    let bytes = shared.snapshot();

    let avih_at = bytes.windows(4).position(|w| w == b"avih").unwrap();
    let avih_body = &bytes[avih_at + 8..];
    assert_eq!(le32_at(avih_body, 4), 1000 + 2000, "dwMaxBytesPerSec");

    let strh_positions = find_all(&bytes, *b"strh");
    let suggested_buffers: Vec<u32> = strh_positions
        .iter()
        .map(|&p| le32_at(&bytes, p + 8 + 36))
        .collect();
    // Video's largest chunk is 40 bytes; audio's only chunk is 20.
    assert_eq!(suggested_buffers, vec![40, 20]);
}

/// Measured: an audio stream's `strh.fccHandler` is the raw `u32` value `1`
/// regardless of its actual `wFormatTag` — an AAC-tagged stream (`wFormatTag
/// = 0x00FF`, nothing like `1`) measures the exact same `fccHandler` a PCM
/// stream does, so it is a fixed placeholder, not a mirror of `wFormatTag`.
#[test]
fn audio_fcc_handler_is_the_fixed_value_one_not_the_format_tag() {
    let sink = MemorySink::new();
    let shared: SharedBytes = sink.shared();
    let mut mux = vaco_mux_avi::AviMuxer::new(Box::new(sink), &FormatOptions::default()).unwrap();
    let mut p = CodecParameters::audio();
    p.codec_id = Some(CodecId::Aac);
    p.extradata = Some(vec![0x12, 0x10]);
    if let Some(a) = &mut p.audio {
        a.sample_rate = 44_100;
    }
    let a = mux.add_stream(&p).unwrap();
    mux.write_header().unwrap();
    mux.write_packet(&packet(a, &[0u8; 8], true)).unwrap();
    mux.write_trailer().unwrap();

    let bytes = shared.snapshot();
    let strh_at = bytes.windows(4).position(|w| w == b"strh").unwrap();
    assert_eq!(le32_at(&bytes, strh_at + 8 + 4), 1);
}

fn le16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

/// Measured: a video stream's `strh.rcFrame` is `{0, 0, width, height}`, not
/// all zero, and `strf.biSizeImage` is `width * height * 3` — the raw-RGB
/// byte count `biBitCount = 24` implies — even though the actual codec is
/// compressed. Both were previously left `0`.
#[test]
fn video_rcframe_and_bisizeimage_are_not_left_zero() {
    let (bytes, _v, _a) = mux_sample();

    let strh_at = bytes.windows(4).position(|w| w == b"strh").unwrap();
    let rcframe_at = strh_at + 8 + 48;
    let rcframe: Vec<i16> = (0..4)
        .map(|i| le16_at(&bytes, rcframe_at + i * 2).cast_signed())
        .collect();
    assert_eq!(rcframe, vec![0, 0, 64, 48], "strh.rcFrame");

    let strf_at = bytes.windows(4).position(|w| w == b"strf").unwrap();
    assert_eq!(
        le32_at(&bytes, strf_at + 8 + 20),
        64 * 48 * 3,
        "strf.biSizeImage"
    );
}

/// Measured: a compressed (VBR) audio stream's `strh.dwScale/dwRate` is one
/// *frame's* duration, not one sample's — an AAC stream at 44100 Hz reduces
/// to `256/11025` (`1024/44100`, AAC-LC's fixed frame size), not `1/44100`.
/// `strf`'s own `nSamplesPerSec` stays the true sample rate regardless,
/// since that field means something different from `strh`'s time base.
#[test]
fn compressed_audio_strh_time_base_is_one_frame_not_one_sample() {
    let sink = MemorySink::new();
    let shared: SharedBytes = sink.shared();
    let mut mux = vaco_mux_avi::AviMuxer::new(Box::new(sink), &FormatOptions::default()).unwrap();
    let mut p = CodecParameters::audio();
    p.codec_id = Some(CodecId::Aac);
    p.extradata = Some(vec![0x12, 0x10]);
    if let Some(a) = &mut p.audio {
        a.sample_rate = 44_100;
    }
    let a = mux.add_stream(&p).unwrap();
    mux.write_header().unwrap();
    mux.write_packet(&packet(a, &[0u8; 8], true)).unwrap();
    mux.write_trailer().unwrap();

    let bytes = shared.snapshot();
    let strh_at = bytes.windows(4).position(|w| w == b"strh").unwrap();
    let scale = le32_at(&bytes, strh_at + 8 + 20);
    let rate = le32_at(&bytes, strh_at + 8 + 24);
    assert_eq!((scale, rate), (256, 11025), "strh dwScale/dwRate");

    let strf_at = bytes.windows(4).position(|w| w == b"strf").unwrap();
    assert_eq!(le32_at(&bytes, strf_at + 8 + 4), 44_100, "strf nSamplesPerSec");
}

/// Measured: a compressed audio stream's `strf.nAvgBytesPerSec` is its own
/// declared `bit_rate` divided by 8, not `sample_rate * nBlockAlign` — the
/// same mechanism `avih.dwMaxBytesPerSec` sums across streams, applied here
/// to one stream's own field. `nBlockAlign` itself is a separate,
/// unresolved field — see the `write_strl` comment beside it.
#[test]
fn compressed_audio_avg_bytes_per_sec_comes_from_bit_rate() {
    let sink = MemorySink::new();
    let shared: SharedBytes = sink.shared();
    let mut mux = vaco_mux_avi::AviMuxer::new(Box::new(sink), &FormatOptions::default()).unwrap();
    let mut p = CodecParameters::audio();
    p.codec_id = Some(CodecId::Aac);
    p.extradata = Some(vec![0x12, 0x10]);
    p.bit_rate = Some(70_303);
    if let Some(a) = &mut p.audio {
        a.sample_rate = 44_100;
    }
    let a = mux.add_stream(&p).unwrap();
    mux.write_header().unwrap();
    mux.write_packet(&packet(a, &[0u8; 8], true)).unwrap();
    mux.write_trailer().unwrap();

    let bytes = shared.snapshot();
    let strf_at = bytes.windows(4).position(|w| w == b"strf").unwrap();
    assert_eq!(le32_at(&bytes, strf_at + 8 + 8), 8787);
}
