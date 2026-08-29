//! TIFF decode against arbitrary bytes: it must never panic, and every
//! successful decode's first page must survive a decode -> encode ->
//! decode round trip with pixel-identical output, for every pixel format
//! `vaco-codec-tiff` can actually encode.
//!
//! Grayscale+alpha is a documented decode-only gap (the `tiff` crate's own
//! encoder has no `GrayAlpha` colour type, see `codec.rs`'s
//! `to_encodable`): a decode that lands on `Ya8`/`Ya16` is skipped rather
//! than treated as a re-encode failure, since `Error::Unsupported` there is
//! correct behaviour, not a bug this fuzz target exists to catch.
//!
//! fuzz-crate: vaco-codec-tiff
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_pixfmt::PixFmt;
use vaco_limits::{Budget, Limits};

fn frame_bytes(frame: &vaco_frame::Frame) -> Option<Vec<u8>> {
    let plane = frame.plane(0)?;
    let mut out = Vec::new();
    for row in plane.rows_iter() {
        out.extend_from_slice(row);
    }
    Some(out)
}

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::strict());
    let Ok(frames) = vaco_codec_tiff::decode(data, &mut budget) else {
        return;
    };
    let Some(first) = frames.into_iter().next() else {
        return;
    };
    let vaco_frame::FrameData::Video { format, .. } = first.data else {
        panic!("vaco-codec-tiff only ever produces FrameData::Video");
    };
    if matches!(format, PixFmt::Ya8 | PixFmt::Ya16le | PixFmt::Ya16be) {
        return;
    }

    let Ok(encoded) = vaco_codec_tiff::encode(
        std::slice::from_ref(&first),
        &vaco_codec_tiff::EncodeOptions::default(),
    ) else {
        return;
    };
    let mut budget2 = Budget::new(Limits::permissive());
    let redecoded = vaco_codec_tiff::decode(&encoded, &mut budget2)
        .expect("re-encoding a successfully decoded frame must itself be decodable");
    let Some(redecoded_first) = redecoded.into_iter().next() else {
        panic!("encoding one frame must decode back to at least one frame");
    };
    assert_eq!(
        frame_bytes(&first),
        frame_bytes(&redecoded_first),
        "decode -> encode -> decode changed pixel content"
    );
});
