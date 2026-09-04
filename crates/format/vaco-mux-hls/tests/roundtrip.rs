//! Mux ten seconds through `vaco-mux-hls` onto real files in a
//! `tempfile::tempdir()`, then read the result back with `vaco-demux-hls`.
//!
//! This is the property the brief calls out explicitly: "mux N seconds,
//! demux it back, get the same segment boundaries." Everything here goes
//! through `file:` — no network, matching the brief's "no test may require a
//! server" rule.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]

use vaco_codec_core::{CodecId, CodecParameters, VideoParameters};
use vaco_core::{Error, MediaType, Rational, Result, Timestamp};
use vaco_demux_hls::{HlsDemuxer, HlsOptions, RemoteAccess as ReadAccess};
use vaco_demux_mpegts::MpegTsDemuxer;
use vaco_format_adaptive::{
    NoSegmentDemuxers, SegmentContainerHint, SegmentDemuxerProvider, SegmentMuxerProvider,
};
use vaco_format_core::{Demuxer, FormatOptions, Muxer, ParserProvider, discovery::NoParsers};
use vaco_io::{MediaSink, MediaSource};
use vaco_limits::{Budget, Limits};
use vaco_mux_hls::{HlsMuxOptions, HlsMuxer, HlsPlaylistType, WriteAccess};
use vaco_mux_mpegts::mux::MpegTsMuxer;
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
            SegmentContainerHint::MpegTs => {
                let mut m = MpegTsMuxer::new(sink);
                for p in streams {
                    m.add_stream(p)?;
                }
                Ok(Box::new(m))
            }
            _ => Err(Error::Unsupported("only MpegTs is exercised by this test")),
        }
    }
}

#[derive(Debug, Default)]
struct TestSegmentDemuxers;

impl SegmentDemuxerProvider for TestSegmentDemuxers {
    fn open_segment(
        &self,
        hint: SegmentContainerHint,
        init: Option<&[u8]>,
        source: Box<dyn MediaSource>,
        parsers: &dyn ParserProvider,
    ) -> Result<Box<dyn Demuxer>> {
        match hint {
            SegmentContainerHint::MpegTs => Ok(Box::new(MpegTsDemuxer::open(
                source,
                parsers,
                &FormatOptions::default(),
            )?)),
            _ => NoSegmentDemuxers.open_segment(hint, init, source, parsers),
        }
    }
}

fn registry() -> ProtocolRegistry {
    let mut r = ProtocolRegistry::new();
    r.register(&vaco_protocol_file::FILE_PROTOCOL);
    r
}

