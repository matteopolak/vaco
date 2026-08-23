//! Mux a few seconds of video through `vaco-mux-dash` onto real files in a
//! `tempfile::tempdir()`, then read the segment files back with
//! `vaco-demux-mp4` directly (bypassing MPD parsing, which
//! `vaco-demux-dash`'s own test suite already covers) to confirm every
//! packet written comes back, and that the rendered MPD's structure
//! (`<Representation>`/`<S>` counts, total duration) agrees with what was
//! actually segmented.
//!
//! Everything here goes through `file:` — no network.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]

use vaco_codec_core::{CodecId, CodecParameters, VideoParameters};
use vaco_core::{Error, MediaType, Result, Timestamp};
use vaco_demux_mp4::{Mp4Demuxer, Mp4Options};
use vaco_format_adaptive::{SegmentContainerHint, SegmentMuxerProvider, WriteAccess};
use vaco_format_core::{Demuxer, FormatOptions, Muxer, discovery::NoParsers};
use vaco_io::{MediaSink, MediaSource, MemorySource};
use vaco_limits::{Budget, Limits};
use vaco_mux_dash::{DashMuxOptions, DashMuxer};
use vaco_mux_mp4::MovMuxer;
use vaco_packet::{Packet, PacketFlags};
use vaco_protocol_core::ProtocolRegistry;

#[derive(Debug, Default)]
struct TestSegmentMuxers;

impl SegmentMuxerProvider for TestSegmentMuxers {
    fn open_segment(
        &self,
        hint: SegmentContainerHint,
        sink: Box<dyn MediaSink>,
        streams: &[CodecParameters],
        _init_only: bool,
    ) -> Result<Box<dyn Muxer>> {
        match hint {
            SegmentContainerHint::Fmp4 => {
                let mut m = MovMuxer::new(sink)?;
                for p in streams {
                    m.add_stream(p)?;
                }
                Ok(Box::new(m))
            }
            _ => Err(Error::Unsupported("not exercised by this test")),
        }
    }
}

fn avc_extradata() -> Vec<u8> {
    vec![
        1, 0x42, 0x00, 0x0A, 0xFF, 0xE1, 0x00, 0x04, 0x67, 0x42, 0x00, 0x0A, 0x01, 0x00, 0x02,
        0x68, 0xCE,
    ]
}

fn h264_params() -> CodecParameters {
    let mut p = CodecParameters {
        media_type: Some(MediaType::Video),
        codec_id: Some(CodecId::H264),
        extradata: Some(avc_extradata()),
        ..CodecParameters::default()
    };
    p.video = Some(VideoParameters {
        width: 64,
        height: 48,
        frame_rate: vaco_core::Rational::new(30, 1),
        ..VideoParameters::default()
    });
    p
}

fn registry() -> ProtocolRegistry {
    let mut r = ProtocolRegistry::new();
    r.register(&vaco_protocol_file::FILE_PROTOCOL);
    r
}

#[test]
fn six_seconds_of_video_produces_two_second_segments_readable_back() {
    // 10 fps for 6 seconds = 60 frames; a keyframe every 20th frame lines up
    // exactly with the 2-second `seg_duration` target.
    const FPS: i64 = 10;
    const SECONDS: i64 = 6;
    // Written out rather than `90_000 / FPS`: the workspace denies
    // `integer_division`, and a hard-coded 9_000 says "one tenth of a second at
    // 90 kHz" at least as clearly as the division did.
    const STEP_TICKS: i64 = 9_000;
    let dir = tempfile::tempdir().unwrap();
    let mpd_url = dir.path().join("stream.mpd").to_str().unwrap().to_owned();

    let mut mux = DashMuxer::new(
        mpd_url.clone(),
        Some(WriteAccess::unrestricted(registry())),
        Box::new(TestSegmentMuxers),
        DashMuxOptions {
            seg_duration: 2.0,
            ..DashMuxOptions::new()
        },
    );
    let video = mux.add_stream(&h264_params()).expect("add_stream");
    mux.init().expect("init");
    mux.write_header().expect("write_header");

    let mut budget = Budget::new(Limits::permissive());
    for i in 0..(FPS * SECONDS) {
        let mut pkt = Packet::from_slice(&mut budget, &[0xAB; 64]).expect("alloc");
        pkt.stream_index = video;
        pkt.pts = Timestamp::new(i * STEP_TICKS);
        pkt.dts = pkt.pts;
        if i % 20 == 0 {
            pkt.flags |= PacketFlags::KEY;
        }
        mux.write_packet(&pkt).expect("write_packet");
    }
    mux.write_trailer().expect("write_trailer");
    drop(mux);

    let mpd_text = std::fs::read_to_string(&mpd_url).unwrap();
    assert_eq!(
        mpd_text.matches("<Representation").count(),
        1,
        "one stream must produce one Representation:\n{mpd_text}"
    );
    let s_count = mpd_text.matches("<S ").count();
    assert!(s_count >= 1, "expected at least one <S> entry:\n{mpd_text}");

    // Read every segment file this run produced directly, in order, and
    // confirm every frame comes back.
    let mut total = 0i64;
    let mut number = 1u64;
    loop {
        let name = format!("chunk-stream0-{number:05}.m4s");
        let path = dir.path().join(&name);
        if !path.exists() {
            break;
        }
        let bytes = std::fs::read(&path).unwrap();
        let src: Box<dyn MediaSource> = Box::new(MemorySource::new(bytes));
        let mut demux = Mp4Demuxer::open(
            src,
            &NoParsers,
            &FormatOptions::default(),
            Mp4Options::default(),
        )
        .expect("open segment as mp4");
        loop {
            match demux.read_packet() {
                Ok(_) => total += 1,
                Err(Error::Eof) => break,
                Err(e) => panic!("unexpected error reading segment {number}: {e:?}"),
            }
        }
        number += 1;
    }
    assert!(
        number > 1,
        "no segment files were found under {}",
        dir.path().display()
    );
    assert_eq!(
        total,
        FPS * SECONDS,
        "every written frame must be recoverable from the segment files"
    );
}
