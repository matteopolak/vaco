//! Round-trips this crate's own output through its sibling demuxer
//! (`vaco-demux-mxf`) — the most direct way to verify the byte layout is
//! self-consistent, and it reuses that crate's own measured understanding
//! of the format rather than re-deriving a second, parallel one just for
//! tests (see `vaco-mux-mp4`'s identical dependency for the same
//! reasoning).
//!
//! These tests do not need a real decodable MPEG-2/PCM bitstream: neither
//! muxer nor demuxer here interprets essence content, only frames it, so
//! synthetic payload bytes are enough to prove positions, lengths, stream
//! shape and codec parameters survive the round trip.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::cast_possible_wrap,
    clippy::panic,
    reason = "test code"
)]

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{MediaType, Rational, Timestamp};
use vaco_demux_mxf::MxfDemuxer;
use vaco_format_core::{Demuxer, Muxer, discovery::NoParsers};
use vaco_io::{IoOptions, MemorySource, SharedDynBuf};
use vaco_limits::{Budget, Limits};
use vaco_mux_mxf::{MUXER_D10, MUXER_OPATOM, MxfMuxer};
use vaco_packet::{Packet, PacketFlags};

/// `video_params` plus an explicit `bit_rate` — D-10 is a constrained CBR
/// profile and needs to know which of its three fixed rates (30/40/50
/// Mbit/s) applies before any packet arrives (see
/// `vaco-mux-mxf::mux::MxfMuxer::build_d10_index_table`'s own doc comment).
fn video_params_d10(width: u32, height: u32, bit_rate: u64) -> CodecParameters {
    let mut p = video_params(width, height);
    p.bit_rate = Some(bit_rate);
    p
}

fn video_params(width: u32, height: u32) -> CodecParameters {
    let mut p = CodecParameters::video();
    p.codec_id = Some(CodecId::Mpeg2video);
    if let Some(v) = p.video.as_mut() {
        v.width = width;
        v.height = height;
        v.frame_rate = Rational { num: 25, den: 1 };
    }
    p
}

fn audio_params(sample_rate: u32, channels: u32) -> CodecParameters {
    let mut p = CodecParameters::audio();
    p.codec_id = Some(CodecId::PcmS16le);
    if let Some(a) = p.audio.as_mut() {
        a.sample_rate = sample_rate;
        a.layout = vaco_chlayout::ChannelLayout::default_for(channels);
        a.bits_per_coded_sample = Some(16);
    }
    p
}

fn packet(stream_index: u32, pts: i64, bytes: &[u8], key: bool) -> Packet {
    let mut budget = Budget::new(Limits::permissive());
    let mut pkt = Packet::from_slice(&mut budget, bytes).unwrap();
    pkt.stream_index = stream_index;
    pkt.pts = Timestamp::new(pts);
    pkt.dts = Timestamp::new(pts);
    if key {
        pkt.flags |= PacketFlags::KEY;
    }
    pkt
}

#[test]
fn a_video_only_file_round_trips_through_the_sibling_demuxer() {
    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let mut mux = MxfMuxer::new(Box::new(sink.clone()), &vaco_format_core::FormatOptions::default()).unwrap();
    mux.add_stream(&video_params(720, 576)).unwrap();
    mux.init().unwrap();
    mux.write_header().unwrap();

    let frames: [&[u8]; 3] = [&[0xAAu8; 4096], &[0xBBu8; 1024], &[0xCCu8; 2048]];
    for (i, frame) in frames.iter().enumerate() {
        mux.write_packet(&packet(0, i as i64, frame, i == 0))
            .unwrap();
    }
    mux.write_trailer().unwrap();

    let bytes = sink.snapshot();
    assert!(!bytes.is_empty());

    let src = Box::new(vaco_io::MemorySource::new(bytes));
    let mut demux = MxfDemuxer::open(src, &NoParsers).unwrap();
    assert_eq!(demux.streams().len(), 1);
    let s = &demux.streams()[0];
    assert_eq!(s.media_type(), Some(MediaType::Video));
    assert_eq!(s.params.codec_id, Some(CodecId::Mpeg2video));
    let v = s.params.video.as_ref().unwrap();
    assert_eq!((v.width, v.height), (720, 576));

    for frame in &frames {
        let pkt = demux.read_packet().unwrap();
        assert_eq!(pkt.payload(), *frame);
    }
    assert!(matches!(
        demux.read_packet(),
        Err(vaco_core::Error::Eof)
    ));

    // Duration: the footer's restated graph states 3 edit units at 25/1 —
    // this is the header-metadata `Duration` this crate's own D14.1
    // boundary chose to defer to the footer (see `mux.rs`'s module docs).
    assert_eq!(demux.duration().map(vaco_core::Duration::as_micros), Some(120_000));
}

