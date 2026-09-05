//! Independent high-precision checks for the nonlinear colour stage.
//!
//! These equations deliberately do not call `vaco_scale::colour`: they are the
//! direct H.273/BT.709/IEC 61966-2-1 definitions and the published D65
//! BT.709-to-BT.2020 primary matrix.  That makes a mistaken plan composition
//! fail even if the scaler and its unit helpers share the same mistake.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::many_single_char_names,
    clippy::panic,
    clippy::unwrap_used,
    reason = "independent conformance oracle test"
)]

use vaco_color::{
    ColorInfo, ColorPrimaries, ColorRange, MatrixCoefficients, TransferCharacteristic,
};
use vaco_pixfmt::PixFmt;
use vaco_scale::exec::{DstPlane, SrcPlane};
use vaco_scale::{ImageSpec, ScaleOptions, Scaler};

fn rgb_info(primaries: ColorPrimaries, transfer: TransferCharacteristic) -> ColorInfo {
    ColorInfo {
        primaries,
        transfer,
        matrix: MatrixCoefficients::Identity,
        range: ColorRange::Full,
        ..ColorInfo::default()
    }
}

fn scale_rgb(input: &[[u8; 3]], src: ColorInfo, dst: ColorInfo) -> Vec<[u8; 3]> {
    let width = input.len() as u32;
    let mut bytes = Vec::new();
    for pixel in input {
        bytes.extend(pixel);
    }
    let mut output = vec![0u8; bytes.len()];
    let src_spec = ImageSpec::new(PixFmt::Rgb24, width, 1).with_color(src);
    let dst_spec = ImageSpec::new(PixFmt::Rgb24, width, 1).with_color(dst);
    let mut scaler = Scaler::new(&src_spec, &dst_spec, &ScaleOptions::default()).expect("plan");
    scaler
        .scale_planes(
            &[SrcPlane {
                data: &bytes,
                stride: bytes.len(),
            }],
            &mut [DstPlane {
                data: &mut output,
                stride: bytes.len(),
            }],
        )
        .expect("scale");
    output
        .chunks_exact(3)
        .map(|chunk| [chunk[0], chunk[1], chunk[2]])
        .collect()
}

fn bt709_decode(v: f64) -> f64 {
    if v < 0.081 {
        v / 4.5
    } else {
        ((v + 0.099) / 1.099).powf(1.0 / 0.45)
    }
}

fn bt709_encode(v: f64) -> f64 {
    if v < 0.018 {
        4.5 * v
    } else {
        1.099 * v.powf(0.45) - 0.099
    }
}

fn srgb_encode(v: f64) -> f64 {
    if v < 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

fn mul(m: [[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

fn to_codes(rgb: [f64; 3]) -> [u8; 3] {
    rgb.map(|v| (v * 255.0).round().clamp(0.0, 255.0) as u8)
}

#[test]
fn bt709_and_bt2020_primary_matrices_match_published_vectors_both_directions() {
    // IEC 61966-2-1 / BT.2020's D65 RGB transforms: M_2020^-1 * M_709,
    // rounded here to 10 decimal places, independently of vaco-color's
    // chromaticity solve.
    const TO_2020: [[f64; 3]; 3] = [
        [0.627_403_895_9, 0.329_283_038_4, 0.043_313_065_7],
        [0.069_097_289_4, 0.919_540_395_1, 0.011_362_315_5],
        [0.016_391_438_9, 0.088_013_307_9, 0.895_595_253_2],
    ];
    const TO_709: [[f64; 3]; 3] = [
        [1.660_491_002_1, -0.587_641_138_8, -0.072_849_863_3],
        [-0.124_550_474_5, 1.132_899_897_1, -0.008_349_422_6],
        [-0.018_150_763_4, -0.100_578_898, 1.118_729_661_4],
    ];
    let input = [
        [64, 96, 128],
        [96, 128, 112],
        [128, 112, 96],
        [180, 160, 140],
    ];
    let bt709 = rgb_info(ColorPrimaries::Bt709, TransferCharacteristic::Bt709);
    let bt2020 = rgb_info(ColorPrimaries::Bt2020, TransferCharacteristic::Bt709);

    let expected_2020 = input.map(|pixel| {
        let linear = pixel.map(|v| bt709_decode(f64::from(v) / 255.0));
        to_codes(mul(TO_2020, linear).map(bt709_encode))
    });
    assert_eq!(scale_rgb(&input, bt709, bt2020), expected_2020);

    let expected_709 = expected_2020.map(|pixel| {
        let linear = pixel.map(|v| bt709_decode(f64::from(v) / 255.0));
        to_codes(mul(TO_709, linear).map(bt709_encode))
    });
    assert_eq!(scale_rgb(&expected_2020, bt2020, bt709), expected_709);
}

#[test]
fn linear_to_srgb_uses_the_published_iec_curve() {
    let input = [[0, 26, 128], [255, 64, 192]];
    let linear = rgb_info(ColorPrimaries::Bt709, TransferCharacteristic::Linear);
    let srgb = rgb_info(ColorPrimaries::Bt709, TransferCharacteristic::Iec61966_2_1);
    let expected = input.map(|pixel| to_codes(pixel.map(|v| srgb_encode(f64::from(v) / 255.0))));
    assert_eq!(scale_rgb(&input, linear, srgb), expected);
}
