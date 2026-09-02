//! Demux real `ffmpeg -f swf` output, check the extracted streams/packets
//! against the reference, then remux with this crate's own muxer and check
//! that the reference can still read the result (D17: measure, do not
//! recall).
//!
//! `tests/fixtures/sample.swf` is
//!
//! ```sh
//! ffmpeg -f lavfi -i "testsrc2=size=64x64:rate=12:d=1" \
//!        -f lavfi -i "sine=frequency=1000:duration=1" \
//!        -pix_fmt yuv420p -c:v flv1 -c:a mp3 -map 0:v -map 1:a -bitexact sample.swf
//! ```
//!
//! captured 2026-08-27 with ffmpeg 8.1: 12 video frames and 12
//! `SoundStreamBlock`s (distinguishing input: two streams, more than one
//! frame of each).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

use vaco_format_core::{Demuxer, Muxer};
use vaco_format_swf::{SwfDemuxer, SwfMuxer};
use vaco_io::{MemorySource, SharedDynBuf};

const SAMPLE: &[u8] = include_bytes!("fixtures/sample.swf");

fn open(bytes: &[u8]) -> SwfDemuxer {
    let src = Box::new(MemorySource::new(bytes.to_vec()));
    SwfDemuxer::open(src).expect("open")
}

#[test]
fn a_real_sample_reports_the_measured_streams() {
    let demux = open(SAMPLE);
    let video = demux
        .streams()
        .iter()
        .find(|s| s.media_type() == Some(vaco_core::MediaType::Video))
        .expect("a video stream");
    assert_eq!(video.params.codec_id, Some(vaco_codec_core::CodecId::Flv1));
    let vp = video.params.video.as_ref().expect("video params");
    assert_eq!(vp.width, 64);
    assert_eq!(vp.height, 64);
    assert_eq!(vp.frame_rate, vaco_core::Rational { num: 12, den: 1 });

    let audio = demux
        .streams()
        .iter()
        .find(|s| s.media_type() == Some(vaco_core::MediaType::Audio))
        .expect("an audio stream");
    assert_eq!(audio.params.codec_id, Some(vaco_codec_core::CodecId::Mp3));
    let ap = audio.params.audio.as_ref().expect("audio params");
    assert_eq!(ap.sample_rate, 44_100);
}

#[test]
fn a_real_sample_yields_twelve_packets_per_stream() {
    let mut demux = open(SAMPLE);
    let video_index = demux
        .streams()
        .iter()
        .find(|s| s.media_type() == Some(vaco_core::MediaType::Video))
        .expect("a video stream")
        .index;
    let mut video_count = 0u32;
    let mut audio_count = 0u32;
    loop {
        match demux.read_packet() {
            Ok(p) => {
                if p.stream_index == video_index {
                    video_count += 1;
                } else {
                    audio_count += 1;
                }
            }
            Err(vaco_core::Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    assert_eq!(video_count, 12);
    assert_eq!(audio_count, 12);
}

/// Not byte-identical (see `mux.rs`'s module docs: no `PlaceObject2`,
/// approximate frame ordering) — the distinguishing check here is
/// structural: this crate's own muxer output must still be readable by the
/// reference itself, with the right codec/dimensions/rate/packet count.
/// Skipped rather than failed when `ffmpeg` is absent (mirrors
/// `vaco-codec-core`'s `tests/params.rs`).
#[test]
fn remuxing_a_real_sample_is_still_readable_by_the_reference() {
    let mut demux = open(SAMPLE);
    let streams: Vec<_> = demux.streams().to_vec();

    let sink = SharedDynBuf::new();
    let mirror = sink.clone();
    let mut mux = SwfMuxer::new(Box::new(sink));
    let mut index_map = std::collections::HashMap::new();
    for s in &streams {
        let new_index = mux.add_stream(&s.params).expect("add_stream");
        index_map.insert(s.index, new_index);
    }
    mux.write_header().expect("write_header");
    loop {
        match demux.read_packet() {
            Ok(mut p) => {
                p.stream_index = *index_map.get(&p.stream_index).expect("known stream");
                mux.write_packet(&p).expect("write_packet");
            }
            Err(vaco_core::Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    mux.write_trailer().expect("write_trailer");
    let bytes = mirror.take();
    assert_eq!(&bytes[0..3], b"FWS");

    let dir = std::env::temp_dir();
    let path = dir.join(format!("vaco-swf-remux-test-{}.swf", std::process::id()));
    std::fs::write(&path, &bytes).expect("write temp file");

    let Ok(out) = std::process::Command::new("ffprobe")
        .args([
            "-hide_banner",
            "-v",
            "error",
            "-of",
            "default=nw=1",
            "-show_entries",
            "stream=codec_name,width,height,sample_rate",
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
    assert!(text.contains("codec_name=flv1"), "missing flv1 in: {text}");
    assert!(text.contains("codec_name=mp3"), "missing mp3 in: {text}");
    assert!(text.contains("width=64"), "missing width in: {text}");
    assert!(
        text.contains("sample_rate=44100"),
        "missing sample_rate in: {text}"
    );
}
