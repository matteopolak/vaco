//! Separable FIR motion compensation benchmarks: scalar reference vs the
//! dispatched vector implementation, at a realistic block row width.
//!
//! ```text
//! cargo bench -p vaco-codec-dsp-mc
//! ```

use vaco_codec_dsp_mc::fir::{self, taps};
use vaco_simd::Caps;

fn main() {
    divan::main();
}

/// A 16-sample output row (one 16x16 luma block's width, the common case for
/// H.264/HEVC full-block interpolation) plus the five-sample halo a six-tap
/// filter needs on the right.
fn src_row() -> Vec<u8> {
    (0..16 + 5).map(|i| ((i * 53) & 0xFF) as u8).collect()
}

#[divan::bench]
fn h264_halfpel_scalar_16px(bencher: divan::Bencher<'_, '_>) {
    let src = src_row();
    bencher.bench(|| fir::fir_row_scalar(divan::black_box(&src), &taps::H264_LUMA_HALFPEL, 16));
}

#[divan::bench]
fn h264_halfpel_dispatched_16px(bencher: divan::Bencher<'_, '_>) {
    let src = src_row();
    let caps = Caps::detect();
    bencher.bench(|| {
        let mut dst = vec![0u8; 16];
        fir::fir_row(
            caps,
            divan::black_box(&src),
            &taps::H264_LUMA_HALFPEL,
            &mut dst,
        );
        divan::black_box(dst);
    });
}

#[divan::bench]
fn bilinear_scalar_16px(bencher: divan::Bencher<'_, '_>) {
    let src = src_row();
    bencher.bench(|| fir::fir_row_scalar(divan::black_box(&src), &taps::BILINEAR, 16));
}

#[divan::bench]
fn bilinear_dispatched_16px(bencher: divan::Bencher<'_, '_>) {
    let src = src_row();
    let caps = Caps::detect();
    bencher.bench(|| {
        let mut dst = vec![0u8; 16];
        fir::fir_row(caps, divan::black_box(&src), &taps::BILINEAR, &mut dst);
        divan::black_box(dst);
    });
}

/// A full 1920px row, the throughput case rather than the one-block case
/// above — closer to how a row-at-a-time MC kernel is actually called.
#[divan::bench]
fn h264_halfpel_scalar_1920px(bencher: divan::Bencher<'_, '_>) {
    let src: Vec<u8> = (0..1920 + 5).map(|i| ((i * 53) & 0xFF) as u8).collect();
    bencher.bench(|| fir::fir_row_scalar(divan::black_box(&src), &taps::H264_LUMA_HALFPEL, 1920));
}

#[divan::bench]
fn h264_halfpel_dispatched_1920px(bencher: divan::Bencher<'_, '_>) {
    let src: Vec<u8> = (0..1920 + 5).map(|i| ((i * 53) & 0xFF) as u8).collect();
    let caps = Caps::detect();
    bencher.bench(|| {
        let mut dst = vec![0u8; 1920];
        fir::fir_row(
            caps,
            divan::black_box(&src),
            &taps::H264_LUMA_HALFPEL,
            &mut dst,
        );
        divan::black_box(dst);
    });
}
