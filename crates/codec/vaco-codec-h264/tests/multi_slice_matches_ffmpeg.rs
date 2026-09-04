//! Clause 7.4.3's multi-slice picture, byte-for-byte against a real
//! reference decode.
//!
//! A picture coded as several slices is not just a framing detail: clause
//! 6.4.8 makes a macroblock in a *different* slice "not available", which
//! stops intra prediction from reading across the boundary and changes what
//! the deblocking filter does there. Getting that wrong is quiet -- the
//! error is confined to a few macroblock rows around each boundary, so a
//! decode still looks plausible and a size check still passes. Hence the
//! bar here is the same as `decoder_output_matches_ffmpeg`'s: every frame,
//! every plane, every byte, with the first differing frame named on
//! failure.
//!
//! `fixtures/multi_slice_6.264` is `libx264 -profile:v main -bf 0 -refs 1
//! -slices 6` over `testsrc2` at 176x144, six pictures of six slices each
//! (16-17 macroblocks per slice, so most slices start mid-row) with
//! deblocking at its default -- on, and filtering across slice boundaries.
//! `fixtures/multi_slice_6_ref.yuv` is `ffmpeg`'s own decode of it.
//!
//! Driven through [`H264Decoder`]'s public [`Decoder`] surface, with one
//! packet per access unit carrying all six of that picture's slice NALs --
//! the framing an MP4 sample gives a decoder.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::integer_division,
    reason = "test code over a fixed fixture: every division is by the constant 2 (4:2:0 chroma \
              subsampling) over compile-time constants"
)]

use vaco_bitstream::annexb;
use vaco_codec_core::Decoder;
use vaco_codec_h264::H264Decoder;
use vaco_core::Error;
use vaco_frame::FrameData;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

const WIDTH: usize = 176;
const HEIGHT: usize = 144;
const LUMA: usize = WIDTH * HEIGHT;
const CHROMA: usize = (WIDTH / 2) * (HEIGHT / 2);
const FRAME: usize = LUMA + 2 * CHROMA;
const FRAMES: usize = 6;
const SLICES_PER_PICTURE: usize = 6;

fn packed(plane: &vaco_frame::Plane, row_bytes: usize, rows: usize) -> Vec<u8> {
    let data = plane.data.as_slice();
    let mut out = Vec::new();
    for r in 0..rows {
        let start = r * plane.stride;
        out.extend_from_slice(&data[start..start + row_bytes]);
    }
    out
}

/// Split the elementary stream into extradata and one packet per access
/// unit. A new picture starts at the next slice NAL whose
/// `first_mb_in_slice` is 0, which is its first `ue(v)` being the single
/// bit `1`.
fn access_units(stream: &[u8]) -> (Vec<u8>, Vec<Vec<u8>>) {
    let mut extradata = Vec::new();
    let mut aus: Vec<Vec<u8>> = Vec::new();
    let mut au: Vec<u8> = Vec::new();
    let mut have_slice = false;
    for nal in annexb::nal_units(stream) {
        let Some(t) = nal.first().map(|b| b & 0x1F) else {
            continue;
        };
        if matches!(t, 7 | 8) {
            extradata.extend_from_slice(&[0, 0, 0, 1]);
            extradata.extend_from_slice(nal);
            continue;
        }
        if !matches!(t, 1 | 5 | 6 | 9) {
            continue;
        }
        let starts_picture = matches!(t, 1 | 5) && nal.get(1).is_some_and(|b| b & 0x80 != 0);
        if (starts_picture || matches!(t, 6 | 9)) && have_slice {
            aus.push(core::mem::take(&mut au));
            have_slice = false;
        }
        au.extend_from_slice(&[0, 0, 0, 1]);
        au.extend_from_slice(nal);
        if matches!(t, 1 | 5) {
            have_slice = true;
        }
    }
    if have_slice {
        aus.push(au);
    }
    (extradata, aus)
}

#[test]
fn every_frame_of_a_six_slice_stream_is_byte_exact_against_ffmpeg() {
    let stream: &[u8] = include_bytes!("fixtures/multi_slice_6.264");
    let reference: &[u8] = include_bytes!("fixtures/multi_slice_6_ref.yuv");
    assert_eq!(
        reference.len(),
        FRAMES * FRAME,
        "reference fixture is not six whole frames"
    );

    let (extradata, aus) = access_units(stream);
    assert_eq!(aus.len(), FRAMES, "fixture should carry six access units");
    // Guards the fixture itself: if a re-encode ever produced one slice per
    // picture, every assertion below would still pass while testing nothing
    // this file exists to test.
    let slice_nals = annexb::nal_units(stream)
        .filter(|n| matches!(n.first().map(|b| b & 0x1F), Some(1 | 5)))
        .count();
    assert_eq!(
        slice_nals,
        FRAMES * SLICES_PER_PICTURE,
        "fixture must be six slices per picture for this test to mean anything"
    );

    let mut d = H264Decoder::new(Limits::default());
    let mut budget = Budget::new(Limits::default());
    d.set_extradata(&extradata).unwrap();

    let mut frames: Vec<vaco_frame::Frame> = Vec::new();
    for au in &aus {
        let pkt = Packet::from_slice(&mut budget, au).unwrap();
        loop {
            match d.send_packet(Some(&pkt)) {
                Ok(()) => break,
                Err(Error::OutputPending) => frames.push(d.receive_frame().unwrap()),
                Err(e) => panic!("send_packet failed: {e:?}"),
            }
        }
        while let Ok(frame) = d.receive_frame() {
            frames.push(frame);
        }
    }
    d.send_packet(None).unwrap();
    while let Ok(frame) = d.receive_frame() {
        frames.push(frame);
    }

    assert_eq!(frames.len(), FRAMES, "decoded frame count");

    for (i, frame) in frames.iter().enumerate() {
        let FrameData::Video {
            width,
            height,
            planes,
            ..
        } = &frame.data
        else {
            panic!("frame {i} is not video");
        };
        assert_eq!((*width as usize, *height as usize), (WIDTH, HEIGHT));
        let mut got = packed(&planes[0], WIDTH, HEIGHT);
        got.extend_from_slice(&packed(&planes[1], WIDTH / 2, HEIGHT / 2));
        got.extend_from_slice(&packed(&planes[2], WIDTH / 2, HEIGHT / 2));
        let want = &reference[i * FRAME..(i + 1) * FRAME];
        if got != want {
            let differing = got.iter().zip(want).filter(|(a, b)| a != b).count();
            let first = got.iter().zip(want).position(|(a, b)| a != b).unwrap();
            let max = got
                .iter()
                .zip(want)
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap_or(0);
            panic!(
                "frame {i} diverges from the reference decode: {differing} of {FRAME} bytes \
                 ({:.2}%), first at byte {first}, max per-sample delta {max}",
                100.0 * differing as f64 / FRAME as f64
            );
        }
    }
}
