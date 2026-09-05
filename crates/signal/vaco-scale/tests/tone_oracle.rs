//! Independent BT.2390 HDR-to-SDR oracle.
//!
//! The test owns its PQ, BT.709, and Hermite equations. It deliberately does
//! not call `vaco_color` or `vaco_scale::colour`, so a shared mistake in the
//! transfer stage and the LUT builder cannot self-certify.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::many_single_char_names,
    clippy::panic,
    clippy::print_stdout,
    clippy::unwrap_used,
    reason = "independent high-precision conformance oracle"
)]

use vaco_color::{
    ColorInfo, ColorPrimaries, ColorRange, MatrixCoefficients, TransferCharacteristic,
};
use vaco_pixfmt::PixFmt;
use vaco_scale::exec::{DstPlane, SrcPlane};
use vaco_scale::{ImageSpec, ScaleOptions, Scaler};

const M1: f64 = 2610.0 / 16_384.0;
const M2: f64 = 2523.0 / 32.0;
const C1: f64 = 3424.0 / 4096.0;
const C2: f64 = 2413.0 / 128.0;
const C3: f64 = 2392.0 / 128.0;

fn pq_encode(nits: f64) -> f64 {
    let light = (nits / 10_000.0).clamp(0.0, 1.0);
    let raised = light.powf(M1);
    ((C1 + C2 * raised) / (1.0 + C3 * raised)).powf(M2)
}

fn pq_decode(code: f64) -> f64 {
    let raised = code.clamp(0.0, 1.0).powf(1.0 / M2);
    ((raised - C1).max(0.0) / (C2 - C3 * raised)).powf(1.0 / M1) * 10_000.0
}

fn bt709_encode(light: f64) -> f64 {
    if light < 0.018 {
        4.5 * light
    } else {
        1.099 * light.powf(0.45) - 0.099
    }
}

fn bt2390(nits: f64, source_peak: f64, target_peak: f64) -> f64 {
    let source_white = pq_encode(source_peak);
    let target_white = pq_encode(target_peak);
    let max_luminance = target_white / source_white;
    let knee = (1.5 * max_luminance - 0.5).clamp(0.0, 1.0);
    let input = (pq_encode(nits.min(source_peak)) / source_white).clamp(0.0, 1.0);
    let shaped = if input < knee || knee >= 1.0 {
        input
    } else {
        let t = (input - knee) / (1.0 - knee);
        let t2 = t * t;
        let t3 = t2 * t;
        (2.0 * t3 - 3.0 * t2 + 1.0) * knee
            + (t3 - 2.0 * t2 + t) * (1.0 - knee)
            + (-2.0 * t3 + 3.0 * t2) * max_luminance
    };
    pq_decode(shaped * source_white)
}

fn rgb(primaries: ColorPrimaries, transfer: TransferCharacteristic) -> ColorInfo {
    ColorInfo {
        primaries,
        transfer,
        matrix: MatrixCoefficients::Identity,
        range: ColorRange::Full,
        ..ColorInfo::default()
    }
}

#[test]
fn bt2390_pq_to_sdr_matches_an_independent_peak_aware_oracle() {
    let source_peak = 1_000u32;
    let target_peak = 100u32;
    let target_nits = [1.0, 10.0, 50.0, 100.0, 400.0, 1_000.0];
    let input: Vec<u8> = target_nits
        .iter()
        .flat_map(|nits| {
            let code = (pq_encode(*nits) * 255.0).round() as u8;
            [code, code, code]
        })
        .collect();
    let src = ImageSpec::new(PixFmt::Rgb24, target_nits.len() as u32, 1)
        .with_color(rgb(
            ColorPrimaries::Bt2020,
            TransferCharacteristic::Smpte2084,
        ))
        .with_hdr_peaks(Some(source_peak), None);
    let dst = ImageSpec::new(PixFmt::Rgb24, target_nits.len() as u32, 1)
        .with_color(rgb(ColorPrimaries::Bt709, TransferCharacteristic::Bt709))
        .with_hdr_peaks(Some(target_peak), None);
    let mut output = vec![0u8; input.len()];
    let mut scaler = Scaler::new(&src, &dst, &ScaleOptions::default()).expect("build tone LUT");
    scaler
        .scale_planes(
            &[SrcPlane {
                data: &input,
                stride: input.len(),
            }],
            &mut [DstPlane {
                data: &mut output,
                stride: input.len(),
            }],
        )
        .expect("run tone LUT");
    let mut max_error = 0u8;
    for (index, source) in input.chunks_exact(3).enumerate() {
        let nits = pq_decode(f64::from(source[0]) / 255.0);
        let expected = (bt709_encode(
            bt2390(nits, f64::from(source_peak), f64::from(target_peak)) / f64::from(target_peak),
        ) * 255.0)
            .round()
            .clamp(0.0, 255.0) as u8;
        let actual = output[index * 3];
        max_error = max_error.max(actual.abs_diff(expected));
        assert!(
            actual.abs_diff(expected) <= 4,
            "patch {index}: actual {actual}, expected {expected}"
        );
        assert_eq!(output[index * 3 + 1], actual, "neutral green {index}");
        assert_eq!(output[index * 3 + 2], actual, "neutral blue {index}");
    }
    println!("BT.2390 LUT max error against independent oracle: {max_error} LSB");
}
