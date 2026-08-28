//! Every `vaco-codec-image-simple` decoder against arbitrary bytes.
//!
//! Beyond panic-freedom, every successful decode is checked for a real
//! value: its own encoder (each of the six formats has one, previously
//! unused here) must reproduce a byte stream that decodes back to the same
//! pixels. Comparison is per-plane, per-row at the format's *meaningful*
//! width/height (`PixFmt::min_stride`/`plane_height`), not a whole-buffer
//! comparison, since a plane's stride may exceed what the pixels need for
//! alignment.
//!
//! fuzz-crate: vaco-codec-image-simple
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};

type DecodeFn = fn(&[u8], &mut Budget) -> vaco_core::Result<Frame>;
type EncodeFn = fn(&Frame) -> vaco_core::Result<Vec<u8>>;

/// Every plane's rows, trimmed to the format's meaningful width/height —
/// stride padding excluded, so only real pixel content is compared.
fn plane_rows(frame: &Frame) -> Option<Vec<Vec<Vec<u8>>>> {
    let FrameData::Video {
        format,
        width,
        height,
        planes,
    } = &frame.data
    else {
        return None;
    };
    let mut out = Vec::with_capacity(planes.len());
    for (p, plane) in planes.iter().enumerate() {
        let p = p as u8;
        let row_bytes = format.min_stride(*width, p);
        let n_rows = format.plane_height(*height, p) as usize;
        let bytes = plane.data.as_slice();
        let mut rows = Vec::with_capacity(n_rows);
        for y in 0..n_rows {
            let start = y.checked_mul(plane.stride)?;
            let end = start.checked_add(row_bytes)?;
            rows.push(bytes.get(start..end)?.to_vec());
        }
        out.push(rows);
    }
    Some(out)
}

fn check_round_trip(name: &str, decode: DecodeFn, encode: EncodeFn, data: &[u8]) {
    let mut budget = Budget::new(Limits::strict());
    let Ok(frame) = decode(data, &mut budget) else {
        return;
    };
    let Ok(encoded) = encode(&frame) else {
        panic!("{name}: decode succeeded but re-encoding its own frame failed");
    };
    let mut budget2 = Budget::new(Limits::strict());
    let redecoded = decode(&encoded, &mut budget2).unwrap_or_else(|e| {
        panic!("{name}: re-encoding a successfully decoded frame produced bytes it cannot itself decode: {e:?}")
    });
    let original_rows = plane_rows(&frame).unwrap_or_else(|| panic!("{name}: original frame plane is short"));
    let redecoded_rows =
        plane_rows(&redecoded).unwrap_or_else(|| panic!("{name}: re-decoded frame plane is short"));
    assert_eq!(
        original_rows, redecoded_rows,
        "{name}: decode -> encode -> decode changed pixel content"
    );
}

fuzz_target!(|data: &[u8]| {
    check_round_trip(
        "bmp",
        vaco_codec_image_simple::decode_bmp,
        vaco_codec_image_simple::encode_bmp,
        data,
    );
    check_round_trip(
        "pcx",
        vaco_codec_image_simple::decode_pcx,
        vaco_codec_image_simple::encode_pcx,
        data,
    );
    check_round_trip(
        "tga",
        vaco_codec_image_simple::decode_tga,
        vaco_codec_image_simple::encode_tga,
        data,
    );
    check_round_trip(
        "sgi",
        vaco_codec_image_simple::decode_sgi,
        vaco_codec_image_simple::encode_sgi,
        data,
    );
    check_round_trip(
        "xwd",
        vaco_codec_image_simple::decode_xwd,
        vaco_codec_image_simple::encode_xwd,
        data,
    );
    check_round_trip(
        "xbm",
        vaco_codec_image_simple::decode_xbm,
        vaco_codec_image_simple::encode_xbm,
        data,
    );
});
