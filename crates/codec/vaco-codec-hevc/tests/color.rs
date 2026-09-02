//! Finding 22a (`planning/INTERFACE-GAPS.md`): `Sps::color_info()` was real
//! and correct, and `CodecParameters::color` already set it from the VUI --
//! so `vaco-probe -show_streams` reported the true `color_primaries`/
//! `color_transfer`/`color_space`/`color_range` -- but nothing in
//! `vaco-codec-hevc` ever wrote `Frame::color`, so a decoded HEVC frame
//! carried `Frame::alloc_video`'s `ColorInfo::default()` regardless of what
//! the stream signalled.
//!
//! # Fixture and its generation
//!
//! ```text
//! ffmpeg -y -f lavfi -i "testsrc=size=32x32:rate=25:duration=0.04" -pix_fmt yuv420p \
//!        -c:v libx265 -x265-params \
//!        "colorprim=bt709:transfer=bt709:colormatrix=bt709:range=limited:keyint=1" \
//!        -frames:v 1 -f hevc tests/fixtures/vui_bt709.hevc
//! ```
//!
//! Measured with real `ffprobe 9.0.1` on the same file: `color_range=tv
//! color_space=bt709 color_transfer=bt709 color_primaries=bt709` -- the
//! exact four values [`a_real_bt709_stream_stamps_its_measured_colour_onto_the_decoded_frame`]
//! asserts on the decoded `Frame`.

#![allow(
    clippy::unwrap_used,
    reason = "test code over fixed, checked-in fixtures"
)]

use vaco_codec_core::Decoder;
use vaco_codec_hevc::HevcDecoder;
use vaco_limits::{Budget, Limits};
use vaco_packet::Packet;

fn decode_first_frame(bytes: &[u8]) -> vaco_frame::Frame {
    let mut d = HevcDecoder::new(Limits::default());
    let mut budget = Budget::new(Limits::default());
    let pkt = Packet::from_slice(&mut budget, bytes).unwrap();
    d.send_packet(Some(&pkt)).unwrap();
    d.send_packet(None).unwrap();
    d.receive_frame().unwrap()
}

#[test]
fn a_real_bt709_stream_stamps_its_measured_colour_onto_the_decoded_frame() {
    let frame = decode_first_frame(include_bytes!("fixtures/vui_bt709.hevc"));
    assert_eq!(frame.color.primaries, vaco_color::ColorPrimaries::Bt709);
    assert_eq!(
        frame.color.transfer,
        vaco_color::TransferCharacteristic::Bt709
    );
    assert_eq!(frame.color.matrix, vaco_color::MatrixCoefficients::Bt709);
    assert_eq!(frame.color.range, vaco_color::ColorRange::Limited);
}

/// The regression case on the other side of the fix above, using
/// `flat_gray_64x64.hevc` (already checked in for [`super`]'s own flat-frame
/// test): real `ffmpeg`-measured `color_range=tv color_space=unknown
/// color_transfer=unknown color_primaries=unknown` -- `libx265`'s own
/// default VUI signals `video_full_range_flag` but not a full
/// `colour_description`. `Sps::color_info`'s own doc records the HEVC-
/// specific inference this must land on: `chroma_location=Left` for 4:2:0
/// even though `colour_description` itself is absent (HEVC and H.264
/// deliberately disagree on this -- see that doc comment), not
/// `ColorInfo::default()`.
#[test]
fn a_stream_with_partial_vui_infers_range_but_leaves_colour_description_unspecified() {
    let frame = decode_first_frame(include_bytes!("fixtures/flat_gray_64x64.hevc"));
    assert_eq!(
        frame.color.primaries,
        vaco_color::ColorPrimaries::Unspecified
    );
    assert_eq!(
        frame.color.transfer,
        vaco_color::TransferCharacteristic::Unspecified
    );
    assert_eq!(
        frame.color.matrix,
        vaco_color::MatrixCoefficients::Unspecified
    );
    assert_eq!(frame.color.range, vaco_color::ColorRange::Limited);
    assert_eq!(
        frame.color.chroma_location,
        vaco_color::ChromaLocation::Left
    );
}
