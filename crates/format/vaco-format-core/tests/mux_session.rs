//! The muxer state machine, end to end over the worked-example container.
//!
//! `roundtrip.rs` drives `VacoRawMuxer` directly, which is what a caller had to
//! do before [`MuxBuilder`] existed and what a caller may still do. These cases
//! drive it through the session instead, and assert the things the session is
//! *for*: that the phases run in order, that the M-chain reaches a real muxer
//! rather than a mock, and that a file written through it reads back as what
//! went in.
//!
//! The ordering guarantees themselves are not tested here, because they are not
//! testable: `MuxBuilder` has no `write_packet` and `MuxWriter` has no
//! `add_stream`, so the illegal sequences have no spelling that compiles. What
//! is left to check is everything the types cannot say.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::cast_possible_wrap,
    clippy::field_reassign_with_default,
    reason = "test code"
)]

use std::sync::Arc;

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Duration, Rational, Timestamp};
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::mux::{MuxBuilder, NoBsfs};
use vaco_format_core::vacoraw::{MemorySink, VacoRawDemuxer, VacoRawMuxer};
use vaco_format_core::{AvoidNegativeTs, Demuxer, FormatOptions};
use vaco_io::MemorySource;
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

fn packet(stream: u32, dts: i64, payload: &[u8]) -> Packet {
    let mut budget = Budget::new(Limits::strict());
    let mut p = Packet::from_slice(&mut budget, payload).unwrap();
    p.stream_index = stream;
    p.dts = Timestamp::new(dts);
    p.pts = p.dts;
    p.flags = PacketFlags::KEY;
    p
}

fn video() -> CodecParameters {
    CodecParameters::video().with_codec(CodecId::H264)
}

fn audio() -> CodecParameters {
    CodecParameters::audio().with_codec(CodecId::Opus)
}

/// Mux `specs` through a session and return the bytes plus the report.
fn mux(
    opts: &FormatOptions,
    streams: &[CodecParameters],
    input_tb: Rational,
    specs: &[(u32, i64, &[u8])],
) -> (Vec<u8>, vaco_format_core::MuxReport) {
    let sink = MemorySink::new();
    let written = sink.shared();
    let muxer = VacoRawMuxer::new(Box::new(sink), opts).unwrap();
    let mut builder = MuxBuilder::new(Box::new(muxer), opts).with_bsfs(Arc::new(NoBsfs));
    for p in streams {
        builder.add_stream(p, input_tb).unwrap();
    }
    let mut writer = builder.open().unwrap();
    for &(s, dts, payload) in specs {
        writer.write_packet(packet(s, dts, payload)).unwrap();
    }
    let report = writer.finish().unwrap();
    (written.snapshot(), report)
}

#[test]
fn a_session_written_file_demuxes_back_to_what_went_in() {
    let opts = FormatOptions::default();
    let tb = Rational::new(1, 1000);
    let specs: &[(u32, i64, &[u8])] = &[
        (0, 0, b"v0"),
        (1, 10, b"a0"),
        (0, 40, b"v1"),
        (1, 30, b"a1"),
        (0, 80, b"v2"),
    ];
    let (bytes, report) = mux(&opts, &[video(), audio()], tb, specs);
    assert_eq!(report.packets, specs.len() as u64);
    assert_eq!(report.per_stream_packets, vec![3, 2]);
    assert!(report.trailer_written);

    let mut demux =
        VacoRawDemuxer::open(Box::new(MemorySource::new(bytes)), &NoParsers, &opts).unwrap();
    assert_eq!(demux.streams().len(), 2);
    let mut seen = Vec::new();
    while let Ok(p) = demux.read_packet() {
        seen.push((p.stream_index, p.payload().to_vec()));
    }
    assert_eq!(seen.len(), specs.len());
    // The queue orders between streams by DTS, so the file is in DTS order and
    // not in call order.
    let payloads: Vec<&[u8]> = seen.iter().map(|(_, p)| p.as_slice()).collect();
    assert_eq!(payloads, vec![&b"v0"[..], b"a0", b"a1", b"v1", b"v2"]);
}

