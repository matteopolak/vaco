//! Scalar vs. dispatched-vector throughput for the hot comparison kernels,
//! at a realistic inter-prediction block size.

use vaco_codec_dsp_mecmp::{MecmpKernels, Plane};
use vaco_simd::KernelSet;

const W: usize = 16;
const H: usize = 16;

fn buffers() -> (Vec<u8>, Vec<u8>) {
    let cur: Vec<u8> = (0..W * H).map(|i| (i * 7 % 256) as u8).collect();
    let refb: Vec<u8> = (0..W * H).map(|i| (i * 11 % 256) as u8).collect();
    (cur, refb)
}

#[divan::bench]
fn sad_scalar(bencher: divan::Bencher<'_, '_>) {
    let (cur, refb) = buffers();
    let k = MecmpKernels::reference();
    bencher.bench(|| {
        let c = Plane::new(divan::black_box(&cur), W, W, H);
        let r = Plane::new(divan::black_box(&refb), W, W, H);
        (k.sad)(c, r)
    });
}

#[divan::bench]
fn sad_dispatched(bencher: divan::Bencher<'_, '_>) {
    let (cur, refb) = buffers();
    let k = MecmpKernels::select();
    bencher.bench(|| {
        let c = Plane::new(divan::black_box(&cur), W, W, H);
        let r = Plane::new(divan::black_box(&refb), W, W, H);
        (k.sad)(c, r)
    });
}

#[divan::bench]
fn ssd_scalar(bencher: divan::Bencher<'_, '_>) {
    let (cur, refb) = buffers();
    let k = MecmpKernels::reference();
    bencher.bench(|| {
        let c = Plane::new(divan::black_box(&cur), W, W, H);
        let r = Plane::new(divan::black_box(&refb), W, W, H);
        (k.ssd)(c, r)
    });
}

#[divan::bench]
fn ssd_dispatched(bencher: divan::Bencher<'_, '_>) {
    let (cur, refb) = buffers();
    let k = MecmpKernels::select();
    bencher.bench(|| {
        let c = Plane::new(divan::black_box(&cur), W, W, H);
        let r = Plane::new(divan::black_box(&refb), W, W, H);
        (k.ssd)(c, r)
    });
}

#[divan::bench]
fn satd_scalar(bencher: divan::Bencher<'_, '_>) {
    let (cur, refb) = buffers();
    let k = MecmpKernels::reference();
    bencher.bench(|| {
        let c = Plane::new(divan::black_box(&cur), W, W, H);
        let r = Plane::new(divan::black_box(&refb), W, W, H);
        (k.satd)(c, r)
    });
}

fn main() {
    divan::main();
}