#[test]
fn ten_seconds_of_video_produces_five_two_second_segments_and_reads_back_whole() {
    // 5 fps for 10 seconds = 50 frames; a keyframe every 10th frame lines up
    // exactly with the 2-second `hls_time` target, so five clean segments are
    // the only reasonable outcome of a correct implementation.
    const FPS: i64 = 5;
    const SECONDS: i64 = 10;
    #[allow(
        clippy::integer_division,
        reason = "90_000 (the MPEG-TS clock) is exactly divisible by every realistic fps"
    )]
    const STEP_90K: i64 = 90_000 / FPS;

    let dir = tempfile::tempdir().unwrap();
    let playlist_url = dir.path().join("out.m3u8").to_str().unwrap().to_owned();

    let mut mux = HlsMuxer::new(
        playlist_url.clone(),
        Some(WriteAccess::unrestricted(registry())),
        Box::new(TestSegmentMuxers),
        HlsMuxOptions {
            hls_time: 2.0,
            hls_list_size: 0, // VOD: keep every segment listed
            hls_playlist_type: HlsPlaylistType::Vod,
            ..HlsMuxOptions::default()
        },
    );
    let video = mux
        .add_stream(&CodecParameters {
            media_type: Some(MediaType::Video),
            codec_id: Some(CodecId::Mpeg2video),
            ..CodecParameters::new(MediaType::Video)
        })
        .expect("add_stream");
    mux.init().expect("init");
    mux.write_header().expect("write_header");

    let mut budget = Budget::new(Limits::permissive());
    for i in 0..(FPS * SECONDS) {
        let mut pkt = Packet::from_slice(&mut budget, &[0xAB; 64]).expect("alloc");
        pkt.stream_index = video;
        pkt.pts = Timestamp::new(i * STEP_90K);
        pkt.dts = pkt.pts;
        if i % 10 == 0 {
            pkt.flags |= PacketFlags::KEY;
        }
        mux.write_packet(&pkt).expect("write_packet");
    }
    mux.write_trailer().expect("write_trailer");
    drop(mux);

    // Read the playlist text directly: five segments, VOD, ENDLIST.
    let text = std::fs::read_to_string(&playlist_url).unwrap();
    let segment_count = text.lines().filter(|l| l.starts_with("#EXTINF:")).count();
    assert_eq!(segment_count, 5, "playlist:\n{text}");
    assert!(text.contains("#EXT-X-PLAYLIST-TYPE:VOD\n"));
    assert!(text.contains("#EXT-X-ENDLIST\n"));

    // Read it back with the real HLS demuxer and confirm every frame comes
    // back, in order, with a continuous (non-decreasing) timeline — the
    // actual "same segment boundaries" property: nothing lost, nothing
    // duplicated, and the discontinuity-free path leaves timestamps alone.
    let src = Box::new(vaco_io::MemorySource::new(
        std::fs::read(&playlist_url).unwrap(),
    ));
    let mut demux = HlsDemuxer::open(
        src,
        &playlist_url,
        Some(ReadAccess::unrestricted(registry())),
        Box::new(NoParsers),
        Box::new(TestSegmentDemuxers),
        &HlsOptions::default(),
    )
    .expect("open");

    let mut count = 0i64;
    let mut last_dts = i64::MIN;
    loop {
        match demux.read_packet() {
            Ok(p) => {
                let dts = p.dts.ticks().expect("dts");
                assert!(
                    dts >= last_dts,
                    "dts must not go backwards: {dts} < {last_dts}"
                );
                last_dts = dts;
                count += 1;
            }
            Err(Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    assert_eq!(count, FPS * SECONDS, "every frame written must come back");
}

/// `#EXTINF` must cover each segment's real content span — the last
/// packet's own duration included, not merely "the timestamp gap to that
/// packet's start". No packet here states a `duration` (the ordinary
/// `-c copy` case out of a demuxer that reports none), so before this fix
/// every segment's `finish_current_segment` computed only
/// `last_dts - start_dts`, one whole frame (200ms at 5fps) short of the
/// true 2.0s span — not only on the last segment, but on all five.
#[test]
fn extinf_covers_the_last_frames_own_span_not_just_its_start() {
    const FPS: i64 = 5;
    const SECONDS: i64 = 10;
    #[allow(
        clippy::integer_division,
        reason = "90_000 (the MPEG-TS clock) is exactly divisible by every realistic fps"
    )]
    const STEP_90K: i64 = 90_000 / FPS;

    let dir = tempfile::tempdir().unwrap();
    let playlist_url = dir.path().join("out.m3u8").to_str().unwrap().to_owned();

    let mut mux = HlsMuxer::new(
        playlist_url.clone(),
        Some(WriteAccess::unrestricted(registry())),
        Box::new(TestSegmentMuxers),
        HlsMuxOptions {
            hls_time: 2.0,
            hls_list_size: 0,
            hls_playlist_type: HlsPlaylistType::Vod,
            ..HlsMuxOptions::default()
        },
    );
    let video = mux
        .add_stream(&CodecParameters {
            media_type: Some(MediaType::Video),
            codec_id: Some(CodecId::Mpeg2video),
            video: Some(VideoParameters {
                frame_rate: Rational::new(FPS as i32, 1),
                ..VideoParameters::default()
            }),
            ..CodecParameters::new(MediaType::Video)
        })
        .expect("add_stream");
    mux.init().expect("init");
    mux.write_header().expect("write_header");

    let mut budget = Budget::new(Limits::permissive());
    for i in 0..(FPS * SECONDS) {
        let mut pkt = Packet::from_slice(&mut budget, &[0xAB; 64]).expect("alloc");
        pkt.stream_index = video;
        pkt.pts = Timestamp::new(i * STEP_90K);
        pkt.dts = pkt.pts;
        // Deliberately left at its default (no stated duration) — this is
        // the case the fallback exists for.
        if i % 10 == 0 {
            pkt.flags |= PacketFlags::KEY;
        }
        mux.write_packet(&pkt).expect("write_packet");
    }
    mux.write_trailer().expect("write_trailer");
    drop(mux);

    let text = std::fs::read_to_string(&playlist_url).unwrap();
    let extinf_lines: Vec<&str> = text.lines().filter(|l| l.starts_with("#EXTINF:")).collect();
    assert_eq!(extinf_lines.len(), 5, "playlist:\n{text}");
    for line in &extinf_lines {
        assert_eq!(
            *line, "#EXTINF:2.000,",
            "every 10-frame segment at 5fps spans a full 2.0s, including its \
             last frame's own 200ms — not the 1.8s a start-to-start span gives:\n{text}"
        );
    }
}