#[test]
fn the_output_time_base_is_the_muxers_and_the_rescale_happens() {
    // `vacoraw` derives a video stream's base from its frame rate, so the
    // container — not the caller — decides, which is exactly the M1/M12 case.
    let opts = FormatOptions::default();
    let mut params = video();
    if let Some(v) = params.video.as_mut() {
        v.frame_rate = Rational::new(25, 1);
    }
    let sink = MemorySink::new();
    let written = sink.shared();
    let muxer = VacoRawMuxer::new(Box::new(sink), &opts).unwrap();
    let mut builder = MuxBuilder::new(Box::new(muxer), &opts);
    // Packets arrive in milliseconds.
    builder.add_stream(&params, Rational::new(1, 1000)).unwrap();
    let mut writer = builder.open().unwrap();
    assert_eq!(writer.output_time_base(0), Some(Rational::new(1, 25)));
    for i in 0..3i64 {
        writer.write_packet(packet(0, i * 40, b"f")).unwrap();
    }
    writer.finish().unwrap();

    let mut demux = VacoRawDemuxer::open(
        Box::new(MemorySource::new(written.snapshot())),
        &NoParsers,
        &opts,
    )
    .unwrap();
    // 0, 40 and 80 ms are frames 0, 1 and 2 at 25 fps.
    let ticks: Vec<Option<i64>> = (0..3)
        .map(|_| demux.read_packet().unwrap().dts.ticks())
        .collect();
    assert_eq!(ticks, vec![Some(0), Some(1), Some(2)]);
}

#[test]
fn avoid_negative_ts_shifts_the_whole_file_once() {
    let opts = FormatOptions::default();
    let tb = Rational::new(1, 1000);
    let specs: &[(u32, i64, &[u8])] = &[(0, -250, b"v"), (1, -100, b"a"), (0, 0, b"v2")];
    let (bytes, report) = mux(&opts, &[video(), audio()], tb, specs);
    assert_eq!(report.avoid_negative_ts, AvoidNegativeTs::MakeNonNegative);
    assert_eq!(report.ts_offset_us, Some(250_000));

    let mut demux =
        VacoRawDemuxer::open(Box::new(MemorySource::new(bytes)), &NoParsers, &opts).unwrap();
    let mut dts = Vec::new();
    while let Ok(p) = demux.read_packet() {
        dts.push(p.dts.ticks());
    }
    // Every stream moved by the same 250 ms — a per-stream shift would have put
    // the audio packet at 0 too, and desynchronised the file. The values are in
    // microseconds because `vacoraw` chose that base for a stream with no frame
    // rate, which is M12 working: the container decides, not the caller.
    assert_eq!(dts, vec![Some(0), Some(150_000), Some(250_000)]);
}

#[test]
fn output_ts_offset_composes_with_the_shift() {
    let mut opts = FormatOptions::default();
    opts.output_ts_offset = Duration::from_micros(1_000_000);
    let tb = Rational::new(1, 1000);
    let (bytes, report) = mux(&opts, &[video()], tb, &[(0, -250, b"v")]);
    // M2 moved the packet to +750 ms, so M3 has nothing left to do.
    assert_eq!(report.ts_offset_us, Some(0));
    let mut demux =
        VacoRawDemuxer::open(Box::new(MemorySource::new(bytes)), &NoParsers, &opts).unwrap();
    assert_eq!(demux.read_packet().unwrap().dts.ticks(), Some(750_000));
}

#[test]
fn a_non_monotonic_stream_is_refused_rather_than_repaired() {
    let opts = FormatOptions::default();
    let sink = MemorySink::new();
    let muxer = VacoRawMuxer::new(Box::new(sink), &opts).unwrap();
    let mut builder = MuxBuilder::new(Box::new(muxer), &opts);
    builder
        .add_stream(&video(), Rational::new(1, 1000))
        .unwrap();
    let mut writer = builder.open().unwrap();
    writer.write_packet(packet(0, 10, b"a")).unwrap();
    assert!(
        writer.write_packet(packet(0, 10, b"b")).is_err(),
        "an equal DTS must be refused by a container that requires strictly increasing"
    );
    // The session is still usable: the packet never entered the queue.
    writer.write_packet(packet(0, 20, b"c")).unwrap();
    let report = writer.finish().unwrap();
    assert_eq!(report.packets, 2);
}

