//! Everything in `vaco-color` that takes a value someone else chose.
//!
//! Two untrusted sources meet in this crate. Code points arrive from a
//! bitstream — H.264/H.265 VUI, an AV1 sequence header, a Matroska Colour
//! element — and names arrive from a command line. Neither may panic, and the
//! arithmetic behind them (shifts by a bit depth, divisions by `1 - Kb`) must
//! not overflow: this target runs with `overflow-checks = true`, so a wrap is a
//! finding rather than a silent wrong answer.
//! fuzz-crate: vaco-color
#![no_main]

use libfuzzer_sys::fuzz_target;
use vaco_color::{
    ChromaLocation, ColorPrimaries, ColorRange, MatrixCoefficients, TransferCharacteristic,
};

fuzz_target!(|data: &[u8]| {
    // --- names. The whole input as one string is the realistic shape: an
    // option value is whatever followed the `=`.
    if let Ok(s) = core::str::from_utf8(data) {
        if let Some(p) = ColorPrimaries::from_name(s) {
            assert_eq!(ColorPrimaries::from_u8(p.to_u8()), Some(p));
            let _ = p.chromaticity();
            let _ = p.rgb_to_xyz();
            let _ = p.xyz_to_rgb();
        }
        if let Some(t) = TransferCharacteristic::from_name(s) {
            assert_eq!(TransferCharacteristic::from_u8(t.to_u8()), Some(t));
        }
        if let Some(m) = MatrixCoefficients::from_name(s) {
            assert_eq!(MatrixCoefficients::from_u8(m.to_u8()), Some(m));
        }
        if let Some(r) = ColorRange::from_name(s) {
            assert_eq!(ColorRange::from_u8(r.to_u8()), Some(r));
        }
        if let Some(c) = ChromaLocation::from_name(s) {
            assert_eq!(ChromaLocation::from_u8(c.to_u8()), Some(c));
            let _ = c.sample_offset_420();
        }
    }

    // --- code points and the maths hanging off them. Bytes are consumed as a
    // (primaries, transfer, matrix, range, chroma, depth) tuple so the fuzzer
    // can reach the cross-product, which is where chroma-derived coefficients
    // live.
    let mut it = data.iter().copied();
    let (Some(pb), Some(tb), Some(mb), Some(rb), Some(cb), Some(db)) = (
        it.next(),
        it.next(),
        it.next(),
        it.next(),
        it.next(),
        it.next(),
    ) else {
        return;
    };

    let primaries = ColorPrimaries::from_u8(pb).unwrap_or_default();
    let matrix = MatrixCoefficients::from_u8(mb).unwrap_or_default();

    if let Some(t) = TransferCharacteristic::from_u8(tb) {
        // A signal value built from the same bytes, so the fuzzer can steer it.
        let e = f64::from(u32::from(pb) | (u32::from(rb) << 8)) / 65535.0 * 2.0 - 0.5;
        if let Some(v) = t.encode(e) {
            assert!(!v.is_nan(), "{t:?}: encode({e}) is NaN");
            let back = t.decode(v).expect("decode where encode succeeded");
            assert!(!back.is_nan(), "{t:?}: decode({v}) is NaN");
        }
        let d = t.decode(e);
        assert_eq!(d.is_some(), t != TransferCharacteristic::Unspecified);
    }

    if let Some(fwd) = matrix.rgb_to_ycbcr_with(primaries) {
        let inv = matrix
            .ycbcr_to_rgb_with(primaries)
            .expect("an invertible matrix must have an inverse");
        // Both directions exist together, and neither may produce a non-finite
        // coefficient: a division by `1 - Kb` with bitstream-supplied primaries
        // is exactly the shape that quietly yields an infinity.
        for row in fwd.iter().chain(inv.iter()) {
            for cell in row {
                assert!(cell.is_finite(), "{matrix:?}/{primaries:?}: {cell}");
            }
        }
    }

    // Bit depth from a byte: every value, not just the plausible ones. The
    // shifts inside `luma_levels` are the overflow risk.
    let depth = u32::from(db);
    if let Some(range) = ColorRange::from_u8(rb) {
        if let Some(l) = range.luma_levels(depth) {
            assert!(l.offset <= l.max && l.scale <= l.max);
        }
        if let Some(c) = range.chroma_levels(depth) {
            assert!(c.offset <= c.max && c.scale <= c.max);
        }
    }

    let _ = ChromaLocation::from_u8(cb).map(ChromaLocation::sample_offset_420);
    let _ = ChromaLocation::from_h264_loc_type(cb);
});
