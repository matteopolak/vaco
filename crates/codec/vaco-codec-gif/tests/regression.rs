//! Byte-exact decode regression against a real `ffmpeg -f gif`-encoded
//! fixture (`Vaco-Spec-Ref: ffprobe-gif-pixfmt-probe`; the pixel-format bug
//! fixed here was found by measurement against `ffmpeg`/`ffprobe`, not by
//! reading GIF89a itself, which says nothing about the channel order a
//! particular decoder chooses to emit).
//!
//! `testsrc_8x6.gif` is an 8x6 single-frame GIF (`ffmpeg`'s own
//! `testsrc` pattern, chosen because it has varied, non-gray colours per
//! pixel — a red/blue channel swap would otherwise go unnoticed on a
//! grayscale or symmetric-colour source) produced by the real `ffmpeg`
//! binary; `testsrc_8x6.bgra.raw` is `ffmpeg -pix_fmt bgra -f rawvideo`'s own
//! decode of that same file, i.e. the ground truth.
//!
//! Before the fix, `codec::decode` allocated its output canvas as
//! `PixFmt::Rgba` and `composite()` copied the `gif` crate's already-RGBA
//! subframe bytes straight onto it — but `vaco-parse-image`'s own `gif::Gif`
//! parser (and the reference decoder) both declare this codec's stream
//! format as `bgra`. A decoder whose frames disagree with its own probed
//! format is wrong regardless of whether anything downstream currently
//! checks: every red and blue byte in the output was swapped. This test
//! decodes the real file and compares every byte, not just a sampled pixel,
//! so a regression in row/column order or the swap itself would also fail
//! it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "integration test: a panic here is a test failure by design, and slicing on \
              offsets this same file just computed from the decoded frame's own reported \
              geometry is the readable form of a bounds check that would otherwise just be \
              reasserting the arithmetic two lines up"
)]

use vaco_frame::FrameData;
use vaco_limits::{Budget, Limits};

const FIXTURE_GIF: &[u8] = include_bytes!("fixtures/testsrc_8x6.gif");
const FIXTURE_BGRA: &[u8] = include_bytes!("fixtures/testsrc_8x6.bgra.raw");

#[test]
fn decodes_a_testsrc_frame_byte_exact_to_ffmpegs_own_bgra_decode() {
    let mut budget = Budget::new(Limits::permissive());
    let frames = vaco_codec_gif::decode(FIXTURE_GIF, &mut budget)
        .expect("a real ffmpeg-produced GIF file must decode");
    assert_eq!(frames.len(), 1, "the fixture carries exactly one frame");
    let FrameData::Video {
        width,
        height,
        planes,
        ..
    } = &frames[0].data
    else {
        panic!("gif always decodes to a video frame");
    };
    assert_eq!((*width, *height), (8, 6));
    let plane = &planes[0];
    let stride = plane.stride;
    let raw = plane.data.as_slice();
    let mut decoded = Vec::new();
    for y in 0..*height as usize {
        decoded.extend_from_slice(&raw[y * stride..y * stride + *width as usize * 4]);
    }
    assert_eq!(
        decoded, FIXTURE_BGRA,
        "decoded BGRA pixels must match ffmpeg's own bgra decode of the same file byte for byte"
    );
}