#[test]
fn aborting_leaves_no_trailer_and_hands_the_muxer_back() {
    let opts = FormatOptions::default();
    let sink = MemorySink::new();
    let written = sink.shared();
    let muxer = VacoRawMuxer::new(Box::new(sink), &opts).unwrap();
    let mut builder = MuxBuilder::new(Box::new(muxer), &opts);
    builder
        .add_stream(&video(), Rational::new(1, 1000))
        .unwrap();
    let mut writer = builder.open().unwrap();
    writer.write_packet(packet(0, 0, b"v")).unwrap();
    let (mut muxer, report) = writer.abort();
    assert!(!report.trailer_written);

    // The caller still owns the muxer and may finalise by hand if it wants to.
    // Before the abort there is no index; after it there is.
    let before = written.snapshot().len();
    muxer.write_trailer().unwrap();
    assert!(written.snapshot().len() > before);
}

#[test]
fn a_sparse_stream_does_not_stall_the_session() {
    let mut opts = FormatOptions::default();
    opts.max_interleave_delta = 1_000_000;
    let sink = MemorySink::new();
    let muxer = VacoRawMuxer::new(Box::new(sink), &opts).unwrap();
    let mut builder = MuxBuilder::new(Box::new(muxer), &opts);
    let tb = Rational::new(1, 1000);
    builder.add_stream(&video(), tb).unwrap();
    builder.add_stream(&audio(), tb).unwrap();
    let mut writer = builder.open().unwrap();
    // Stream 1 is a subtitle-shaped track that never speaks. Once the spread
    // passes the threshold, video flows anyway.
    for i in 0..10i64 {
        writer.write_packet(packet(0, i * 500, b"v")).unwrap();
    }
    assert!(
        writer.report().packets > 0,
        "the sparse escape never fired: the file would be empty until EOF"
    );
    writer.finish().unwrap();
}

#[test]
fn max_streams_of_zero_makes_the_session_unopenable() {
    let mut opts = FormatOptions::default();
    opts.max_streams = 0;
    let sink = MemorySink::new();
    let muxer = VacoRawMuxer::new(Box::new(sink), &opts).unwrap();
    let mut builder = MuxBuilder::new(Box::new(muxer), &opts);
    assert!(
        builder
            .add_stream(&video(), Rational::new(1, 1000))
            .is_err(),
        "max_streams must bound the mux side too; it is a fuzzing bound"
    );
    // And a container that requires streams then refuses to open at all.
    assert!(builder.open().is_err());
}

#[test]
fn the_bytes_are_the_same_whether_the_session_or_the_muxer_drives() {
    // The session must not be a *different* muxer. Given the same packets in
    // the same order, it writes the same file the direct path does — which is
    // what makes adopting it a no-op for an existing caller.
    use vaco_format_core::Muxer;

    let opts = FormatOptions::default();
    // Declared in the base `vacoraw` picks for a stream with no frame rate, so
    // M1 is the identity and the two paths are comparable at all. Declare a
    // different one and the session writes a *better* file than the direct
    // path: it rescales into the base the container actually recorded in the
    // header, where the direct path writes the caller's ticks under the
    // container's base and quietly means something else.
    let tb = Rational::new(1, 1_000_000);
    let specs: &[(u32, i64, &[u8])] = &[(0, 0, b"v0"), (0, 40, b"v1"), (0, 80, b"v2")];
    let (via_session, _) = mux(&opts, &[video()], tb, specs);

    let sink = MemorySink::new();
    let written = sink.shared();
    let mut muxer = VacoRawMuxer::new(Box::new(sink), &opts).unwrap();
    muxer.add_stream(&video()).unwrap();
    muxer.write_header().unwrap();
    for &(s, dts, payload) in specs {
        muxer.write_packet(&packet(s, dts, payload)).unwrap();
    }
    muxer.write_trailer().unwrap();
    assert_eq!(via_session, written.snapshot());
}
