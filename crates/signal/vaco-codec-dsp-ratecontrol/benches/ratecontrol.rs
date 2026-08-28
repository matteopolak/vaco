//! Per-call cost of `next_qscale`/`report` under each mode. Rate control
//! runs once per frame, not per pixel, so the interesting number is
//! "negligible compared to encoding a frame," not raw throughput — this
//! exists mainly to catch a future change that accidentally makes either
//! call allocate or grow unboundedly expensive.

use vaco_codec_dsp_ratecontrol::{FrameReport, RateControlConfig, RateController};
use vaco_core::Rational;

const FPS: Rational = Rational { num: 30, den: 1 };

#[divan::bench]
fn cbr_step(bencher: divan::Bencher<'_, '_>) {
    let mut rc = RateController::new(RateControlConfig::cbr(2_000_000, FPS));
    let mut i = 0u32;
    bencher.bench_local(|| {
        i = i.wrapping_add(1);
        let complexity = 1.0 + 0.1 * f64::from(i % 7);
        let qscale = rc.next_qscale(divan::black_box(complexity));
        rc.report(FrameReport {
            bits: 60_000,
            qscale,
        });
    });
}

#[divan::bench]
fn vbr_step(bencher: divan::Bencher<'_, '_>) {
    let mut rc = RateController::new(RateControlConfig::vbr(2_000_000, 4_000_000, FPS));
    let mut i = 0u32;
    bencher.bench_local(|| {
        i = i.wrapping_add(1);
        let complexity = 1.0 + 0.1 * f64::from(i % 7);
        let qscale = rc.next_qscale(divan::black_box(complexity));
        rc.report(FrameReport {
            bits: 60_000,
            qscale,
        });
    });
}

fn main() {
    divan::main();
}
