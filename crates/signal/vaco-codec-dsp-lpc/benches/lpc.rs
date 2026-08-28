//! Throughput of the analysis and synthesis paths at FLAC's default block
//! size (4096 samples) and maximum order (32).
//!
//! ```text
//! cargo bench -p vaco-codec-dsp-lpc
//! ```

use vaco_codec_dsp_lpc::{autocorrelate, levinson_durbin, quantize, synthesize};

fn main() {
    divan::main();
}

const BLOCK: usize = 4096;
const ORDER: usize = 32;

fn ramp() -> Vec<f64> {
    (0..BLOCK).map(|i| (i as f64 * 0.01).sin() * 1000.0).collect()
}

#[divan::bench]
fn autocorrelate_order32(bencher: divan::Bencher<'_, '_>) {
    let samples = ramp();
    let mut out = vec![0.0; ORDER + 1];
    bencher
        .counter(divan::counter::ItemsCount::new(BLOCK * ORDER))
        .bench_local(|| autocorrelate(&samples, &mut out));
}

#[divan::bench]
fn levinson_durbin_order32(bencher: divan::Bencher<'_, '_>) {
    let samples = ramp();
    let mut autoc = vec![0.0; ORDER + 1];
    autocorrelate(&samples, &mut autoc);
    bencher
        .counter(divan::counter::ItemsCount::new(ORDER))
        .bench_local(|| levinson_durbin(&autoc, ORDER));
}

#[divan::bench]
fn synthesize_order32(bencher: divan::Bencher<'_, '_>) {
    let samples = ramp();
    let mut autoc = vec![0.0; ORDER + 1];
    autocorrelate(&samples, &mut autoc);
    let ld = levinson_durbin(&autoc, ORDER);
    let q = quantize(ld.coefficients(ORDER), 15);
    let warmup = vec![0i32; ORDER];
    let residual = vec![1i64; BLOCK - ORDER];
    let mut out = vec![0i64; BLOCK];
    bencher
        .counter(divan::counter::ItemsCount::new(BLOCK))
        .bench_local(|| synthesize(&warmup, &residual, q.coefficients(), q.shift, &mut out));
}
