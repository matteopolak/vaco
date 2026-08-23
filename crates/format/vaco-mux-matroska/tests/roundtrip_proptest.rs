//! Mux, then demux with `vaco-demux-matroska`, and check what came back.
//!
//! This is the property this crate cares about most: a file this muxer
//! writes must be exactly the file its sibling demuxer already has 76 tests
//! insisting it can read. Unit tests check the wire shape by hand; this
//! exercises the whole round trip over arbitrary packet timing instead.

#![allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]

use proptest::prelude::*;
use vaco_codec_core::{CodecId, CodecParameters, VideoParameters};
use vaco_core::{Rational, Timestamp};
use vaco_format_core::discovery::NoParsers;
use vaco_format_core::vacoraw::MemorySink;
use vaco_format_core::{Demuxer, FormatOptions, Muxer};
use vaco_io::MemorySource;
use vaco_limits::{Budget, Limits};
use vaco_mux_matroska::MatroskaMuxer;
use vaco_packet::{Packet, PacketFlags};

fn video_params() -> CodecParameters {
    let mut p = CodecParameters::video().with_codec(CodecId::H264);
    p.video = Some(VideoParameters {
        width: 16,
        height: 16,
        frame_rate: Rational::new(25, 1),
        ..VideoParameters::default()
    });
    p.extradata = Some(vec![0xAA, 0xBB]);
    p
}

fn pkt(stream: u32, pts_ms: i64, key: bool, payload: &[u8]) -> Packet {
    let mut budget = Budget::new(Limits::strict());
    let mut p = Packet::from_slice(&mut budget, payload).unwrap();
    p.stream_index = stream;
    p.pts = Timestamp::new(pts_ms);
    p.dts = p.pts;
    if key {
        p.flags = PacketFlags::KEY;
    }
    p
}

proptest! {
    /// Every payload, in order, survives a mux/demux round trip on a
    /// single video track with strictly increasing millisecond timestamps.
    #[test]
    fn payload_order_and_content_survive_a_round_trip(
        // Millisecond deltas between consecutive frames; kept small and
        // positive so a cluster boundary or the i16 relative-timestamp cap
        // does not have to be reasoned about frame by frame — that path has
        // its own unit tests in `mux::tests`.
        deltas in prop::collection::vec(1i64..40, 1..12),
    ) {
        let sink = MemorySink::new();
        let shared = sink.shared();
        let mut mux = MatroskaMuxer::new_matroska(Box::new(sink), &FormatOptions::default()).unwrap();
        let idx = mux.add_stream(&video_params()).unwrap();
        mux.write_header().unwrap();

        let mut ts = 0i64;
        let mut payloads = Vec::new();
        for (i, d) in deltas.iter().enumerate() {
            let payload = vec![i as u8; 3];
            mux.write_packet(&pkt(idx, ts, i == 0, &payload)).unwrap();
            payloads.push(payload);
            ts += d;
        }
        mux.write_trailer().unwrap();

        let bytes = shared.snapshot();
        let src: Box<dyn vaco_io::MediaSource> = Box::new(MemorySource::new(bytes));
        let mut demux =
            vaco_demux_matroska::MatroskaDemuxer::open(src, &NoParsers, &FormatOptions::default())
                .unwrap();
        prop_assert_eq!(demux.streams().len(), 1);

        let mut got = Vec::new();
        while let Ok(p) = demux.read_packet() {
            got.push(p.payload().to_vec());
        }
        prop_assert_eq!(got, payloads);
    }
}
