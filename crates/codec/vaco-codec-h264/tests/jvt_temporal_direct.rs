//! JVT `CABA3_SVA_B` temporal-direct regression.
//!
//! This published JVT AVCv1 conformance stream is the smallest registered
//! temporal-direct case: 33 QCIF CABAC IPB pictures, one slice per picture,
//! `direct_spatial_mv_pred_flag == 0`, and `direct_8x8_inference_flag == 0`.
//! Its supplied reconstruction is independently byte-identical to the local
//! `ffmpeg` 9.0.1 raw elementary-stream decode.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "fixed published conformance fixture with checked frame geometry"
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
const CHROMA_WIDTH: usize = 88;
const CHROMA_HEIGHT: usize = 72;
const LUMA: usize = WIDTH * HEIGHT;
const CHROMA: usize = CHROMA_WIDTH * CHROMA_HEIGHT;
const FRAME: usize = LUMA + 2 * CHROMA;
const FRAMES: usize = 33;

fn packed(plane: &vaco_frame::Plane, row_bytes: usize, rows: usize) -> Vec<u8> {
    let data = plane.data.as_slice();
    let mut out = Vec::new();
    for row in 0..rows {
        let start = row * plane.stride;
        out.extend_from_slice(&data[start..start + row_bytes]);
    }
    out
}

#[test]
fn jvt_caba3_temporal_direct_matches_its_published_reconstruction() {
    let stream = include_bytes!("fixtures/jvt_caba3_sva_b_temporal_direct.264");
    let reference = include_bytes!("fixtures/jvt_caba3_sva_b_temporal_direct_ref.yuv");
    assert_eq!(
        reference.len(),
        FRAMES * FRAME,
        "reference must contain whole QCIF frames"
    );

    let mut extradata = Vec::new();
    let mut pictures = Vec::new();
    for nal in annexb::nal_units(stream) {
        match nal.first().map(|byte| byte & 0x1f) {
            Some(7 | 8) => {
                extradata.extend_from_slice(&[0, 0, 0, 1]);
                extradata.extend_from_slice(nal);
            }
            Some(1 | 5) => {
                let mut picture = vec![0, 0, 0, 1];
                picture.extend_from_slice(nal);
                pictures.push(picture);
            }
            _ => {}
        }
    }
    assert_eq!(
        pictures.len(),
        FRAMES,
        "fixture must contain one slice per picture"
    );

    let mut decoder = H264Decoder::new(Limits::default());
    decoder.set_extradata(&extradata).unwrap();
    let mut budget = Budget::new(Limits::default());
    let mut output = Vec::new();

    for picture in pictures {
        let packet = Packet::from_slice(&mut budget, &picture).unwrap();
        loop {
            match decoder.send_packet(Some(&packet)) {
                Ok(()) => break,
                Err(Error::OutputPending) => {
                    let frame = decoder.receive_frame().unwrap();
                    append_yuv420p(&mut output, &frame);
                }
                Err(error) => panic!("JVT temporal-direct decode failed: {error:?}"),
            }
        }
        while let Ok(frame) = decoder.receive_frame() {
            append_yuv420p(&mut output, &frame);
        }
    }
    decoder.send_packet(None).unwrap();
    loop {
        match decoder.receive_frame() {
            Ok(frame) => append_yuv420p(&mut output, &frame),
            Err(Error::Eof) => break,
            Err(error) => panic!("JVT temporal-direct drain failed: {error:?}"),
        }
    }

    assert_eq!(
        output.len(),
        reference.len(),
        "decoded frame count or geometry differs"
    );
    assert_eq!(
        output, reference,
        "JVT CABA3 temporal-direct output differs"
    );
}

fn append_yuv420p(output: &mut Vec<u8>, frame: &vaco_frame::Frame) {
    let FrameData::Video { format, planes, .. } = &frame.data else {
        panic!("expected a video frame");
    };
    assert_eq!(format.name(), "yuv420p");
    assert_eq!(planes.len(), 3);
    output.extend_from_slice(&packed(&planes[0], WIDTH, HEIGHT));
    output.extend_from_slice(&packed(&planes[1], CHROMA_WIDTH, CHROMA_HEIGHT));
    output.extend_from_slice(&packed(&planes[2], CHROMA_WIDTH, CHROMA_HEIGHT));
}
