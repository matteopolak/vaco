//! End-to-end tests over the worked-example container.
//!
//! The point of these is not `vacoraw`. It is that the frozen [`Demuxer`] and
//! [`Muxer`] traits, the probe engine, the seek strategies, the interleave queue
//! and the discovery wrapper all compose into something that reads and writes a
//! real file — because until one format does that end to end, the interface is
//! a design and not an implementation.
//!
//! Everything here is a *named case*: a specific file, a specific seek, a
//! specific truncation, chosen because it pins a rule down. The generated
//! cases live next door in `properties.rs`, which drives the same fixtures
//! through `proptest`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::cast_possible_wrap,
    reason = "test code"
)]

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, Timestamp};
use vaco_format_core::discovery::{Discovery, NoParsers};
use vaco_format_core::probe::{Probe, ProbeData, ProbeScore};
use vaco_format_core::seek::{SeekFlags, SeekTarget};
use vaco_format_core::vacoraw::{self, ForwardOnlySink, MemorySink, VacoRawDemuxer, VacoRawMuxer};
use vaco_format_core::{Demuxer, DemuxerDesc, FormatOptions, InterleaveQueue, Muxer};
use vaco_io::{IoContext, IoOptions, MemorySource};
use vaco_limits::{Budget, Limits};
use vaco_packet::{Packet, PacketFlags};

/// One packet's worth of intent, so the expectation and the input come from the
/// same description.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Spec {
    stream: u32,
    dts: i64,
    key: bool,
    payload: Vec<u8>,
}

fn build(specs: &[Spec], streams: usize, seekable: bool) -> Vec<u8> {
    let opts = FormatOptions::default();
    if seekable {
        let sink = MemorySink::new();
        let bytes = sink.shared();
        let mut mux = VacoRawMuxer::new(Box::new(sink), &opts).unwrap();
        write_all(&mut mux, specs, streams, &opts);
        bytes.snapshot()
    } else {
        let sink = ForwardOnlySink::new();
        let bytes = sink.shared();
        let mut mux = VacoRawMuxer::new(Box::new(sink), &opts).unwrap();
        write_all(&mut mux, specs, streams, &opts);
        bytes.snapshot()
    }
}

/// Add the streams, push everything through the interleave queue, close up.
///
/// The queue is in the path deliberately: it is how a real caller drives a
/// muxer, so the round-trip test covers the ordering rules too.
fn write_all(mux: &mut VacoRawMuxer, specs: &[Spec], streams: usize, opts: &FormatOptions) {
    let mut budget = Budget::new(Limits::permissive());
    for i in 0..streams {
        let params = if i == 0 {
            CodecParameters::video().with_codec(CodecId::H264)
        } else {
            CodecParameters::audio().with_codec(CodecId::Opus)
        };
        mux.add_stream(&params).unwrap();
    }
    mux.write_header().unwrap();
    let mut queue = InterleaveQueue::new(streams.max(1), opts);
    for i in 0..streams {
        queue.set_time_base(i as u32, vaco_format_core::time::TIME_BASE_Q);
    }
    for s in specs {
        let mut p = Packet::from_slice(&mut budget, &s.payload).unwrap();
        p.stream_index = s.stream;
        p.dts = Timestamp::new(s.dts);
        p.pts = Timestamp::new(s.dts);
        if s.key {
            p.flags = PacketFlags::KEY;
        }
        queue.push(p).unwrap();
        while let Some(out) = queue.next(false) {
            mux.write_packet(&out).unwrap();
        }
    }
    for i in 0..streams {
        queue.end_stream(i as u32);
    }
    for out in queue.drain() {
        mux.write_packet(&out).unwrap();
    }
    mux.write_trailer().unwrap();
}

fn open(bytes: Vec<u8>, seekable: bool) -> VacoRawDemuxer {
    let src: Box<dyn vaco_io::MediaSource> = if seekable {
        Box::new(MemorySource::new(bytes))
    } else {
        Box::new(MemorySource::forward_only(bytes))
    };
    VacoRawDemuxer::open(src, &NoParsers, &FormatOptions::default()).unwrap()
}

fn drain(d: &mut impl Demuxer) -> Vec<Spec> {
    let mut out = Vec::new();
    loop {
        match d.read_packet() {
            Ok(p) => out.push(Spec {
                stream: p.stream_index,
                dts: p.dts.ticks().unwrap_or(i64::MIN),
                key: p.is_key(),
                payload: p.payload().to_vec(),
            }),
            Err(Error::Eof) => break,
            Err(e) => panic!("read failed: {e}"),
        }
    }
    out
}

