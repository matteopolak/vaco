//! Reachability checks for all four rendering intents.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    reason = "end-to-end rendering-intent assertions use compact raw RGB fixtures"
)]

use vaco_color::{
    ColorInfo, ColorPrimaries, ColorRange, MatrixCoefficients, TransferCharacteristic,
};
use vaco_pixfmt::PixFmt;
use vaco_scale::exec::{DstPlane, SrcPlane};
use vaco_scale::{ImageSpec, RenderingIntent, ScaleOptions, Scaler};

fn color(primaries: ColorPrimaries) -> ColorInfo {
    ColorInfo {
        primaries,
        transfer: TransferCharacteristic::Bt709,
        matrix: MatrixCoefficients::Identity,
        range: ColorRange::Full,
        ..ColorInfo::default()
    }
}

fn convert(pixel: [u8; 3], intent: RenderingIntent) -> [u8; 3] {
    let src = ImageSpec::new(PixFmt::Rgb24, 1, 1).with_color(color(ColorPrimaries::Smpte431));
    let dst = ImageSpec::new(PixFmt::Rgb24, 1, 1).with_color(color(ColorPrimaries::Bt709));
    let options = ScaleOptions {
        intent,
        ..ScaleOptions::default()
    };
    let mut scaler = Scaler::new(&src, &dst, &options).expect("build intent plan");
    let mut output = [0u8; 3];
    scaler
        .scale_planes(
            &[SrcPlane {
                data: &pixel,
                stride: 3,
            }],
            &mut [DstPlane {
                data: &mut output,
                stride: 3,
            }],
        )
        .expect("run intent plan");
    output
}

#[test]
fn each_rendering_intent_reaches_a_distinct_policy() {
    // DCI-P3 has a non-D65 white, so the two colorimetric intents are
    // discriminated by this neutral rather than collapsing to the same result.
    let neutral = [160, 160, 160];
    let relative = convert(neutral, RenderingIntent::RelativeColorimetric);
    let absolute = convert(neutral, RenderingIntent::AbsoluteColorimetric);
    assert_ne!(
        relative, absolute,
        "absolute intent must not Bradford-adapt white"
    );

    // This saturated P3 red is outside BT.709. The two non-colorimetric
    // policies map that excess through their own LUTs rather than silently
    // falling back to the relative clip path.
    let saturated = [255, 16, 16];
    let clipped = convert(saturated, RenderingIntent::RelativeColorimetric);
    let perceptual = convert(saturated, RenderingIntent::Perceptual);
    let saturation = convert(saturated, RenderingIntent::Saturation);
    assert_ne!(
        perceptual, clipped,
        "perceptual intent must use its LUT policy"
    );
    assert_ne!(
        saturation, clipped,
        "saturation intent must use its LUT policy"
    );
    assert_ne!(
        perceptual, saturation,
        "perceptual and saturation must not alias"
    );
}
