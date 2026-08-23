//! Shared fixtures for the integration tests: a `TestSegmentDemuxers`
//! provider backed by the real MPEG-TS demuxer/muxer (dev-dependencies of
//! this crate only, never a production dependency — see the crate docs and
//! `vaco_format_adaptive::provider` for why).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use vaco_codec_core::{CodecId, CodecParameters};
use vaco_core::{Error, MediaType, Result, Timestamp};
use vaco_demux_mpegts::MpegTsDemuxer;
use vaco_format_adaptive::{NoSegmentDemuxers, SegmentContainerHint, SegmentDemuxerProvider};
use vaco_format_core::{Demuxer, FormatOptions, Muxer, ParserProvider};
use vaco_io::{MediaSink, MediaSource, SharedDynBuf};
use vaco_limits::{Budget, Limits};
use vaco_mux_mpegts::mux::MpegTsMuxer;
use vaco_packet::{Packet, PacketFlags};

/// Opens MPEG-TS segments with the real demuxer; refuses fMP4 (not needed by
/// these tests, and exercising the "no implementation for this hint" path is
/// exactly what [`NoSegmentDemuxers`] already covers on its own).
#[derive(Debug, Default)]
pub(crate) struct TestSegmentDemuxers;

impl SegmentDemuxerProvider for TestSegmentDemuxers {
    fn open_segment(
        &self,
        hint: SegmentContainerHint,
        init: Option<&[u8]>,
        source: Box<dyn MediaSource>,
        parsers: &dyn ParserProvider,
    ) -> Result<Box<dyn Demuxer>> {
        match hint {
            SegmentContainerHint::MpegTs => {
                let d = MpegTsDemuxer::open(source, parsers, &FormatOptions::default())?;
                Ok(Box::new(d))
            }
            _ => NoSegmentDemuxers.open_segment(hint, init, source, parsers),
        }
    }
}

/// Build one MPEG-TS segment's bytes: one video stream, `count` packets each
/// `step_90k` ticks (90 kHz) apart, decode times starting at `start_90k`.
pub(crate) fn ts_segment(start_90k: i64, step_90k: i64, count: i64) -> Vec<u8> {
    let sink = SharedDynBuf::new();
    let mirror = sink.clone();
    let mut mux = MpegTsMuxer::new(Box::new(sink) as Box<dyn MediaSink>);
    let video = mux
        .add_stream(&CodecParameters {
            media_type: Some(MediaType::Video),
            codec_id: Some(CodecId::Mpeg2video),
            ..CodecParameters::new(MediaType::Video)
        })
        .expect("add video stream");
    mux.init().expect("init");
    mux.write_header().expect("write_header");
    let mut budget = Budget::new(Limits::permissive());
    for i in 0..count {
        let ts = start_90k + i * step_90k;
        let mut pkt = Packet::from_slice(&mut budget, &[0xAB; 188]).expect("alloc packet");
        pkt.stream_index = video;
        pkt.pts = Timestamp::new(ts);
        pkt.dts = Timestamp::new(ts);
        if i == 0 {
            pkt.flags |= PacketFlags::KEY;
        }
        mux.write_packet(&pkt).expect("write packet");
    }
    mux.write_trailer().expect("write_trailer");
    drop(mux);
    mirror.take()
}

/// Every emitted `(stream_index, dts_ticks)` pair, draining a demuxer to
/// `Eof`.
pub(crate) fn drain(demux: &mut dyn Demuxer) -> Vec<(u32, Option<i64>)> {
    let mut out = Vec::new();
    loop {
        match demux.read_packet() {
            Ok(p) => out.push((p.stream_index, p.dts.ticks())),
            Err(Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    out
}
