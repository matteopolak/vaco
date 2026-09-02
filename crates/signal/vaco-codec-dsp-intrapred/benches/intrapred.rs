//! Throughput of the three prediction primitives at a representative
//! transform-block size (32, HEVC's largest).
//!
//! ```text
//! cargo bench -p vaco-codec-dsp-intrapred
//! ```

use vaco_codec_dsp_intrapred::{angular_project, dc_predict, planar_predict, simd};
use vaco_simd::Caps;

fn main() {
    divan::main();
}

const SIZE: usize = 32;

#[divan::bench]
fn dc_predict_scalar_32(bencher: divan::Bencher<'_, '_>) {
    let top = [128u16; SIZE];
    let left = [64u16; SIZE];
    bencher.bench_local(|| dc_predict(divan::black_box(&top), divan::black_box(&left), SIZE, 8));
}

#[divan::bench]
fn dc_predict_dispatched_32(bencher: divan::Bencher<'_, '_>) {
    let top = [128u16; SIZE];
    let left = [64u16; SIZE];
    let caps = Caps::detect();
    bencher.bench_local(|| {
        simd::dc_predict(
            caps,
            divan::black_box(&top),
            divan::black_box(&left),
            SIZE,
            8,
        )
    });
}

#[divan::bench]
fn planar_predict_32(bencher: divan::Bencher<'_, '_>) {
    let top = [100u16; SIZE];
    let left = [50u16; SIZE];
    let mut dst = [0u16; SIZE * SIZE];
    bencher.bench_local(|| planar_predict(&mut dst, &top, &left, 100, 50, SIZE, 5));
}

#[divan::bench]
fn angular_project_row_32(bencher: divan::Bencher<'_, '_>) {
    let refs: [u16; 96] = core::array::from_fn(|i| (i % 256) as u16);
    let mut dst = [0u16; SIZE];
    bencher.bench_local(|| angular_project(&mut dst, &refs, 15, 13));
}
