//! JPEG decode against arbitrary bytes: baseline and progressive, every
//! marker parser, every scan variant, restart handling.
//!
//! Beyond panic-freedom on arbitrary bytes, one exact property from
//! `tests/roundtrip.rs` is wired in over a fuzzed slice of the format,
//! size and quality space: a perfectly flat image round-trips exactly at
//! quality 100 (`a_perfectly_flat_image_round_trips_exactly_at_quality_100`).
//! JPEG is lossy in general — a gradient only round-trips to within a
//! measured tolerance, per that same file — so this deliberately does not
//! attempt to check the arbitrary-byte decode's own output against
//! anything: there is no encoder input to compare it to. The flat-image
//! case is the one place JPEG's own quantisation still guarantees an exact
//! answer, so it is the one property that survives being generalised
//! rather than tested at fixed values.
//! fuzz-crate: vaco-codec-jpeg
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_codec_jpeg::{EncodeOptions, decode, encode};
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};
use vaco_pixfmt::PixFmt;

/// The five formats `tests/roundtrip.rs` itself exercises: one grayscale,
/// four YCbCr subsampling variants. Trying an unsupported format is not
/// tested here -- `encode` already reports that as `Err`, not a panic, and
/// this target is not the place to re-verify that.
const FORMATS: [&str; 5] = ["gray", "yuvj420p", "yuvj422p", "yuvj444p", "yuvj440p"];

#[derive(Arbitrary, Debug)]
struct Input {
    /// Raw bytes for the arbitrary-input decode below, unrelated to the
    /// flat-image check.
    data: Vec<u8>,
    format: u8,
    width: u8,
    height: u8,
    fill: u8,
    restart_interval: u16,
    progressive: bool,
}

fn flat_frame(fmt: PixFmt, width: u32, height: u32, fill: u8) -> Option<Frame> {
    let mut budget = Budget::new(Limits::permissive());
    let mut frame = Frame::alloc_video(&mut budget, fmt, width, height).ok()?;
    let FrameData::Video { planes, .. } = &mut frame.data else {
        return None;
    };
    for plane in planes.iter_mut() {
        let rows = plane.rows();
        let stride = plane.stride;
        let data = plane.data.make_mut();
        for y in 0..rows {
            data.get_mut(y * stride..y * stride + stride)?.fill(fill);
        }
    }
    Some(frame)
}

fn plane_bytes(frame: &Frame, index: usize) -> Option<Vec<u8>> {
    let plane = frame.plane(index)?;
    let mut out = Vec::new();
    for y in 0..plane.rows() {
        out.extend_from_slice(plane.row(y)?);
    }
    Some(out)
}

fuzz_target!(|input: Input| {
    let mut budget = Budget::new(Limits::strict());
    let _ = decode(&input.data, &mut budget);

    // Quality 100 is the one setting the flat-image property holds at
    // exactly; a fuzzed quality below that would make the property false
    // for a reason that is JPEG's own design, not a bug.
    let Some(name) = FORMATS.get(input.format as usize % FORMATS.len()) else {
        return;
    };
    let Ok(fmt) = PixFmt::from_name(name) else {
        return;
    };
    // Keep sizes small (the crate's own tests use 16-64) and non-zero: JPEG
    // has no defined behaviour for a zero-sized image, and this property is
    // not the place to explore that -- decode's arbitrary-byte fuzzing above
    // already does.
    let width = u32::from(input.width % 64) + 1;
    let height = u32::from(input.height % 64) + 1;
    let Some(src) = flat_frame(fmt, width, height, input.fill) else {
        return;
    };
    let options = EncodeOptions {
        quality: 100,
        restart_interval: input.restart_interval,
        progressive: input.progressive,
    };
    let Ok(bytes) = encode(&src, &options) else {
        return;
    };
    let mut budget2 = Budget::new(Limits::permissive());
    let decoded = decode(&bytes, &mut budget2)
        .expect("encoding a flat frame this crate just built must itself decode");
    let FrameData::Video {
        width: dw,
        height: dh,
        format: dfmt,
        ..
    } = decoded.data
    else {
        panic!("jpeg always decodes to a video frame");
    };
    assert_eq!((dw, dh), (width, height), "{name}: dimensions changed");
    assert_eq!(dfmt, fmt, "{name}: format changed");
    for i in 0..fmt.plane_count() {
        let want = plane_bytes(&src, i).expect("source plane {i} exists");
        let got = plane_bytes(&decoded, i).expect("decoded plane {i} exists");
        assert_eq!(
            got, want,
            "{name} {width}x{height} fill={}: a flat image did not survive quality-100 JPEG round trip in plane {i}",
            input.fill
        );
    }
});
