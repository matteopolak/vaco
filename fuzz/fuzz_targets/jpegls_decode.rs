//! JPEG-LS decode against arbitrary bytes, plus one exact property: since
//! `NEAR = 0` JPEG-LS is genuinely lossless, `decode(encode(frame)) == frame`
//! must hold for *any* pixel data this crate's encoder accepts — not just a
//! flat image, unlike a lossy codec's fuzz target. This is also the
//! regression surface for the crate's own documented known gap: a handful
//! of `ffmpeg`-encoded fixtures still disagree with `ffmpeg -c:v jpegls`'s
//! decode in a few pixels, so this target's own encoder/decoder pair, if it
//! is ever caught disagreeing with itself, is strictly a bug this crate
//! introduced independently of that gap.
//! fuzz-crate: vaco-codec-jpegls
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_codec_jpegls::{decode, encode};
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};
use vaco_pixfmt::PixFmt;

#[derive(Arbitrary, Debug)]
struct Input {
    /// Raw bytes for the arbitrary-input decode below, unrelated to the
    /// round-trip check.
    data: Vec<u8>,
    rgb: bool,
    width: u8,
    height: u8,
    pixels: Vec<u8>,
}

fn frame_from(fmt: PixFmt, width: u32, height: u32, pixels: &[u8]) -> Option<Frame> {
    let mut budget = Budget::new(Limits::permissive());
    let mut frame = Frame::alloc_video(&mut budget, fmt, width, height).ok()?;
    let FrameData::Video { planes, .. } = &mut frame.data else {
        return None;
    };
    let plane = planes.first_mut()?;
    let stride = plane.stride;
    let rows = plane.rows();
    let buf = plane.data.make_mut();
    let mut src = pixels.iter().copied().cycle();
    for y in 0..rows {
        let row = buf.get_mut(y * stride..y * stride + stride)?;
        for byte in row.iter_mut() {
            *byte = src.next().unwrap_or(0);
        }
    }
    Some(frame)
}

fn plane_bytes(frame: &Frame) -> Option<Vec<u8>> {
    let plane = frame.plane(0)?;
    let mut out = Vec::new();
    for y in 0..plane.rows() {
        out.extend_from_slice(plane.row(y)?);
    }
    Some(out)
}

fuzz_target!(|input: Input| {
    let mut budget = Budget::new(Limits::strict());
    let _ = decode(&input.data, &mut budget);

    if input.pixels.is_empty() {
        return;
    }
    let fmt = if input.rgb { PixFmt::Rgb24 } else { PixFmt::Gray8 };
    // Keep sizes small: the crate's own tests use up to ~48, and a fuzz
    // target's job here is to explore lots of small shapes quickly, not to
    // stress allocation limits (`decode`'s arbitrary-byte fuzzing above
    // already does that).
    let width = u32::from(input.width % 48) + 1;
    let height = u32::from(input.height % 48) + 1;
    let Some(src) = frame_from(fmt, width, height, &input.pixels) else {
        return;
    };
    let Ok(bytes) = encode(&src) else {
        return;
    };
    let mut budget2 = Budget::new(Limits::permissive());
    let decoded = decode(&bytes, &mut budget2)
        .expect("encoding a frame this crate just built must itself decode");
    let FrameData::Video {
        width: dw,
        height: dh,
        format: dfmt,
        ..
    } = decoded.data
    else {
        panic!("jpegls always decodes to a video frame");
    };
    assert_eq!((dw, dh), (width, height), "dimensions changed");
    assert_eq!(dfmt, fmt, "format changed");
    let want = plane_bytes(&src).expect("source plane 0 exists");
    let got = plane_bytes(&decoded).expect("decoded plane 0 exists");
    assert_eq!(
        got, want,
        "{width}x{height} {fmt:?}: a lossless JPEG-LS round trip changed pixels"
    );
});
