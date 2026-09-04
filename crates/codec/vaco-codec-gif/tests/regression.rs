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
//!
//! # Frame count on a truncated file (`Vaco-Spec-Ref: ffprobe-gif-frame-count-probe`)
//!
//! `truncated_anim3_16x12.gif` is a real `ffmpeg`-encoded 3-frame animation
//! with its last 5 bytes cut off, landing mid-LZW-data in the third frame.
//! `ffprobe -count_frames` still reports 3 frames on this exact file (see
//! the declared source for the full sweep); before this fix,
//! `codec::decode` discarded the third frame outright the moment its pixel
//! data errored, even though that frame's own header (Image Descriptor +
//! Graphic Control Extension, including its delay) had already parsed
//! cleanly, so it returned only 2. GIF89a does not define recovery from a
//! truncated data-sub-block sequence, so there is no spec answer to match —
//! only the reference's own choice, which this reproduces: a frame whose
//! header is complete is counted even if its pixel data ran out, composited
//! from whatever partial pixel data decoded before the error (zero, i.e.
//! transparent, past that point) rather than dropped entirely. This test
//! does not assert the third frame's exact bytes — GIF89a gives no
//! reference point for what a truncated stream's pixels "should" be, and
//! this crate's own partial-fill mechanics have no reason to match the
//! reference's byte-for-byte — only that a third frame exists at all. The
//! first two, undamaged frames are still checked byte-exact against the
//! reference's own decode, so a regression that corrupted them too would
//! also fail this test.

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

const TRUNCATED_GIF: &[u8] = include_bytes!("fixtures/truncated_anim3_16x12.gif");
const TRUNCATED_FIRST_TWO_FRAMES_BGRA: &[u8] =
    include_bytes!("fixtures/truncated_anim3_16x12.first_two_frames.bgra.raw");

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

#[test]
fn a_frame_truncated_mid_lzw_is_still_counted_like_ffprobe_counts_it() {
    let mut budget = Budget::new(Limits::permissive());
    let frames = vaco_codec_gif::decode(TRUNCATED_GIF, &mut budget)
        .expect("a header-complete frame with truncated pixel data must still decode");
    assert_eq!(
        frames.len(),
        3,
        "ffprobe -count_frames reports 3 frames on this exact file; a frame whose header \
         parsed but whose LZW data ran out must still be counted, not dropped"
    );

    let mut first_two = Vec::new();
    for frame in &frames[..2] {
        let FrameData::Video {
            width,
            height,
            planes,
            ..
        } = &frame.data
        else {
            panic!("gif always decodes to a video frame");
        };
        assert_eq!((*width, *height), (16, 12));
        let plane = &planes[0];
        let stride = plane.stride;
        let raw = plane.data.as_slice();
        for y in 0..*height as usize {
            first_two.extend_from_slice(&raw[y * stride..y * stride + *width as usize * 4]);
        }
    }
    assert_eq!(
        first_two, TRUNCATED_FIRST_TWO_FRAMES_BGRA,
        "the two undamaged frames before the truncation must still decode byte-exact"
    );
}
