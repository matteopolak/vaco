//! Transform benchmarks.
//!
//! Two kinds of number here, and they answer different questions.
//!
//! **Absolute throughput** (`fft_f32`, `mdct_f32`, …) is what a codec pays.
//! Sizes are the ones real bitstreams ask for: AAC LC at 2048/256, AC-3 at
//! 512/256, Opus/CELT at 960/480/240/120 — that last family being the
//! mixed-radix 2·3·5 path, which is exactly the case a power-of-two-only FFT
//! would leave a codec to solve for itself.
//!
//! **The `stages` group** answers plan 17 §C.6.3's open question. Its bodies are
//! the *same plan* run twice, once through the vector kernels and once forced
//! scalar, so the ratio isolates the kernels from everything else. In a Stockham
//! flow the stages that cannot be vectorised are the ones where the
//! sub-transform count `s` is below the lane width — and because the planner
//! emits the largest radix first, that is the **first** stage, not the last
//! `log₂(lanes)`. See `docs/signal/vaco-tx.md`.
//!
//! ```text
//! cargo bench -p vaco-tx
//! ```

use std::sync::Arc;
use vaco_tx::{Direction, Plan, Tx, TxFlags, TxKind, TxSample, fixed};

fn main() {
    divan::main();
}

/// Deterministic input, so a run is comparable with the run before it.
fn ramp<T: Copy>(n: usize, f: impl Fn(usize) -> T) -> Vec<T> {
    (0..n).map(f).collect()
}

fn float_input(n: usize) -> Vec<f32> {
    ramp(n, |i| ((i as f32) * 0.7391).sin())
}

fn f64_input(n: usize) -> Vec<f64> {
    ramp(n, |i| ((i as f64) * 0.7391).sin())
}

fn fixed_input(n: usize) -> Vec<i32> {
    ramp(n, |i| (((i as f64) * 0.7391).sin() * 1.6e9) as i32)
}

struct Harness<T: TxSample> {
    tx: Tx<T>,
    input: Vec<T>,
    output: Vec<T>,
}

impl<T: TxSample> Harness<T> {
    fn run(&mut self) {
        self.tx.execute(&mut self.output, &self.input);
    }
}

fn harness<T: TxSample>(
    kind: TxKind,
    dir: Direction,
    len: usize,
    flags: TxFlags,
    scale: T::Scale,
    make: impl Fn(usize) -> Vec<T>,
) -> Option<Harness<T>> {
    let plan = Plan::<T>::new(kind, dir, len, scale, flags).ok()?;
    let input = make(plan.input_len());
    let output = vec![T::default(); plan.output_len()];
    Some(Harness {
        tx: Tx::new(Arc::clone(&plan)),
        input,
        output,
    })
}

// --- 1, 4, 11: the complex FFT, across every decomposition rule the codecs hit.

#[divan::bench(args = [
    64usize, 120, 128, 240, 256, 480, 512, 960, 1024, 4096, 8192, 32768,
])]
fn fft_f32(bencher: divan::Bencher<'_, '_>, n: usize) {
    let Some(mut h) = harness::<f32>(
        TxKind::Fft,
        Direction::Forward,
        n,
        TxFlags::empty(),
        1.0,
        float_input,
    ) else {
        return;
    };
    bencher.bench_local(|| h.run());
}

/// The awkward lengths: primes, prime powers and coprime composites, i.e.
/// Rader, Bluestein and Good–Thomas. Not what codecs ask for, but what the
/// totality promise costs when someone does.
#[divan::bench(args = [97usize, 121, 143, 251, 1021, 2809])]
fn fft_f32_awkward(bencher: divan::Bencher<'_, '_>, n: usize) {
    let Some(mut h) = harness::<f32>(
        TxKind::Fft,
        Direction::Forward,
        n,
        TxFlags::empty(),
        1.0,
        float_input,
    ) else {
        return;
    };
    bencher.bench_local(|| h.run());
}

// --- 8: f64, used by analysis filters.

#[divan::bench(args = [1024usize])]
fn fft_f64(bencher: divan::Bencher<'_, '_>, n: usize) {
    let Some(mut h) = harness::<f64>(
        TxKind::Fft,
        Direction::Forward,
        n,
        TxFlags::empty(),
        1.0,
        f64_input,
    ) else {
        return;
    };
    bencher.bench_local(|| h.run());
}

// --- 2, 3: MDCT. AAC LC is 2048/256; AC-3 is 512/256; Opus is 960/480.

