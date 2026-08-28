//! Demux real `ffmpeg -f mpjpeg` output and remux it, checking for byte
//! identity (D17: measure, do not recall).
//!
//! `tests/fixtures/sample.mjpg` is
//!
//! ```sh
//! ffmpeg -f lavfi -i "testsrc2=size=32x32:rate=25:d=0.2" -pix_fmt yuvj420p \
//!   -c:v mjpeg -f mpjpeg -frames:v 5 sample.mjpg
//! ```
//!
//! captured 2026-08-27 with ffmpeg 8.1: five real JPEG frames, each in its
//! own MIME part, which is the whole of what this format can vary — there is
//! no second stream, no B-frames and no fractional frame rate to distinguish
//! here because MPJPEG structurally cannot carry any of those (see `lib.rs`'s
//! module docs for what this format actually is).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use vaco_format_core::{Demuxer, Muxer};
use vaco_format_mpjpeg::{MpjpegDemuxer, MpjpegMuxer};
use vaco_io::{MemorySource, SharedDynBuf};

const SAMPLE: &[u8] = include_bytes!("fixtures/sample.mjpg");

#[test]
fn a_real_sample_reports_the_measured_dimensions() {
    let src = Box::new(MemorySource::new(SAMPLE.to_vec()));
    let demux = MpjpegDemuxer::open(src).expect("open");
    let video = demux.streams().first().expect("a video stream");
    let vp = video.params.video.as_ref().expect("video parameters");
    assert_eq!(vp.width, 32);
    assert_eq!(vp.height, 32);
    assert_eq!(video.params.codec_id, Some(vaco_codec_core::CodecId::Jpeg));
}

#[test]
fn a_real_sample_yields_exactly_five_keyframe_packets() {
    let src = Box::new(MemorySource::new(SAMPLE.to_vec()));
    let mut demux = MpjpegDemuxer::open(src).expect("open");
    let mut count = 0u32;
    loop {
        match demux.read_packet() {
            Ok(p) => {
                assert_eq!(p.stream_index, 0);
                assert!(p.flags.contains(vaco_packet::PacketFlags::KEY));
                assert!(p.payload().starts_with(&[0xFF, 0xD8]));
                count += 1;
            }
            Err(vaco_core::Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    assert_eq!(count, 5);
}

/// The distinguishing check for this format: demux the reference's own
/// output and remux it with this crate's muxer using the default boundary
/// tag (which is what the reference file was produced with), then compare
/// bytes. No lossy step exists between the two — every header this crate
/// writes is a literal copied from the reference's own layout — so anything
/// other than exact equality is a real bug, not an acceptable divergence.
#[test]
fn remuxing_a_real_sample_reproduces_it_byte_for_byte() {
    let src = Box::new(MemorySource::new(SAMPLE.to_vec()));
    let mut demux = MpjpegDemuxer::open(src).expect("open");

    let sink = SharedDynBuf::new();
    let mirror = sink.clone();
    let mut mux = MpjpegMuxer::new(Box::new(sink));
    let video = demux.streams().first().expect("a video stream").clone();
    mux.add_stream(&video.params).expect("add_stream");
    mux.write_header().expect("write_header");

    loop {
        match demux.read_packet() {
            Ok(p) => mux.write_packet(&p).expect("write_packet"),
            Err(vaco_core::Error::Eof) => break,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    mux.write_trailer().expect("write_trailer");

    assert_eq!(mirror.take(), SAMPLE);
}