fn simple_specs(n: usize, streams: u32) -> Vec<Spec> {
    (0..n)
        .map(|i| Spec {
            stream: (i as u32) % streams,
            dts: (i as i64) * 1000,
            key: i % 5 == 0,
            payload: vec![(i % 251) as u8; 4 + i % 17],
        })
        .collect()
}

// ------------------------------------------------------------------ probing

#[test]
fn probe_recognises_its_own_output_and_nothing_else() {
    let bytes = build(&simple_specs(10, 1), 1, true);
    let opts = FormatOptions::default();
    let cands: &[&DemuxerDesc] = &[&vacoraw::DEMUXER];
    let p = Probe::new(cands, &opts);
    let d = p.best(&ProbeData::new(&bytes)).unwrap();
    assert_eq!(d.desc.name, "vacoraw");
    assert_eq!(d.score, ProbeScore::MAGIC_CHECKED);

    // One byte of the magic changed and it is not ours any more.
    let mut broken = bytes.clone();
    broken[3] ^= 0xff;
    assert!(p.best(&ProbeData::new(&broken)).is_none());
}

#[test]
fn probe_detects_through_a_live_source_and_leaves_the_position_alone() {
    let bytes = build(&simple_specs(40, 2), 2, true);
    let opts = FormatOptions::default();
    let cands: &[&DemuxerDesc] = &[&vacoraw::DEMUXER];
    let mut io = IoContext::new(
        Box::new(MemorySource::forward_only(bytes.clone())),
        &IoOptions::default(),
    )
    .unwrap();
    let d = Probe::new(cands, &opts)
        .detect(&mut io, Some("clip.vacoraw"), None)
        .unwrap();
    assert_eq!(d.desc.name, "vacoraw");
    assert_eq!(io.pos(), 0, "detection must not consume anything");
}

#[test]
fn a_short_prefix_still_probes_without_panicking() {
    let bytes = build(&simple_specs(4, 1), 1, true);
    let opts = FormatOptions::default();
    let cands: &[&DemuxerDesc] = &[&vacoraw::DEMUXER];
    let p = Probe::new(cands, &opts);
    for n in 0..bytes.len().min(64) {
        // Total on every prefix; the padding rule is what makes this work at
        // n = 8, where the stream count is not there yet.
        let _ = p.best(&ProbeData::new(&bytes[..n]));
    }
    assert_eq!(
        p.best(&ProbeData::new(&bytes[..12])).map(|d| d.score),
        Some(ProbeScore::MAGIC),
        "magic without a confirmable stream table is 90, not 100"
    );
}

// ---------------------------------------------------------------- round trip

#[test]
fn packets_survive_a_mux_demux_round_trip() {
    for seekable in [true, false] {
        let specs = simple_specs(64, 2);
        let bytes = build(&specs, 2, seekable);
        let mut d = open(bytes, true);
        let got = drain(&mut d);
        // The interleave queue reorders between streams, so compare as
        // multisets of the same length, plus per-stream order.
        assert_eq!(got.len(), specs.len(), "seekable = {seekable}");
        for stream in 0..2u32 {
            let want: Vec<_> = specs.iter().filter(|s| s.stream == stream).collect();
            let have: Vec<_> = got.iter().filter(|s| s.stream == stream).collect();
            assert_eq!(want, have, "stream {stream}, seekable = {seekable}");
        }
    }
}

#[test]
fn an_index_is_written_only_when_the_sink_can_seek() {
    let specs = simple_specs(40, 1);
    let indexed = open(build(&specs, 1, true), true);
    assert!(
        !indexed.index().is_empty(),
        "a seekable sink must leave an index behind"
    );
    let plain = open(build(&specs, 1, false), true);
    assert!(
        plain.index().is_empty(),
        "a pipe cannot patch the header, so no index may be claimed"
    );
}

#[test]
fn a_file_from_a_pipe_is_still_readable() {
    let specs = simple_specs(30, 1);
    let bytes = build(&specs, 1, false);
    let mut d = open(bytes, false);
    assert_eq!(drain(&mut d).len(), specs.len());
}

// -------------------------------------------------------------------- seeking

#[test]
fn indexed_seek_lands_on_a_keyframe_at_or_before_the_target() {
    let specs = simple_specs(200, 1);
    let bytes = build(&specs, 1, true);
    for want in [0i64, 1, 4999, 5000, 5001, 100_000, 199_000] {
        let mut d = open(bytes.clone(), true);
        let r = d.seek(
            SeekTarget::Timestamp {
                stream_index: 0,
                ts: Timestamp::new(want),
            },
            SeekFlags::BACKWARD,
        );
        if r.is_err() {
            // Only legitimate before the first keyframe.
            assert!(want < 0, "unexpected seek failure at {want}");
            continue;
        }
        let p = d.read_packet().unwrap();
        assert!(p.is_key(), "landed on a non-keyframe seeking to {want}");
        let landed = p.dts.ticks().unwrap();
        assert!(landed <= want, "overshot: wanted {want}, landed {landed}");
        assert!(
            want - landed < 5000,
            "landed too early: wanted {want}, landed {landed}"
        );
    }
}

