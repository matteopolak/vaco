//! Throughput of the per-sample conversion and interleave kernels, at a
//! frame size representative of one AAC/Opus superframe (1024 samples,
//! stereo).
//!
//! ```text
//! cargo bench -p vaco-codec-dsp-fmtconvert
//! ```

use vaco_codec_dsp_fmtconvert::{float_to_int16, int16_to_float, int32_to_float, interleave_f32, simd};
use vaco_simd::Caps;

fn main() {
    divan::main();
}

const N: usize = 1024;

fn ramp_i16() -> [i16; N] {
    core::array::from_fn(|i| {
        let i = i16::try_from(i % 65536).unwrap_or(0);
        i.wrapping_sub(512)
    })
}

fn ramp_f32() -> [f32; N] {
    core::array::from_fn(|i| (i as f32) * 0.001 - 0.5)
}

fn ramp_i32() -> [i32; N] {
    core::array::from_fn(|i| {
        let i = i32::try_from(i).unwrap_or(0);
        i.wrapping_mul(104_729).wrapping_sub(500_000_000)
    })
}

#[divan::bench]
fn int16_to_float_scalar_1024(bencher: divan::Bencher<'_, '_>) {
    let src = ramp_i16();
    let mut dst = [0.0f32; N];
    bencher
        .counter(divan::counter::ItemsCount::new(N))
        .bench_local(|| int16_to_float(&mut dst, &src));
}

#[divan::bench]
fn int16_to_float_dispatched_1024(bencher: divan::Bencher<'_, '_>) {
    // Benchmarks the dispatched body directly (`int16_to_float_vector`),
    // not the crate's public `int16_to_float` entry -- that entry is
    // gated to the scalar path below (measured slower; see
    // `src/simd.rs`'s module doc), so calling it here would compare the
    // scalar loop against itself.
    let src = ramp_i16();
    let mut dst = [0.0f32; N];
    let caps = Caps::detect();
    bencher
        .counter(divan::counter::ItemsCount::new(N))
        .bench_local(|| simd::int16_to_float_vector(caps, &src, &mut dst));
}

#[divan::bench]
fn int32_to_float_scalar_1024(bencher: divan::Bencher<'_, '_>) {
    let src = ramp_i32();
    let mut dst = [0.0f32; N];
    bencher
        .counter(divan::counter::ItemsCount::new(N))
        .bench_local(|| int32_to_float(&mut dst, &src));
}

#[divan::bench]
fn int32_to_float_dispatched_1024(bencher: divan::Bencher<'_, '_>) {
    let src = ramp_i32();
    let mut dst = [0.0f32; N];
    let caps = Caps::detect();
    bencher
        .counter(divan::counter::ItemsCount::new(N))
        .bench_local(|| simd::int32_to_float(caps, &src, &mut dst));
}

#[divan::bench]
fn float_to_int16_1024(bencher: divan::Bencher<'_, '_>) {
    let src = ramp_f32();
    let mut dst = [0i16; N];
    bencher
        .counter(divan::counter::ItemsCount::new(N))
        .bench_local(|| float_to_int16(&mut dst, &src));
}

#[divan::bench]
fn interleave_stereo_1024(bencher: divan::Bencher<'_, '_>) {
    let left = ramp_f32();
    let right = ramp_f32();
    let mut dst = [0.0f32; N * 2];
    bencher
        .counter(divan::counter::ItemsCount::new(N * 2))
        .bench_local(|| interleave_f32(&mut dst, &[&left, &right]));
}
