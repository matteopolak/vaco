//! Fuzzing `vaco-codec-dsp-lpc`'s full analysis-to-synthesis pipeline for
//! panics on arbitrary input.
//!
//! Every stage here eventually feeds attacker-controlled bitstream data in
//! a real decoder: `autocorrelate`/`levinson_durbin` run over decoded PCM
//! (bounded by frame size, but a hostile sample value is fully in the
//! caller's control), and `quantize`/`predict`/`synthesize` are exactly the
//! path a malformed LPC subframe exercises — arbitrary coefficients,
//! arbitrary shift, arbitrary history. No property beyond panic-freedom is
//! checked over the fully arbitrary domain, matching `dsp_idct`'s own
//! reasoning: out-of-conformance input has no defined output to check
//! against.
//! fuzz-crate: vaco-codec-dsp-lpc
#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use vaco_codec_dsp_lpc::{autocorrelate, levinson_durbin, predict, quantize, synthesize};

#[derive(Arbitrary, Debug)]
struct Input {
    samples: Vec<f64>,
    max_lag: u8,
    max_order: u8,
    precision: u32,
    predict_history: Vec<i32>,
    predict_coeffs: Vec<i32>,
    shift: u32,
    warmup: Vec<i32>,
    residual: Vec<i64>,
}

fuzz_target!(|input: Input| {
    // Cap collection sizes so a single input cannot make the target spend
    // unbounded time or memory — the property under test is panic-freedom,
    // not throughput, and a huge `Vec<f64>` from the fuzzer's byte budget
    // would otherwise dominate every run's cost for no additional coverage.
    let samples: Vec<f64> = input.samples.into_iter().take(4096).collect();
    let mut autoc = vec![0.0; usize::from(input.max_lag).min(64) + 1];
    autocorrelate(&samples, &mut autoc);

    let ld = levinson_durbin(&autoc, usize::from(input.max_order).min(64));
    for order in 0..=usize::from(input.max_order) {
        let _ = ld.coefficients(order);
        let _ = ld.error(order);
        let _ = ld.reflection(order);
    }

    let q = quantize(ld.coefficients(ld.order_computed()), input.precision);
    let _ = q.coefficients();

    let history: Vec<i32> = input.predict_history.into_iter().take(64).collect();
    let coeffs: Vec<i32> = input.predict_coeffs.into_iter().take(64).collect();
    let _ = predict(&history, &coeffs, input.shift);

    let warmup: Vec<i32> = input.warmup.into_iter().take(64).collect();
    let residual: Vec<i64> = input.residual.into_iter().take(4096).collect();
    let mut out = vec![0i64; warmup.len() + residual.len()];
    synthesize(&warmup, &residual, &coeffs, input.shift, &mut out);

    // The analysis-to-synthesis round trip a real encoder/decoder pair
    // actually performs: quantise the coefficients this exact input's
    // autocorrelation produced, then synthesize with them.
    let mut out2 = vec![0i64; warmup.len() + residual.len()];
    synthesize(&warmup, &residual, q.coefficients(), q.shift, &mut out2);
});