#[divan::bench(args = [256usize, 512, 960, 2048])]
fn mdct_f32(bencher: divan::Bencher<'_, '_>, n: usize) {
    let Some(mut h) = harness::<f32>(
        TxKind::Mdct,
        Direction::Forward,
        n,
        TxFlags::empty(),
        1.0,
        float_input,
    ) else {
        return;
    };
    bencher.bench_local(|| h.run());
}

#[divan::bench(args = [256usize, 512, 960, 2048])]
fn imdct_f32(bencher: divan::Bencher<'_, '_>, n: usize) {
    let Some(mut h) = harness::<f32>(
        TxKind::Mdct,
        Direction::Inverse,
        n,
        TxFlags::empty(),
        1.0,
        float_input,
    ) else {
        return;
    };
    bencher.bench_local(|| h.run());
}

// --- 5, 6: RDFT and DCT-II.

#[divan::bench(args = [512usize, 2048])]
fn rdft_f32(bencher: divan::Bencher<'_, '_>, n: usize) {
    let Some(mut h) = harness::<f32>(
        TxKind::Rdft,
        Direction::Forward,
        n,
        TxFlags::empty(),
        1.0,
        float_input,
    ) else {
        return;
    };
    bencher.bench_local(|| h.run());
}

#[divan::bench(args = [32usize, 512])]
fn dct2_f32(bencher: divan::Bencher<'_, '_>, n: usize) {
    let Some(mut h) = harness::<f32>(
        TxKind::Dct,
        Direction::Forward,
        n,
        TxFlags::empty(),
        1.0,
        float_input,
    ) else {
        return;
    };
    bencher.bench_local(|| h.run());
}

// --- 7: the fixed-point path. Expected slower than float; the point is to know
//        by how much, since a fixed-point codec has no alternative.

#[divan::bench(args = [64usize, 128, 256, 512, 1024])]
fn fft_i32(bencher: divan::Bencher<'_, '_>, n: usize) {
    let Some(mut h) = harness::<i32>(
        TxKind::Fft,
        Direction::Forward,
        n,
        TxFlags::empty(),
        fixed::ONE,
        fixed_input,
    ) else {
        return;
    };
    bencher.bench_local(|| h.run());
}

#[divan::bench(args = [256usize, 512, 2048])]
fn mdct_i32(bencher: divan::Bencher<'_, '_>, n: usize) {
    let Some(mut h) = harness::<i32>(
        TxKind::Mdct,
        Direction::Forward,
        n,
        TxFlags::empty(),
        fixed::ONE,
        fixed_input,
    ) else {
        return;
    };
    bencher.bench_local(|| h.run());
}

// --- 9: cold plan setup. A decoder that builds plans per stream cares.

#[divan::bench(args = [256usize, 960, 2048])]
fn plan_new_f32(bencher: divan::Bencher<'_, '_>, n: usize) {
    bencher.bench_local(|| Plan::<f32>::fft(divan::black_box(n), false).is_ok());
}

#[divan::bench(args = [97usize, 1021])]
fn plan_new_f32_prime(bencher: divan::Bencher<'_, '_>, n: usize) {
    bencher.bench_local(|| Plan::<f32>::fft(divan::black_box(n), false).is_ok());
}

/// Plan 17 §C.6.3's measurement: what the un-vectorisable stages actually cost.
///
/// `vector` runs the plan normally; `scalar` forces every stage through the
/// scalar kernels. The ratio is the whole SIMD win, and the *residual* — how far
/// `vector` sits above a hypothetical fully-vectorised transform — is bounded by
/// the fraction of stages with `s < lanes`, which for these lengths is one stage
/// out of three to five.
mod stages {
    use super::{TxFlags, float_input, harness};
    use vaco_tx::{Direction, TxKind};

    #[divan::bench(args = [64usize, 256, 1024, 4096])]
    fn vector(bencher: divan::Bencher<'_, '_>, n: usize) {
        let Some(mut h) = harness::<f32>(
            TxKind::Fft,
            Direction::Forward,
            n,
            TxFlags::empty(),
            1.0,
            float_input,
        ) else {
            return;
        };
        bencher.bench_local(|| h.run());
    }

    #[divan::bench(args = [64usize, 256, 1024, 4096])]
    fn scalar(bencher: divan::Bencher<'_, '_>, n: usize) {
        let Some(mut h) = harness::<f32>(
            TxKind::Fft,
            Direction::Forward,
            n,
            TxFlags::empty(),
            1.0,
            float_input,
        ) else {
            return;
        };
        h.tx.set_scalar_reference(true);
        bencher.bench_local(|| h.run());
    }
}
