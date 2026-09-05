//! H.264 MC contract benchmarks: the pre-contract scalar call shapes against
//! the resolved whole-row/block entries used by the decoder.

#![allow(
    clippy::indexing_slicing,
    reason = "benchmark loops use fixed-size arrays and statically bounded indices"
)]

use vaco_codec_dsp_mc::fir::{self, taps};
use vaco_codec_dsp_mc::h264::{BiWeight, ChromaJob, H264McKernels, UniWeight};
use vaco_simd::KernelSet;

fn main() {
    divan::main();
}

fn clip(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

fn luma_source() -> [[u8; 21]; 21] {
    core::array::from_fn(|y| core::array::from_fn(|x| ((x * 53 + y * 97 + x * y * 7) & 255) as u8))
}

#[divan::bench]
fn luma_raw_scalar_21_rows(bencher: divan::Bencher<'_, '_>) {
    let src = luma_source();
    bencher.bench(|| {
        let src = divan::black_box(&src);
        let mut out = [[0i32; 16]; 21];
        for (source, dest) in src.iter().zip(out.iter_mut()) {
            for (x, value) in dest.iter_mut().enumerate() {
                *value = fir::tap_sum(
                    source.get(x..x + 6).unwrap_or(&[]),
                    &taps::H264_LUMA_HALFPEL.coeffs,
                );
            }
        }
        divan::black_box(out)
    });
}

#[divan::bench]
fn luma_raw_kernel_21_rows(bencher: divan::Bencher<'_, '_>) {
    let src = luma_source();
    let kernels = H264McKernels::select();
    bencher.bench(|| {
        let mut out = [[0i32; 16]; 21];
        (kernels.luma_half_raw)(divan::black_box(&src), 16, 21, &mut out);
        divan::black_box(out)
    });
}

fn chroma_source(index: usize) -> [[u8; 3]; 3] {
    core::array::from_fn(|y| {
        core::array::from_fn(|x| ((index * 31 + x * 47 + y * 73 + x * y * 11) & 255) as u8)
    })
}

fn chroma_sample(src: &[[u8; 3]; 3], x: usize, y: usize, fx: i32, fy: i32) -> u8 {
    let sum = (8 - fx) * (8 - fy) * i32::from(src[y][x])
        + fx * (8 - fy) * i32::from(src[y][x + 1])
        + (8 - fx) * fy * i32::from(src[y + 1][x])
        + fx * fy * i32::from(src[y + 1][x + 1]);
    clip((sum + 32) >> 6)
}

#[divan::bench]
fn chroma_scalar_16_blocks(bencher: divan::Bencher<'_, '_>) {
    let src: [_; 16] = core::array::from_fn(chroma_source);
    bencher.bench(|| {
        let src = divan::black_box(&src);
        let mut out = [[[0u8; 2]; 2]; 16];
        for (index, block) in out.iter_mut().enumerate() {
            let fx = divan::black_box(i32::try_from(index & 7).unwrap_or(0));
            let fy = divan::black_box(i32::try_from((index * 3) & 7).unwrap_or(0));
            for (y, row) in block.iter_mut().enumerate() {
                for (x, sample) in row.iter_mut().enumerate() {
                    *sample = chroma_sample(&src[index], x, y, fx, fy);
                }
            }
        }
        divan::black_box(out)
    });
}

#[divan::bench]
fn chroma_kernel_16_blocks(bencher: divan::Bencher<'_, '_>) {
    let src: [_; 16] = core::array::from_fn(chroma_source);
    let jobs: [_; 16] = core::array::from_fn(|index| ChromaJob {
        src: src[index],
        frac_x: (index & 7) as u8,
        frac_y: ((index * 3) & 7) as u8,
    });
    let kernels = H264McKernels::select();
    bencher.bench(|| {
        let mut out = [[[0u8; 2]; 2]; 16];
        (kernels.chroma_batch)(divan::black_box(&jobs), &mut out);
        divan::black_box(out)
    });
}

fn pred_row(seed: usize) -> [u8; 256] {
    core::array::from_fn(|i| ((i * 67 + seed * 101 + i * seed * 3) & 255) as u8)
}

#[divan::bench]
fn weighted_uni_scalar_256(bencher: divan::Bencher<'_, '_>) {
    let src = pred_row(3);
    bencher.bench(|| {
        let src = divan::black_box(&src);
        let weight = divan::black_box(15i32);
        let offset = divan::black_box(-3i32);
        let denom = divan::black_box(4u8);
        let round = 1i32 << (denom - 1);
        let out = src.map(|sample| {
            clip(((i32::from(sample) * weight + round) >> denom).saturating_add(offset))
        });
        divan::black_box(out)
    });
}

#[divan::bench]
fn weighted_uni_kernel_256(bencher: divan::Bencher<'_, '_>) {
    let src = pred_row(3);
    let kernels = H264McKernels::select();
    bencher.bench(|| {
        let mut out = [0u8; 256];
        (kernels.weight_uni)(
            divan::black_box(&src),
            256,
            &mut out,
            256,
            256,
            1,
            divan::black_box(UniWeight {
                weight: 15,
                offset: -3,
                log2_denom: 4,
            }),
        );
        divan::black_box(out)
    });
}

#[divan::bench]
fn weighted_bi_scalar_256(bencher: divan::Bencher<'_, '_>) {
    let src0 = pred_row(3);
    let src1 = pred_row(11);
    bencher.bench(|| {
        let src0 = divan::black_box(&src0);
        let src1 = divan::black_box(&src1);
        let weight0 = divan::black_box(48i32);
        let weight1 = divan::black_box(16i32);
        let denom = divan::black_box(5u8);
        let out = core::array::from_fn::<_, 256, _>(|i| {
            clip(
                (i32::from(src0[i]) * weight0 + i32::from(src1[i]) * weight1 + (1i32 << denom))
                    >> (denom + 1),
            )
        });
        divan::black_box(out)
    });
}

#[divan::bench]
fn weighted_bi_kernel_256(bencher: divan::Bencher<'_, '_>) {
    let src0 = pred_row(3);
    let src1 = pred_row(11);
    let kernels = H264McKernels::select();
    bencher.bench(|| {
        let mut out = [0u8; 256];
        (kernels.weight_bi)(
            divan::black_box(&src0),
            256,
            divan::black_box(&src1),
            256,
            &mut out,
            256,
            256,
            1,
            divan::black_box(BiWeight {
                weight0: 48,
                weight1: 16,
                offset: 0,
                log2_denom: 5,
            }),
        );
        divan::black_box(out)
    });
}
