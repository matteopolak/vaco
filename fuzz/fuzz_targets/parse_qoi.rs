//! QOI decode against arbitrary bytes: it must never panic, and every
//! successful decode must survive a decode -> encode -> decode round trip
//! with pixel-identical output. `encode` is exercised for real here, not
//! just for panic-freedom: its bytes are fed straight back into `decode`
//! and compared row-by-row against the first decode's planes, so a decoder
//! and encoder that agree on a *wrong* pixel would still be caught the
//! moment either one alone disagrees with a plain re-decode of its own
//! output... which is not actually guaranteed by construction, so the
//! comparison here is between the two *decodes*, not decoder-vs-encoder:
//! that is the one part of the pipeline this crate does not depend on the
//! reference implementation to check (see `codec.rs`'s D17 note).
//!
//! fuzz-crate: vaco-codec-qoi
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};
use vaco_pixfmt::PixFmt;

/// QOI only ever produces these two formats (`codec.rs`'s `channels` match).
fn bytes_per_pixel(format: PixFmt) -> usize {
    match format {
        PixFmt::Rgb24 => 3,
        PixFmt::Rgba => 4,
        other => unreachable!("vaco-codec-qoi never produces {other:?}"),
    }
}

/// Extract plane 0 as `height` rows of exactly `row_bytes` bytes each,
/// ignoring any padding stride adds past the meaningful width.
fn plane0_rows(frame: &Frame, row_bytes: usize) -> Option<Vec<Vec<u8>>> {
    let FrameData::Video { height, planes, .. } = &frame.data else {
        return None;
    };
    let plane = planes.first()?;
    let bytes = plane.data.as_slice();
    let mut rows = Vec::with_capacity(*height as usize);
    for y in 0..*height as usize {
        let start = y.checked_mul(plane.stride)?;
        let end = start.checked_add(row_bytes)?;
        rows.push(bytes.get(start..end)?.to_vec());
    }
    Some(rows)
}

fuzz_target!(|data: &[u8]| {
    let mut budget = Budget::new(Limits::strict());
    let Ok(frame) = vaco_codec_qoi::decode(data, &mut budget) else {
        return;
    };
    let FrameData::Video { format, width, .. } = &frame.data else {
        unreachable!("vaco-codec-qoi only ever produces FrameData::Video");
    };
    let row_bytes = *width as usize * bytes_per_pixel(*format);

    let Ok(encoded) = vaco_codec_qoi::encode(&frame) else {
        panic!("decode succeeded but re-encoding its own frame failed");
    };

    let mut budget2 = Budget::new(Limits::strict());
    let redecoded = vaco_codec_qoi::decode(&encoded, &mut budget2)
        .expect("re-encoding a successfully decoded frame must itself be decodable");

    let original_rows = plane0_rows(&frame, row_bytes).expect("original frame plane is short");
    let redecoded_rows =
        plane0_rows(&redecoded, row_bytes).expect("re-decoded frame plane is short");
    assert_eq!(
        original_rows, redecoded_rows,
        "decode -> encode -> decode changed pixel content"
    );
});