/// `hls_flags single_file`: every segment lands in one physical file,
/// addressed by `#EXT-X-BYTERANGE`. Checks the byte ranges are contiguous,
/// non-overlapping, and that reading them back through the real HLS demuxer
/// (which drives `BoundedSource`) recovers every frame — the same
/// nothing-lost/nothing-duplicated property as the multi-file case, over the
/// genuinely different code path `counting::CountingSink` exists for.
#[test]
fn single_file_segments_are_contiguous_non_overlapping_byte_ranges() {
    const FPS: i64 = 5;
    const SECONDS: i64 = 6;
    #[allow(
        clippy::integer_division,
        reason = "90_000 (the MPEG-TS clock) is exactly divisible by every realistic fps"
    )]
    const STEP_90K: i64 = 90_000 / FPS;

    let dir = tempfile::tempdir().unwrap();
    let playlist_url = dir.path().join("out.m3u8").to_str().unwrap().to_owned();

    let mut mux = HlsMuxer::new(
        playlist_url.clone(),
        Some(WriteAccess::unrestricted(registry())),
        Box::new(TestSegmentMuxers),
        HlsMuxOptions {
            hls_time: 2.0,
            hls_list_size: 0,
            hls_playlist_type: HlsPlaylistType::Vod,
            hls_flags: vaco_mux_hls::HlsFlags::SINGLE_FILE,
            ..HlsMuxOptions::default()
        },
    );
    let video = mux
        .add_stream(&CodecParameters {
            media_type: Some(MediaType::Video),
            codec_id: Some(CodecId::Mpeg2video),
            ..CodecParameters::new(MediaType::Video)
        })
        .expect("add_stream");
    mux.init().expect("init");
    mux.write_header().expect("write_header");
    let mut budget = Budget::new(Limits::permissive());
    for i in 0..(FPS * SECONDS) {
        let mut pkt = Packet::from_slice(&mut budget, &[0xCD; 64]).expect("alloc");
        pkt.stream_index = video;
        pkt.pts = Timestamp::new(i * STEP_90K);
        pkt.dts = pkt.pts;
        if i % 10 == 0 {
            pkt.flags |= PacketFlags::KEY;
        }
        mux.write_packet(&pkt).expect("write_packet");
    }
    mux.write_trailer().expect("write_trailer");
    drop(mux);

    let text = std::fs::read_to_string(&playlist_url).unwrap();
    assert_eq!(
        text.lines().filter(|l| l.starts_with("#EXTINF:")).count(),
        3,
        "playlist:\n{text}"
    );
    // Every segment must name the *same* underlying file.
    let uris: Vec<&str> = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .collect();
    assert_eq!(uris.len(), 3);
    assert!(uris.iter().all(|u| *u == uris[0]));

    // Byte ranges must tile the file with no gap and no overlap.
    let ranges: Vec<(u64, u64)> = text
        .lines()
        .filter_map(|l| l.strip_prefix("#EXT-X-BYTERANGE:"))
        .map(|v| {
            let (len, off) = v.split_once('@').expect("length@offset");
            (off.parse().unwrap(), len.parse().unwrap())
        })
        .collect();
    assert_eq!(ranges.len(), 3);
    let mut cursor = 0u64;
    for &(offset, length) in &ranges {
        assert_eq!(offset, cursor, "ranges must be contiguous: {ranges:?}");
        cursor += length;
    }

    let src = Box::new(vaco_io::MemorySource::new(
        std::fs::read(&playlist_url).unwrap(),
    ));
    let mut demux = HlsDemuxer::open(
        src,
        &playlist_url,
        Some(ReadAccess::unrestricted(registry())),
        Box::new(NoParsers),
        Box::new(TestSegmentDemuxers),
        &HlsOptions::default(),
    )
    .expect("open");
    let mut count = 0i64;
    loop {
        match demux.read_packet() {
            Ok(_) => count += 1,
            Err(Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    assert_eq!(count, FPS * SECONDS);
}