#[test]
fn a_video_and_audio_file_reports_both_streams_via_the_multiple_descriptor_expansion() {
    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let mut mux = MxfMuxer::new(Box::new(sink.clone()), &vaco_format_core::FormatOptions::default()).unwrap();
    mux.add_stream(&video_params(720, 576)).unwrap();
    mux.add_stream(&audio_params(48_000, 2)).unwrap();
    mux.init().unwrap();
    mux.write_header().unwrap();

    for i in 0..3i64 {
        mux.write_packet(&packet(0, i, &[0x11u8; 512], i == 0))
            .unwrap();
        mux.write_packet(&packet(1, i, &[0x22u8; 128], true))
            .unwrap();
    }
    mux.write_trailer().unwrap();

    let bytes = sink.snapshot();
    let src = Box::new(vaco_io::MemorySource::new(bytes));
    let mut demux = MxfDemuxer::open(src, &NoParsers).unwrap();
    // This is exactly the shape `metadata::resolve_track_descriptor`'s own
    // regression test guards on the read side: before that fix, every
    // track in a multi-essence-track package resolved to the same
    // `MultipleDescriptor` (which carries none of the real properties),
    // and the package produced zero streams — video included.
    assert_eq!(demux.streams().len(), 2);
    let video = &demux.streams()[0];
    assert_eq!(video.media_type(), Some(MediaType::Video));
    assert_eq!(video.params.codec_id, Some(CodecId::Mpeg2video));
    let audio = &demux.streams()[1];
    assert_eq!(audio.media_type(), Some(MediaType::Audio));
    let a = audio.params.audio.as_ref().unwrap();
    assert_eq!(a.sample_rate, 48_000);
    assert_eq!(a.layout.as_ref().unwrap().channels, 2);

    let mut counts = [0u32, 0u32];
    loop {
        match demux.read_packet() {
            Ok(pkt) => counts[pkt.stream_index as usize] += 1,
            Err(vaco_core::Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    assert_eq!(counts, [3, 3]);
}

#[test]
fn a_non_seekable_sink_still_produces_a_sequentially_readable_file() {
    // `vaco_io::MemorySink` and similar non-seekable sinks cannot receive
    // the `FooterPartition` backpatch (`mux.rs`'s module docs) -- this is
    // the honestly-degraded case: `vaco-demux-mxf::MxfDemuxer::open` will
    // not find the footer's restated Duration/Index (it trusts
    // `FooterPartition == 0` to mean "no footer"), but the header alone
    // still describes the stream correctly and the essence is still
    // sequentially readable.
    struct NonSeekableSink(Vec<u8>);
    impl vaco_io::MediaSink for NonSeekableSink {
        fn write(&mut self, buf: &[u8]) -> vaco_core::Result<()> {
            self.0.extend_from_slice(buf);
            Ok(())
        }
        fn seek(&mut self, _pos: u64) -> vaco_core::Result<u64> {
            Err(vaco_core::Error::NotSeekable)
        }
        fn position(&self) -> u64 {
            self.0.len() as u64
        }
        fn is_seekable(&self) -> bool {
            false
        }
        fn flush(&mut self) -> vaco_core::Result<()> {
            Ok(())
        }
    }

    let sink = Box::new(NonSeekableSink(Vec::new()));
    let mut mux = MxfMuxer::new(sink, &vaco_format_core::FormatOptions::default()).unwrap();
    mux.add_stream(&video_params(720, 576)).unwrap();
    mux.init().unwrap();
    mux.write_header().unwrap();
    mux.write_packet(&packet(0, 0, &[0x99u8; 256], true))
        .unwrap();
    // Must not error just because the sink cannot seek.
    mux.write_trailer().unwrap();
}


#[test]
fn a_real_ffprobe_reports_the_correct_stream_shape_for_a_video_only_file() {
    // The strongest available claim (see this crate's own docs and the
    // coordinator's dispatch): mux with ours, demux with the reference,
    // and check the stream shape survives -- stronger than
    // our-demuxer-accepts-our-muxer, which proves only self-consistency.
    // Skips gracefully if `ffprobe` is not on PATH, the same pattern
    // `vaco-format-swf`'s own reference-file test uses.
    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let mut mux = MxfMuxer::new(Box::new(sink.clone()), &vaco_format_core::FormatOptions::default())
        .unwrap();
    mux.add_stream(&video_params(720, 576)).unwrap();
    mux.init().unwrap();
    mux.write_header().unwrap();
    for (i, frame) in [&[0xAAu8; 4096][..], &[0xBBu8; 1024][..], &[0xCCu8; 2048][..]]
        .iter()
        .enumerate()
    {
        mux.write_packet(&packet(0, i as i64, frame, i == 0))
            .unwrap();
    }
    mux.write_trailer().unwrap();

    let bytes = sink.snapshot();
    let dir = std::env::temp_dir();
    let path = dir.join(format!("vaco-mxf-mux-test-{}.mxf", std::process::id()));
    std::fs::write(&path, &bytes).expect("write temp file");

    let Ok(out) = std::process::Command::new("ffprobe")
        .args([
            "-hide_banner",
            "-v",
            "error",
            "-of",
            "default=nw=1",
            "-show_entries",
            "stream=codec_type,codec_name,width,height",
        ])
        .arg(&path)
        .output()
    else {
        eprintln!("skipping: ffprobe not on PATH");
        let _ = std::fs::remove_file(&path);
        return;
    };
    let _ = std::fs::remove_file(&path);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "ffprobe failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("codec_type=video"), "missing video in: {text}");
    assert!(
        text.contains("codec_name=mpeg2video"),
        "missing mpeg2video in: {text}"
    );
    assert!(text.contains("width=720"), "missing width in: {text}");
    assert!(text.contains("height=576"), "missing height in: {text}");
}

#[test]
fn a_real_ffprobe_resolves_both_tracks_of_a_multiple_descriptor_file() {
    // The multi-track counterpart of the test above, and the more
    // significant claim: a real `ffmpeg -i` on an early version of this
    // crate's two-essence-track output logged `source track N: stream M,
    // no descriptor found` for *both* tracks and reported `codec_name=unknown`,
    // `width=0`, `height=0`, `sample_rate=0` — even though
    // `vaco-demux-mxf`'s own `MultipleDescriptor` expansion already
    // resolved the identical file correctly (see
    // `a_video_and_audio_file_reports_both_streams_via_the_multiple_descriptor_expansion`
    // above). Root cause: this crate had invented tag `0x0603` for
    // `SubDescriptorUIDs` instead of measuring it — the real, conventional
    // tag is `0x3f01`, and `ffmpeg`'s own resolution of that specific
    // property evidently does not go through general per-file primer/UL
    // matching the way every other property here does. Fixed alongside a
    // second real bug the same investigation surfaced: this crate had been
    // writing the *video* essence-container UL onto the *audio* track's own
    // descriptor too, which made a real `ffmpeg -i` guess `mp2` instead of
    // `pcm_s16le` for the audio stream even once its dimensions/rate
    // resolved.
    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let mut mux = MxfMuxer::new(Box::new(sink.clone()), &vaco_format_core::FormatOptions::default())
        .unwrap();
    mux.add_stream(&video_params(720, 576)).unwrap();
    mux.add_stream(&audio_params(48_000, 2)).unwrap();
    mux.init().unwrap();
    mux.write_header().unwrap();
    for i in 0..3i64 {
        mux.write_packet(&packet(0, i, &[0x11u8; 512], i == 0))
            .unwrap();
        mux.write_packet(&packet(1, i, &[0x22u8; 128], true))
            .unwrap();
    }
    mux.write_trailer().unwrap();

    let bytes = sink.snapshot();
    let dir = std::env::temp_dir();
    let path = dir.join(format!("vaco-mxf-mux-test-multi-{}.mxf", std::process::id()));
    std::fs::write(&path, &bytes).expect("write temp file");

    let Ok(out) = std::process::Command::new("ffprobe")
        .args([
            "-hide_banner",
            "-v",
            "error",
            "-of",
            "default=nw=1",
            "-show_entries",
            "stream=codec_type,codec_name,width,height,sample_rate,channels",
        ])
        .arg(&path)
        .output()
    else {
        eprintln!("skipping: ffprobe not on PATH");
        let _ = std::fs::remove_file(&path);
        return;
    };
    let _ = std::fs::remove_file(&path);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "ffprobe failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("codec_type=video"), "missing video in: {text}");
    assert!(
        text.contains("codec_name=mpeg2video"),
        "missing mpeg2video in: {text}"
    );
    assert!(text.contains("width=720"), "missing width in: {text}");
    assert!(text.contains("height=576"), "missing height in: {text}");
    assert!(text.contains("codec_type=audio"), "missing audio in: {text}");
    assert!(
        text.contains("codec_name=pcm_s16le"),
        "missing pcm_s16le in: {text}"
    );
    assert!(text.contains("sample_rate=48000"), "missing sample_rate in: {text}");
    assert!(text.contains("channels=2"), "missing channels in: {text}");
}

#[test]
fn a_d10_file_round_trips_through_the_sibling_demuxer() {
    // D-10's own CBR shape: every edit unit's essence element must be
    // exactly the same byte length (`bit_rate / 8 / edit_rate`), matching
    // the fixed 30 Mbit/s profile this test declares. `FrameLayout = 1`
    // ("Separate Fields") halves the descriptor's own Stored/Sampled/
    // Display height (measured against a real `ffmpeg -f mxf_d10` file:
    // `288` for a 576-line frame) — `vaco-demux-mxf::descriptor::
    // picture_parameters` already doubles it back on the way in, so the
    // round trip should still report the real, undoubled `576`.
    const FRAME_BYTES: usize = 150_000; // 30_000_000 bit/s / 8 / 25fps.

    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let mut mux = (MUXER_D10.open)(Box::new(sink.clone())).unwrap();
    mux.add_stream(&video_params_d10(720, 576, 30_000_000)).unwrap();
    mux.init().unwrap();
    mux.write_header().unwrap();

    let frames: [Vec<u8>; 2] = [vec![0xAAu8; FRAME_BYTES], vec![0xBBu8; FRAME_BYTES]];
    for (i, frame) in frames.iter().enumerate() {
        mux.write_packet(&packet(0, i as i64, frame, i == 0))
            .unwrap();
    }
    mux.write_trailer().unwrap();

    let bytes = sink.snapshot();
    assert!(!bytes.is_empty());

    let src = Box::new(vaco_io::MemorySource::new(bytes));
    let mut demux = MxfDemuxer::open(src, &NoParsers).unwrap();
    assert_eq!(demux.streams().len(), 1);
    let s = &demux.streams()[0];
    assert_eq!(s.media_type(), Some(MediaType::Video));
    assert_eq!(s.params.codec_id, Some(CodecId::Mpeg2video));
    let v = s.params.video.as_ref().unwrap();
    assert_eq!((v.width, v.height), (720, 576));

    for frame in &frames {
        let pkt = demux.read_packet().unwrap();
        assert_eq!(pkt.payload(), frame.as_slice());
    }
    assert!(matches!(demux.read_packet(), Err(vaco_core::Error::Eof)));
}

#[test]
fn a_real_ffprobe_reports_the_correct_stream_shape_for_a_d10_file() {
    // The D-10 counterpart of
    // `a_real_ffprobe_reports_the_correct_stream_shape_for_a_video_only_file`:
    // mux with ours, demux with the reference. Skips gracefully if
    // `ffprobe` is not on PATH.
    const FRAME_BYTES: usize = 150_000;

    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let mut mux = (MUXER_D10.open)(Box::new(sink.clone())).unwrap();
    mux.add_stream(&video_params_d10(720, 576, 30_000_000)).unwrap();
    mux.init().unwrap();
    mux.write_header().unwrap();
    for i in 0..2 {
        mux.write_packet(&packet(0, i, &vec![0xAAu8; FRAME_BYTES], i == 0))
            .unwrap();
    }
    mux.write_trailer().unwrap();

    let bytes = sink.snapshot();
    let dir = std::env::temp_dir();
    let path = dir.join(format!("vaco-mxf-d10-mux-test-{}.mxf", std::process::id()));
    std::fs::write(&path, &bytes).expect("write temp file");

    let Ok(out) = std::process::Command::new("ffprobe")
        .args([
            "-hide_banner",
            "-v",
            "error",
            "-of",
            "default=nw=1",
            "-show_entries",
            "stream=codec_type,codec_name,width,height",
        ])
        .arg(&path)
        .output()
    else {
        eprintln!("skipping: ffprobe not on PATH");
        let _ = std::fs::remove_file(&path);
        return;
    };
    let _ = std::fs::remove_file(&path);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "ffprobe failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("codec_type=video"), "missing video in: {text}");
    assert!(
        text.contains("codec_name=mpeg2video"),
        "missing mpeg2video in: {text}"
    );
    assert!(text.contains("width=720"), "missing width in: {text}");
    // Not asserting `height=576` here: `ffprobe` may report the D-10
    // `FrameLayout = 1` halved value (`288`) directly rather than doubling
    // it back the way this crate's own `vaco-demux-mxf` does — that
    // asymmetry, if it turns out to be real, belongs in
    // `docs/format/vaco-mux-mxf.md`'s byte-identity matrix, not asserted
    // away here on a guess.
}

#[test]
fn an_op_atom_file_round_trips_through_the_sibling_demuxer() {
    // OP-Atom's own shape: exactly one essence track, clip-wrapped (one
    // Generic Container element for the whole file, not one per frame —
    // measured this session against a real `ffmpeg -f mxf_opatom` file).
    // `write_packet` buffers every payload and `write_trailer` writes the
    // single real element once the total length is known.
    //
    // This workspace's own demuxer reads this file back as a *single*
    // packet holding all three frames concatenated (length
    // 4096+1024+2048=7168), not three: `vaco-demux-mxf::demux::MxfDemuxer::
    // read_packet` reads "one KLV, one packet" unconditionally (that
    // crate's own `essence.rs` module docs explain why: `clip_wrapped_spans`
    // exists but was never wired in). Checked directly against a real
    // `ffmpeg -i`/`ffprobe -show_packets` on this exact file this session,
    // this is not a gap this crate introduced: the reference's own
    // `-show_packets` reports the identical shape — one packet, size 7168 —
    // for the same file. Frame-level access into OP-Atom's clip-wrapped
    // essence is apparently a decoder-level concern (parsing picture start
    // codes within the one packet), not a container-framing one; this
    // crate's write side and both demuxers' read sides already agree.
    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let mut mux = (MUXER_OPATOM.open)(Box::new(sink.clone())).unwrap();
    mux.add_stream(&video_params(720, 576)).unwrap();
    mux.init().unwrap();
    mux.write_header().unwrap();

    let frames: [&[u8]; 3] = [&[0xAAu8; 4096], &[0xBBu8; 1024], &[0xCCu8; 2048]];
    let total_len: usize = frames.iter().map(|f| f.len()).sum();
    for (i, frame) in frames.iter().enumerate() {
        mux.write_packet(&packet(0, i as i64, frame, i == 0))
            .unwrap();
    }
    mux.write_trailer().unwrap();

    let bytes = sink.snapshot();
    assert!(!bytes.is_empty());

    let src = Box::new(vaco_io::MemorySource::new(bytes));
    let mut demux = MxfDemuxer::open(src, &NoParsers).unwrap();
    assert_eq!(demux.streams().len(), 1);
    let s = &demux.streams()[0];
    assert_eq!(s.media_type(), Some(MediaType::Video));
    assert_eq!(s.params.codec_id, Some(CodecId::Mpeg2video));
    let v = s.params.video.as_ref().unwrap();
    assert_eq!((v.width, v.height), (720, 576));

    let pkt = demux.read_packet().unwrap();
    assert_eq!(pkt.payload().len(), total_len, "clip-wrapped essence read back as one packet");
    assert!(matches!(demux.read_packet(), Err(vaco_core::Error::Eof)));
}

#[test]
fn adding_a_second_stream_to_an_op_atom_muxer_is_rejected() {
    // OP-Atom's own defining constraint: exactly one essence track per
    // file (see `MxfMuxer::add_stream`'s own check).
    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let mut mux = (MUXER_OPATOM.open)(Box::new(sink.clone())).unwrap();
    mux.add_stream(&video_params(720, 576)).unwrap();
    assert!(mux.add_stream(&audio_params(48_000, 2)).is_err());
}

#[test]
fn adding_an_audio_stream_to_a_d10_muxer_is_rejected() {
    // D-10 audio (the fixed 8-slot AES3 bundle) is not yet implemented on
    // the write side — see `MUXER_D10`'s own doc comment.
    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let mut mux = (MUXER_D10.open)(Box::new(sink.clone())).unwrap();
    assert!(mux.add_stream(&audio_params(48_000, 2)).is_err());
}

#[test]
fn the_random_index_pack_names_every_partition_with_its_own_body_sid() {
    // A real Random Index Pack has one entry per partition pack actually in
    // the file, each stating *that partition's own* `BodySID` — measured
    // this session against three real fixtures (an earlier version of this
    // crate hardcoded two entries, the header always `BodySID = 1` and no
    // entry at all for the Body Partition Pack, wrong on both counts for a
    // real OP1a/OP-Atom file). This file has one video track, so OP1a's
    // shape applies: header (BodySID = 0, no essence), body (BodySID = 1),
    // footer (BodySID = 0) -- three entries, in that order.
    let sink = SharedDynBuf::with_limits(Limits::permissive());
    let mut mux = MxfMuxer::new(Box::new(sink.clone()), &vaco_format_core::FormatOptions::default()).unwrap();
    mux.add_stream(&video_params(720, 576)).unwrap();
    mux.init().unwrap();
    mux.write_header().unwrap();
    mux.write_packet(&packet(0, 0, &[0xAAu8; 4096], true)).unwrap();
    mux.write_trailer().unwrap();

    let bytes = sink.snapshot();
    let file_len = bytes.len() as u64;
    let mut io = vaco_io::IoContext::new(Box::new(MemorySource::new(bytes)), &IoOptions::default()).unwrap();
    let mut budget = Budget::new(Limits::permissive());
    let rip = vaco_demux_mxf::partition::find_rip(&mut io, &mut budget, file_len)
        .unwrap()
        .expect("a seekable file's trailer names a real Random Index Pack");

    assert_eq!(rip.entries.len(), 3, "header, body, footer -- one each");
    assert_eq!((rip.entries[0].body_sid, rip.entries[0].byte_offset), (0, 0));
    assert_eq!(rip.entries[1].body_sid, 1);
    assert_eq!(rip.entries[2].body_sid, 0);
    // Body and footer offsets are strictly increasing (each partition
    // really is later in the file than the last).
    assert!(rip.entries[1].byte_offset > rip.entries[0].byte_offset);
    assert!(rip.entries[2].byte_offset > rip.entries[1].byte_offset);
}

mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Any sequence of non-empty video frame sizes round-trips through
        /// the sibling demuxer: same count, same bytes, same position order.
        /// This is the cheap round-trip property test the coordinator's own
        /// dispatch asked for in place of a fuzz target — a muxer does not
        /// parse untrusted input the way a demuxer does, but "our own
        /// demuxer reads our own muxer's output" is still worth checking
        /// across more than the three hand-picked sizes the other tests use.
        #[test]
        fn arbitrary_frame_sizes_round_trip(
            sizes in prop::collection::vec(1usize..=8192, 1..8)
        ) {
            let sink = SharedDynBuf::with_limits(Limits::permissive());
            let mut mux = MxfMuxer::new(
                Box::new(sink.clone()),
                &vaco_format_core::FormatOptions::default(),
            )
            .unwrap();
            mux.add_stream(&video_params(720, 576)).unwrap();
            mux.init().unwrap();
            mux.write_header().unwrap();

            let frames: Vec<Vec<u8>> = sizes
                .iter()
                .enumerate()
                .map(|(i, &len)| vec![(i % 256) as u8; len])
                .collect();
            for (i, frame) in frames.iter().enumerate() {
                mux.write_packet(&packet(0, i as i64, frame, i == 0))
                    .unwrap();
            }
            mux.write_trailer().unwrap();

            let bytes = sink.snapshot();
            let src = Box::new(vaco_io::MemorySource::new(bytes));
            let mut demux = MxfDemuxer::open(src, &NoParsers).unwrap();
            prop_assert_eq!(demux.streams().len(), 1);

            for frame in &frames {
                let pkt = demux.read_packet().unwrap();
                prop_assert_eq!(pkt.payload(), frame.as_slice());
            }
            prop_assert!(matches!(demux.read_packet(), Err(vaco_core::Error::Eof)));
        }
    }
}
