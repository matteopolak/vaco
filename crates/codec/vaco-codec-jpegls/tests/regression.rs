//! Byte-exact decode regression against a real `ffmpeg -c:v jpegls`-encoded
//! fixture (`Vaco-Spec-Ref: locoi-hpl98-193`; the specific run-interruption
//! accumulator bug fixed here was found by measurement against `ffmpeg`, not
//! by reading a spec, so this crate's own convention of citing the LOCO-I
//! paper id applies to the surrounding algorithm, not this one formula).
//!
//! `ramp_17x17.jls` is a 17x17 8-bit grayscale ramp (`ffmpeg`'s own
//! `testsrc`-style pattern, sharp enough that its top rows saturate at 255
//! and its bottom-left corner runs a five-sample chain of identical
//! same-context run-interruption events) encoded by the real `ffmpeg`
//! binary; `ramp_17x17.raw` is `ffmpeg -c:v jpegls -f rawvideo`'s own decode
//! of that same file, i.e. the ground truth.
//!
//! Before the fix, `decode_ri_sample`/`encode_ri_sample` updated the
//! run-interruption context's `A` accumulator with the entropy mapping's
//! gap-compressed `shifted` value instead of the real reconstructed error
//! magnitude `eps`. The two are equal whenever `eps <= 0`, so the bug is
//! silent until enough positive-`eps` same-context interruptions accumulate
//! to move the derived Golomb parameter `k` across a power-of-two boundary —
//! this fixture's fifth repeat of an identical `a == b`, `eps == 1`
//! interruption is exactly that boundary: decoding the sixth same-context
//! sample afterward with the wrong `k` reads the wrong number of bits and
//! produces a wrong pixel with no error at all (the "confidently wrong"
//! failure this crate's own module doc warns against). This test decodes
//! the whole image and compares every byte, not just the one that used to
//! differ, so a regression anywhere in the row/column order would also
//! fail it.
//!
//! A real, further gap remains: some larger/busier images (documented in
//! this crate's module doc) still diverge from `ffmpeg` after this fix, in
//! a way traced to the same run-interruption accumulator but not yet fully
//! resolved — see that doc for what was measured.

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

const FIXTURE_JLS: &[u8] = include_bytes!("fixtures/ramp_17x17.jls");
const FIXTURE_RAW: &[u8] = include_bytes!("fixtures/ramp_17x17.raw");

#[test]
fn decodes_a_ramp_with_repeated_run_interruptions_byte_exact_to_ffmpeg() {
    let mut budget = Budget::new(Limits::permissive());
    let frame = vaco_codec_jpegls::decode(FIXTURE_JLS, &mut budget)
        .expect("a real ffmpeg-produced JPEG-LS file must decode");
    let FrameData::Video {
        width,
        height,
        planes,
        ..
    } = &frame.data
    else {
        panic!("jpegls always decodes to a video frame");
    };
    assert_eq!((*width, *height), (17, 17));
    let plane = &planes[0];
    let stride = plane.stride;
    let raw = plane.data.as_slice();
    let mut decoded = Vec::new();
    for y in 0..*height as usize {
        decoded.extend_from_slice(&raw[y * stride..y * stride + *width as usize]);
    }
    assert_eq!(
        decoded, FIXTURE_RAW,
        "decoded pixels must match ffmpeg's own decode of the same file byte for byte"
    );
}
