//! Decodes a real H.264 keyframe through `VideoToolbox` on this machine and
//! checks the result is structurally right — dimensions, pixel format, and
//! non-degenerate pixel content. Byte-exactness against any particular
//! decoder is not the bar (D17): different silicon legitimately produces
//! different output, and this test does not have a reference frame to
//! compare against in any case.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

use vaco_hw_core::HwAccel;
use vaco_hw_videotoolbox::{VideoToolboxDecoder, nal_unit_type, split_annex_b};
use vaco_limits::{Budget, Limits};

const FIXTURE: &[u8] = include_bytes!("fixtures/tiny_baseline_64x64.h264");

/// SPS, PPS and one IDR slice, in the shape `decode_slice` wants: no start
/// code, no length prefix, whichever NAL units the caller decides matter
/// (SEI is dropped here — `VideoToolbox`'s parameter-set API does not want
/// it, and a real caller would route it to its own SEI consumer instead).
fn fixture_units() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let units = split_annex_b(FIXTURE);
    let mut sps = None;
    let mut pps = None;
    let mut idr = None;
    for unit in units {
        match nal_unit_type(unit) {
            Some(7) => sps = Some(unit.to_vec()),
            Some(8) => pps = Some(unit.to_vec()),
            Some(5) => idr = Some(unit.to_vec()),
            _ => {}
        }
    }
    (
        sps.expect("fixture has an SPS"),
        pps.expect("fixture has a PPS"),
        idr.expect("fixture has an IDR slice"),
    )
}

#[test]
fn decodes_a_real_keyframe_to_the_right_dimensions_and_format() {
    let (sps, pps, idr) = fixture_units();

    let mut decoder =
        VideoToolboxDecoder::new(&sps, &pps).expect("VideoToolbox accepts this stream's SPS/PPS");

    decoder.start_frame().expect("start_frame never fails here");
    decoder
        .decode_slice(&idr)
        .expect("decode_slice never fails here");
    let hw_frame = decoder
        .end_frame()
        .expect("a real IDR slice against a matching format description decodes");

    assert_eq!((hw_frame.width, hw_frame.height), (64, 64));
    assert!(hw_frame.hw_pix_fmt.is_hw());

    let mut budget = Budget::new(Limits::strict());
    let frame = hw_frame
        .download(&mut budget)
        .expect("VideoToolbox always produces a readable pixel buffer for a successful decode");

    let vaco_frame::FrameData::Video {
        format,
        width,
        height,
        planes,
    } = frame.data
    else {
        panic!("expected a video frame");
    };
    assert!(!format.is_hw(), "downloaded frame must be a real software pixel format");
    assert_eq!((width, height), (64, 64));
    assert_eq!(planes.len(), 2, "NV12 has two planes");

    // Not byte-exact against anything (there is no reference frame here) —
    // just a sanity check that real pixels came back rather than an
    // untouched, zero-filled allocation.
    let luma = &planes[0];
    let any_nonzero = luma.data.as_slice().iter().any(|&b| b != 0);
    assert!(any_nonzero, "decoded luma plane must not be all zero");
}

#[test]
fn decoding_garbage_slice_data_fails_cleanly_rather_than_panicking() {
    let (sps, pps, _idr) = fixture_units();
    let mut decoder = VideoToolboxDecoder::new(&sps, &pps).expect("SPS/PPS are valid");

    decoder.start_frame().expect("start_frame never fails here");
    // A slice NAL header claiming a type it is not, with no real slice
    // header behind it. VideoToolbox is expected to reject this cleanly.
    decoder
        .decode_slice(&[0x65, 0xFF, 0xFF, 0xFF, 0xFF])
        .expect("decode_slice only buffers bytes; it never inspects them");
    let result = decoder.end_frame();
    assert!(result.is_err(), "garbage slice data must not decode successfully");
}
