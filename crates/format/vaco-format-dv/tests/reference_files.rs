//! Demux real `ffmpeg -f dv` output (D17/plan 13 §1b: measure, do not
//! recall).
//!
//! `tests/fixtures/ntsc_sample.dv` is the first 240000 bytes (exactly two
//! 120000-byte frames) of `ffmpeg -f lavfi -i testsrc=size=720x480:rate=
//! 30000/1001:duration=1 -f lavfi -i sine=frequency=1000:duration=1 -c:v
//! dvvideo -pix_fmt yuv411p -c:a pcm_s16le -ar 48000 -ac 2 -f dv out.dv`,
//! captured 2026-08-23 with ffmpeg 8.1.
//!
//! `tests/fixtures/dvcpro50_sample.dv` is the first 130000 bytes of the same
//! command with `-pix_fmt yuv422p` — real DVCPRO50-shaped output whose
//! actual frame size (240000 bytes, measured) does not match what
//! `DvProfile::detect`'s `dsf`-bit heuristic alone would compute (120000).
//! This exercises the second-frame sanity check `DvDemuxer::open`
//! performs specifically because of that gap (see `profile.rs`'s docs).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use vaco_core::{Error, MediaType};
use vaco_format_core::Demuxer;
use vaco_format_dv::DvDemuxer;
use vaco_io::MemorySource;

fn open(bytes: &'static [u8]) -> vaco_core::Result<DvDemuxer> {
    let src = Box::new(MemorySource::new(bytes.to_vec()));
    DvDemuxer::open(src)
}

#[test]
fn a_real_ntsc_sample_reports_the_measured_dimensions_and_frame_rate() {
    let demux = open(include_bytes!("fixtures/ntsc_sample.dv")).expect("open");
    let video = demux
        .streams()
        .iter()
        .find(|s| s.params.media_type == Some(MediaType::Video))
        .expect("a video stream");
    let vp = video.params.video.as_ref().expect("video parameters");
    assert_eq!(vp.width, 720);
    assert_eq!(vp.height, 480);
    assert_eq!(
        vp.frame_rate,
        vaco_core::Rational {
            num: 30_000,
            den: 1_001
        }
    );
    // Measured (`ffmpeg -c:v dvvideo` at 720x480, real `ffprobe`):
    // `sample_aspect_ratio=8:9`. DV's luma is always sampled at a fixed
    // 720 columns regardless of the picture's true 4:3 shape, so this is
    // not derivable from width/height alone.
    assert_eq!(vp.sample_aspect_ratio, vaco_core::Rational { num: 8, den: 9 });
    // Measured (`ffmpeg -c:v dvvideo`, real `ffprobe`): `field_order=
    // unknown`. DV carries no interlace-flag bit this crate reads,
    // and `VideoParameters::field_order`'s own `#[default]` is
    // `Progressive`, which silently reported the wrong value here before
    // this field was set explicitly.
    assert_eq!(vp.field_order, vaco_codec_core::FieldOrder::Unknown);
}

#[test]
fn a_real_ntsc_sample_yields_two_whole_frame_video_packets() {
    let mut demux = open(include_bytes!("fixtures/ntsc_sample.dv")).expect("open");
    let mut count = 0u32;
    loop {
        match demux.read_packet() {
            Ok(p) => {
                assert_eq!(
                    p.len, 120_000,
                    "each DV25 NTSC frame is exactly 120000 bytes"
                );
                assert!(
                    p.flags.contains(vaco_packet::PacketFlags::KEY),
                    "DV is all-intra"
                );
                count += 1;
            }
            Err(Error::Eof) => break,
            Err(e) => panic!("read_packet: {e:?}"),
        }
    }
    assert_eq!(count, 2, "the fixture holds exactly two complete frames");
}

#[test]
fn a_real_ntsc_sample_reports_the_measured_time_base_and_avg_frame_rate() {
    // Measured directly against real `ffprobe` (a 2-frame fixture, a
    // 150-frame/5s NTSC clip, and a 1s PAL clip): `time_base=1/60000` and
    // `avg_frame_rate=60000/1` for DV video, unconditionally -- not derived
    // from this file's own frame count or duration, and not `50/1` for PAL.
    // `r_frame_rate` is untouched: it keeps reporting the true `30000/1001`.
    let mut demux = open(include_bytes!("fixtures/ntsc_sample.dv")).expect("open");
    let video = demux
        .streams()
        .iter()
        .find(|s| s.params.media_type == Some(MediaType::Video))
        .expect("a video stream");
    assert_eq!(
        video.time_base,
        vaco_core::Rational { num: 1, den: 60_000 }
    );
    assert_eq!(
        video.avg_frame_rate,
        vaco_core::Rational {
            num: 60_000,
            den: 1
        }
    );
    // 2002 ticks/frame at 1/60000 for 30000/1001 fps: the first packet
    // starts at pts=0, the second at pts=2002, matching the real per-frame
    // duration rather than the old one-tick-per-frame scheme.
    let first = demux.read_packet().expect("first packet");
    assert_eq!(first.pts.ticks(), Some(0));
    let second = demux.read_packet().expect("second packet");
    assert_eq!(second.pts.ticks(), Some(2_002));
}

#[test]
fn a_double_rate_dvcpro50_sample_is_refused_rather_than_misframed() {
    let result = open(include_bytes!("fixtures/dvcpro50_sample.dv"));
    assert!(
        matches!(result, Err(Error::InvalidData(_))),
        "expected the second-frame sanity check to reject this, got {result:?}"
    );
}
