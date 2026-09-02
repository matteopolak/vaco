//! Demux real `ffmpeg -f nut` output and check it against the reference
//! (D17: measure, do not recall).
//!
//! `tests/fixtures/sample.nut` is
//!
//! ```sh
//! ffmpeg -f lavfi -i "testsrc2=size=64x64:rate=25:d=1" \
//!        -f lavfi -i "sine=frequency=1000:duration=1:sample_rate=48000" \
//!        -c:v mpeg4 -g 10 -c:a mp3 -map 0:v -map 1:a -bitexact sample.nut
//! ```
//!
//! captured 2026-08-27 with ffmpeg 8.1: `ffprobe` reports 25 video packets
//! and 43 audio packets (distinguishing input: two streams, more than one
//! syncpoint's worth of frames on each, a real MPEG-4 `codec_specific_data`
//! blob, elision headers this crate's own muxer never writes but this file
//! does).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

use vaco_format_core::Demuxer;
use vaco_format_nut::NutDemuxer;
use vaco_io::MemorySource;

const SAMPLE: &[u8] = include_bytes!("fixtures/sample.nut");

fn open() -> NutDemuxer {
    let src = Box::new(MemorySource::new(SAMPLE.to_vec()));
    NutDemuxer::open(src).expect("open")
}

#[test]
fn a_real_sample_reports_the_measured_streams() {
    let demux = open();
    let video = demux
        .streams()
        .iter()
        .find(|s| s.media_type() == Some(vaco_core::MediaType::Video))
        .expect("a video stream");
    assert_eq!(video.params.codec_id, Some(vaco_codec_core::CodecId::Mpeg4));
    let vp = video.params.video.as_ref().expect("video params");
    assert_eq!(vp.width, 64);
    assert_eq!(vp.height, 64);

    let audio = demux
        .streams()
        .iter()
        .find(|s| s.media_type() == Some(vaco_core::MediaType::Audio))
        .expect("an audio stream");
    assert_eq!(audio.params.codec_id, Some(vaco_codec_core::CodecId::Mp3));
    let ap = audio.params.audio.as_ref().expect("audio params");
    assert_eq!(ap.sample_rate, 48_000);
}

#[test]
fn a_real_sample_yields_the_measured_packet_counts() {
    let mut demux = open();
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
    assert_eq!(video_count, 25);
    assert_eq!(audio_count, 43);
}

/// Video uses `-g 10` (a keyframe every 10 frames), so this is also the
/// check that this crate's elision-header prepending is correct: without
/// it, every 10th (non-keyframe-adjacent) MPEG-4 frame would be missing its
/// shared `0x000001B6` picture start code and fail to look like a valid
/// MPEG-4 frame at all.
#[test]
fn every_video_packet_starts_with_a_real_mpeg4_start_code() {
    let mut demux = open();
    let video_index = demux
        .streams()
        .iter()
        .find(|s| s.media_type() == Some(vaco_core::MediaType::Video))
        .expect("a video stream")
        .index;
    let mut checked = 0u32;
    loop {
        match demux.read_packet() {
            Ok(p) if p.stream_index == video_index => {
                assert!(
                    p.payload().starts_with(&[0x00, 0x00, 0x01]),
                    "video packet does not start with an MPEG start code: {:02x?}",
                    &p.payload()[..p.payload().len().min(8)]
                );
                checked += 1;
            }
            Ok(_) => {}
            Err(vaco_core::Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    assert_eq!(checked, 25);
}

/// pts must be non-decreasing per stream, and dts <= pts always (this
/// muxer's own `decode_delay=0` streams have dts==pts, but the *reference*
/// file's streams may not — `mpeg4` here still does not use B-frames, but
/// the check is the general one the specification actually states, not
/// "happens to hold for this fixture").
#[test]
fn timestamps_are_monotone_per_stream_and_dts_never_exceeds_pts() {
    let mut demux = open();
    let mut last_pts: std::collections::HashMap<u32, i64> = std::collections::HashMap::new();
    loop {
        match demux.read_packet() {
            Ok(p) => {
                let pts = p.pts.ticks().expect("a real pts");
                let dts = p.dts.ticks().expect("a real dts");
                assert!(dts <= pts, "dts {dts} > pts {pts}");
                if let Some(&prev) = last_pts.get(&p.stream_index) {
                    assert!(
                        pts >= prev,
                        "pts went backwards on stream {}: {prev} -> {pts}",
                        p.stream_index
                    );
                }
                last_pts.insert(p.stream_index, pts);
            }
            Err(vaco_core::Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
}

/// `codec_specific_data` copied verbatim into `params.extradata`, and the
/// container's own fourcc into `params.codec_tag` — measured on this same
/// real file: 30 bytes of MPEG-4 VOL header starting `00 00 01 b0`
/// (`visual_object_sequence_start_code`), and fourcc `FMP4`/WAVE tag `0x0055`
/// zero-extended for MP3. Regression: both were previously dropped
/// entirely, leaving every SPS/VOL-derived field (`profile`, `pix_fmt`,
/// `level`, `has_b_frames`, `sample_aspect_ratio` for H.264 elsewhere) and
/// `codec_tag_string` unset for every NUT file.
#[test]
fn a_real_sample_carries_its_extradata_and_container_fourcc() {
    let demux = open();
    let video = demux
        .streams()
        .iter()
        .find(|s| s.media_type() == Some(vaco_core::MediaType::Video))
        .expect("a video stream");
    assert_eq!(video.params.codec_tag, Some(*b"FMP4"));
    let extradata = video.params.extradata.as_deref().expect("video extradata");
    assert_eq!(extradata.len(), 30);
    assert_eq!(&extradata[..4], &[0x00, 0x00, 0x01, 0xb0]);

    let audio = demux
        .streams()
        .iter()
        .find(|s| s.media_type() == Some(vaco_core::MediaType::Audio))
        .expect("an audio stream");
    // WAVE_FORMAT_MP3 (0x0055), zero-extended to 4 bytes little-endian.
    assert_eq!(audio.params.codec_tag, Some([0x55, 0x00, 0x00, 0x00]));
    // MP3 needs no extradata; this stays `None` rather than `Some(vec![])`.
    assert_eq!(audio.params.extradata, None);
}