#[test]
fn bisection_seek_works_without_an_index() {
    // Written to a pipe, so the file carries no index; read from a seekable
    // source, so the demuxer can bisect. This is the Matroska-without-Cues and
    // the MPEG-TS case.
    let specs: Vec<Spec> = (0..4000)
        .map(|i| Spec {
            stream: 0,
            dts: i64::from(i) * 100,
            key: i % 4 == 0,
            payload: vec![0u8; 64],
        })
        .collect();
    let bytes = build(&specs, 1, false);
    assert!(
        bytes.len() > 64 * 1024,
        "the fixture must exceed MIN_SEEK_STEP"
    );
    let mut d = open(bytes, true);
    assert!(d.index().is_empty());
    let want = 300_000i64;
    d.seek(
        SeekTarget::Timestamp {
            stream_index: 0,
            ts: Timestamp::new(want),
        },
        SeekFlags::BACKWARD,
    )
    .unwrap();
    let landed = d.read_packet().unwrap().dts.ticks().unwrap();
    assert!(landed <= want, "overshot: {landed} > {want}");
    assert!(!d.index().is_empty(), "the bisection must leave an index");
}

#[test]
fn byte_seek_resynchronises_on_the_packet_magic() {
    let specs = simple_specs(100, 1);
    let bytes = build(&specs, 1, true);
    let mut d = open(bytes, true);
    // Land in the middle of a packet payload; the demuxer must find the next
    // header rather than reading garbage.
    d.seek(SeekTarget::Byte(500), SeekFlags::empty()).unwrap();
    let p = d.read_packet().unwrap();
    assert!(p.dts.ticks().unwrap() >= 0);
    assert!(p.pos.unwrap() >= 500);
}

#[test]
fn seeking_a_pipe_is_refused_rather_than_faked() {
    let bytes = build(&simple_specs(20, 1), 1, false);
    let mut d = open(bytes, false);
    let r = d.seek(
        SeekTarget::Timestamp {
            stream_index: 0,
            ts: Timestamp::new(1000),
        },
        SeekFlags::empty(),
    );
    assert!(matches!(r, Err(Error::NotSeekable)));
}

#[test]
fn end_of_stream_is_stable() {
    // `read_packet` consumes bytes before it can tell whether a packet
    // follows, so a demuxer that does not latch EOF reports the middle of its
    // own trailer as corruption on the second call. `Discovery` relies on this
    // being stable, and so does every scheduler that drains twice.
    let bytes = build(&simple_specs(12, 1), 1, true);
    let mut d = open(bytes, true);
    let _ = drain(&mut d);
    for _ in 0..5 {
        assert!(matches!(d.read_packet(), Err(Error::Eof)));
    }
    // And a seek clears it.
    d.seek(
        SeekTarget::Timestamp {
            stream_index: 0,
            ts: Timestamp::new(0),
        },
        SeekFlags::BACKWARD,
    )
    .unwrap();
    assert!(d.read_packet().is_ok());
}

// ------------------------------------------------------------------ discovery

#[test]
fn discovery_replays_and_reports() {
    let specs = simple_specs(50, 1);
    let bytes = build(&specs, 1, true);
    let inner = open(bytes, true);
    let opts = FormatOptions::default();
    let mut d = Discovery::new(inner, vacoraw::FLAGS, &opts);
    d.run(&NoParsers).unwrap();
    assert!(d.report().packets_read > 0);
    assert_eq!(d.report().start_time.unwrap().as_micros(), 0);
    assert_eq!(drain(&mut d).len(), specs.len());
}

// ------------------------------------------------------------------ robustness

#[test]
fn every_truncation_of_a_valid_file_is_handled() {
    let bytes = build(&simple_specs(30, 2), 2, true);
    for n in 0..bytes.len() {
        let prefix = bytes[..n].to_vec();
        let src: Box<dyn vaco_io::MediaSource> = Box::new(MemorySource::new(prefix));
        if let Ok(mut d) = VacoRawDemuxer::open(src, &NoParsers, &FormatOptions::default()) {
            // Reading must terminate, whatever the truncation did.
            let mut steps = 0;
            while d.read_packet().is_ok() {
                steps += 1;
                assert!(steps < 10_000, "read loop did not terminate at n = {n}");
            }
        }
    }
}
