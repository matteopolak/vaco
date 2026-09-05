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

fn bt709_decode(code: f64) -> f64 {
    if code < 0.081 {
        code / 4.5
    } else {
        ((code + 0.099) / 1.099).powf(1.0 / 0.45)
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

fn lab_from_bt709(code: [u8; 3]) -> [f64; 3] {
    let [r, g, b] = code.map(|value| bt709_decode(f64::from(value) / 255.0));
    let x = 0.412_390_8 * r + 0.357_584_3 * g + 0.180_480_8 * b;
    let y = 0.212_639 * r + 0.715_168_7 * g + 0.072_192_3 * b;
    let z = 0.019_330_8 * r + 0.119_194_8 * g + 0.950_532_2 * b;
    let f = |value: f64| {
        const EPSILON: f64 = 216.0 / 24_389.0;
        const KAPPA: f64 = 24_389.0 / 27.0;
        if value > EPSILON {
            value.cbrt()
        } else {
            (KAPPA * value + 16.0) / 116.0
        }
    };
    let fx = f(x / 0.950_47);
    let fy = f(y);
    let fz = f(z / 1.088_83);
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn delta_e00(left: [f64; 3], right: [f64; 3]) -> f64 {
    let [l1, a1, b1] = left;
    let [l2, a2, b2] = right;
    let chroma = |a: f64, b: f64| a.hypot(b);
    let c1 = chroma(a1, b1);
    let c2 = chroma(a2, b2);
    let c_bar = (c1 + c2) * 0.5;
    let c_bar_7 = c_bar.powi(7);
    let g = 0.5 * (1.0 - (c_bar_7 / (c_bar_7 + 25.0_f64.powi(7))).sqrt());
    let a1_prime = (1.0 + g) * a1;
    let a2_prime = (1.0 + g) * a2;
    let c1_prime = chroma(a1_prime, b1);
    let c2_prime = chroma(a2_prime, b2);
    let hue = |a: f64, b: f64| b.atan2(a).to_degrees().rem_euclid(360.0);
    let h1 = hue(a1_prime, b1);
    let h2 = hue(a2_prime, b2);
    let delta_l = l2 - l1;
    let delta_c = c2_prime - c1_prime;
    let raw_hue = h2 - h1;
    let delta_hue = if c1_prime == 0.0 || c2_prime == 0.0 {
        0.0
    } else if raw_hue > 180.0 {
        raw_hue - 360.0
    } else if raw_hue < -180.0 {
        raw_hue + 360.0
    } else {
        raw_hue
    };
    let delta_h = 2.0 * (c1_prime * c2_prime).sqrt() * (delta_hue.to_radians() * 0.5).sin();
    let l_bar = (l1 + l2) * 0.5;
    let c_bar_prime = (c1_prime + c2_prime) * 0.5;
    let h_bar = if c1_prime == 0.0 || c2_prime == 0.0 {
        h1 + h2
    } else if (h1 - h2).abs() <= 180.0 {
        (h1 + h2) * 0.5
    } else if h1 + h2 < 360.0 {
        (h1 + h2 + 360.0) * 0.5
    } else {
        (h1 + h2 - 360.0) * 0.5
    };
    let t = 1.0 - 0.17 * (h_bar - 30.0).to_radians().cos()
        + 0.24 * (2.0 * h_bar).to_radians().cos()
        + 0.32 * (3.0 * h_bar + 6.0).to_radians().cos()
        - 0.20 * (4.0 * h_bar - 63.0).to_radians().cos();
    let l_term = (l_bar - 50.0).powi(2);
    let s_l = 1.0 + 0.015 * l_term / (20.0 + l_term).sqrt();
    let s_c = 1.0 + 0.045 * c_bar_prime;
    let s_h = 1.0 + 0.015 * c_bar_prime * t;
    let rotation = -2.0
        * (c_bar_prime.powi(7) / (c_bar_prime.powi(7) + 25.0_f64.powi(7))).sqrt()
        * (60.0 * (-((h_bar - 275.0) / 25.0).powi(2)).exp())
            .to_radians()
            .sin();
    ((delta_l / s_l).powi(2)
        + (delta_c / s_c).powi(2)
        + (delta_h / s_h).powi(2)
        + rotation * (delta_c / s_c) * (delta_h / s_h))
        .sqrt()
}

#[test]
fn bt2390_pq_to_sdr_matches_an_independent_peak_aware_oracle() {
    let source_peak = 1_000u32;
    let target_peak = 100u32;
    // RGB values are absolute component luminances.  Keeping BT.709 primaries
    // in this oracle isolates the independently specified BT.2390 EETF from
    // the separately-tested primary conversion stage.
    let patches = [
        [1.0, 1.0, 1.0],
        [15.0, 3.0, 2.0],
        [3.0, 30.0, 5.0],
        [4.0, 8.0, 50.0],
        [100.0, 40.0, 20.0],
        [1_000.0, 100.0, 10.0],
        [100.0, 600.0, 30.0],
        [40.0, 60.0, 100.0],
    ];
    let input: Vec<u8> = patches
        .iter()
        .flat_map(|rgb_nits| rgb_nits.map(|nits| (pq_encode(nits) * 255.0).round() as u8))
        .collect();
    let src = ImageSpec::new(PixFmt::Rgb24, patches.len() as u32, 1)
        .with_color(rgb(
            ColorPrimaries::Bt709,
            TransferCharacteristic::Smpte2084,
        ))
        .with_hdr_peaks(Some(source_peak), None);
    let dst = ImageSpec::new(PixFmt::Rgb24, patches.len() as u32, 1)
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
    let mut sum_squared_error = 0.0;
    let mut max_delta_e = 0.0_f64;
    for (index, source) in input.chunks_exact(3).enumerate() {
        let source_nits = [
            pq_decode(f64::from(source[0]) / 255.0),
            pq_decode(f64::from(source[1]) / 255.0),
            pq_decode(f64::from(source[2]) / 255.0),
        ];
        let source_luminance = 0.212_639 * source_nits[0]
            + 0.715_168_7 * source_nits[1]
            + 0.072_192_3 * source_nits[2];
        let scale = bt2390(
            source_luminance,
            f64::from(source_peak),
            f64::from(target_peak),
        ) / source_luminance;
        let expected = source_nits.map(|nits| {
            (bt709_encode((nits * scale) / f64::from(target_peak)) * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8
        });
        let actual = [output[index * 3], output[index * 3 + 1], output[index * 3 + 2]];
        for channel in 0..3 {
            let error = actual[channel].abs_diff(expected[channel]);
            max_error = max_error.max(error);
            sum_squared_error += f64::from(error).powi(2);
        }
        let delta_e = delta_e00(lab_from_bt709(actual), lab_from_bt709(expected));
        max_delta_e = max_delta_e.max(delta_e);
        assert!(
            actual.iter().zip(expected).all(|(actual, expected)| actual.abs_diff(expected) <= 4),
            "patch {index}: actual {actual:?}, expected {expected:?}, delta E00 {delta_e:.4}"
        );
    }
    let mean_squared_error = sum_squared_error / input.len() as f64;
    let psnr = 10.0 * ((255.0_f64 * 255.0) / mean_squared_error).log10();
    println!(
        "BT.2390 independent HDR patch oracle: max {max_error} LSB, PSNR {psnr:.3} dB, max DeltaE00 {max_delta_e:.4}"
    );
    assert!(psnr >= 40.0, "BT.2390 patch PSNR {psnr:.3} dB is below 40 dB");
    assert!(
        max_delta_e <= 1.0,
        "BT.2390 patch max DeltaE00 {max_delta_e:.4} exceeds 1.0"
    );
}
