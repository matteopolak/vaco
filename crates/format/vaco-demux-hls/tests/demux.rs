//! End-to-end: real MPEG-TS segment files on disk, read through
//! `file:`, reassembled by `HlsDemuxer`.
//!
//! Every test here writes fixtures to a `tempfile::tempdir()` and reads them
//! back through `vaco-protocol-file` — never the network — per the brief's
//! "no test may require a server" rule.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]

mod common;

use common::{TestSegmentDemuxers, drain, ts_segment};
use vaco_demux_hls::{HlsDemuxer, HlsOptions, RemoteAccess};
use vaco_format_adaptive::NoSegmentDemuxers;
use vaco_format_core::Demuxer;
use vaco_format_core::discovery::NoParsers;
use vaco_io::MemorySource;
use vaco_protocol_core::ProtocolRegistry;

fn access(dir: &std::path::Path) -> RemoteAccess {
    let mut registry = ProtocolRegistry::new();
    registry.register(&vaco_protocol_file::FILE_PROTOCOL);
    let mut a = RemoteAccess::unrestricted(registry);
    a.root = Some(dir.to_path_buf());
    a
}

fn open_media_playlist(dir: &std::path::Path, text: &str) -> HlsDemuxer {
    let path = dir.join("media.m3u8");
    std::fs::write(&path, text).expect("write playlist");
    let src = Box::new(MemorySource::new(text.as_bytes().to_vec()));
    HlsDemuxer::open(
        src,
        path.to_str().unwrap(),
        Some(access(dir)),
        Box::new(NoParsers),
        Box::new(TestSegmentDemuxers),
        &HlsOptions::default(),
    )
    .expect("open media playlist")
}

#[test]
fn a_vod_playlist_reads_every_segment_in_order() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("seg0.ts"), ts_segment(0, 3600, 3)).unwrap();
    std::fs::write(dir.path().join("seg1.ts"), ts_segment(10_800, 3600, 3)).unwrap();

    let playlist = format!(
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-PLAYLIST-TYPE:VOD\n\
         #EXTINF:2.0,\nfile://{}/seg0.ts\n\
         #EXTINF:2.0,\nfile://{}/seg1.ts\n#EXT-X-ENDLIST\n",
        dir.path().display(),
        dir.path().display(),
    );
    let mut demux = open_media_playlist(dir.path(), &playlist);
    let rows = drain(&mut demux);
    // 3 packets per segment, 2 segments, continuous 3600-tick spacing since
    // there is no discontinuity between them.
    assert_eq!(rows.len(), 6);
    let dts: Vec<i64> = rows.iter().map(|(_, d)| d.unwrap()).collect();
    for w in dts.windows(2) {
        assert!(w[1] > w[0], "dts must strictly increase: {dts:?}");
    }
}

/// The property the brief calls out explicitly: "mux N seconds, demux it
/// back, get the same segment boundaries" — restated here as "reading past a
/// discontinuity produces a continuous timeline", which is what a player
/// actually needs and what a naive implementation (no re-basing at all) gets
/// visibly wrong: the second segment's raw clock restarts near zero, so
/// without re-basing the dts sequence would go strictly *backwards* at the
/// boundary.
#[test]
fn discontinuity_produces_a_continuous_timeline_not_a_backwards_jump() {
    let dir = tempfile::tempdir().unwrap();
    // Second segment's own raw clock restarts at 0 — as if it were a fresh
    // encode, which is exactly what #EXT-X-DISCONTINUITY documents.
    std::fs::write(dir.path().join("a.ts"), ts_segment(0, 3600, 3)).unwrap();
    std::fs::write(dir.path().join("b.ts"), ts_segment(0, 3600, 3)).unwrap();

    let playlist = format!(
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-PLAYLIST-TYPE:VOD\n\
         #EXTINF:2.0,\nfile://{dir}/a.ts\n\
         #EXT-X-DISCONTINUITY\n\
         #EXTINF:2.0,\nfile://{dir}/b.ts\n#EXT-X-ENDLIST\n",
        dir = dir.path().display(),
    );
    let mut demux = open_media_playlist(dir.path(), &playlist);
    let rows = drain(&mut demux);
    assert_eq!(rows.len(), 6);
    let dts: Vec<i64> = rows.iter().map(|(_, d)| d.unwrap()).collect();
    for w in dts.windows(2) {
        assert!(
            w[1] > w[0],
            "a discontinuity must not produce a backwards or repeated dts: {dts:?}"
        );
    }
}

#[test]
fn a_keyed_segment_fails_the_read_with_a_named_error_not_a_parse_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("seg0.ts"), ts_segment(0, 3600, 1)).unwrap();
    let playlist = format!(
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n\
         #EXT-X-KEY:METHOD=AES-128,URI=\"https://example/key\"\n\
         #EXTINF:2.0,\nfile://{}/seg0.ts\n#EXT-X-ENDLIST\n",
        dir.path().display(),
    );
    let mut demux = open_media_playlist(dir.path(), &playlist);
    assert_eq!(demux.playlist().keys.len(), 1);
    assert_eq!(demux.playlist().keys[0].method, "AES-128");
    let err = demux.read_packet().unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.to_lowercase().contains("aes"),
        "expected the error to name the encryption method, got: {msg}"
    );
}

#[test]
fn opening_through_the_registered_ctor_without_access_still_parses() {
    // `DEMUXER.open` (the plain registry path) cannot fetch anything, but a
    // self-contained media playlist with no segments to actually read still
    // parses without panicking or hanging — see the crate docs' honesty
    // about this gap.
    let text = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-ENDLIST\n";
    let src = Box::new(MemorySource::new(text.as_bytes().to_vec()));
    let demux = (vaco_demux_hls::DEMUXER.open)(src, &NoParsers);
    assert!(demux.is_ok());
}

#[test]
fn no_segment_demuxers_refuses_fmp4_cleanly() {
    // Exercises the "no implementation for this hint" path directly, since
    // the fixture builder here only produces MPEG-TS.
    use vaco_format_adaptive::{SegmentContainerHint, SegmentDemuxerProvider};
    let src = Box::new(MemorySource::new(Vec::new()));
    let result = NoSegmentDemuxers.open_segment(SegmentContainerHint::Fmp4, None, src, &NoParsers);
    // `Box<dyn Demuxer>` is not `Debug`, so match rather than `unwrap_err`.
    let Err(err) = result else {
        panic!("expected NoSegmentDemuxers to refuse Fmp4");
    };
    assert!(matches!(err, vaco_core::Error::Unsupported(_)));
}
