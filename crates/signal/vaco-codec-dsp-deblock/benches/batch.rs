//! Microbenchmark for `#619`'s batched masked-lane-select kernel
//! ([`vaco_codec_dsp_deblock::batch`]) against the per-line scalar loop a
//! caller would otherwise run.
//!
//! Same method as `vaco-simd`'s own `benches/adoption.rs` (that file's doc
//! explains the traps this avoids): both sides are `#[inline(never)]`
//! probes, timed **interleaved** round by round, minimum of N samples, after
//! a ~300ms spin to get promoted off an efficiency core. Run with
//! `cargo bench -p vaco-codec-dsp-deblock`.
//!
//! `docs/core/simd-adoption-measurements.md` records the numbers this
//! produces, including if the result is negative.

#![allow(
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::cast_precision_loss,
    clippy::inline_always,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "benchmark harness: fixed-size local fixtures, not caller-controlled slices"
)]

use std::hint::black_box;
use std::time::Instant;

use vaco_codec_dsp_deblock::batch;
use vaco_codec_dsp_deblock::{
    ChromaLine, EdgeThresholds, LumaLine, filter_chroma_line, filter_luma_line,
};

const ITERS: usize = 2_000;
const REPS: usize = 100;

/// 16 lines' worth of non-flat luma sample data (a real edge's height),
/// with a mixed `bS` pattern (`0..=4`) so every branch the filter takes is
/// exercised -- a flat/no-op-heavy fixture would flatter the branchy
/// scalar path, exactly the mistake `E2E-GAPS.md` already recorded once
/// for this codebase (smptebars passing while mandelbrot was 7.66% wrong).
struct LumaFixture {
    p0: [u8; 16],
    p1: [u8; 16],
    p2: [u8; 16],
    p3: [u8; 16],
    q0: [u8; 16],
    q1: [u8; 16],
    q2: [u8; 16],
    q3: [u8; 16],
    bs: [u8; 16],
    edge: EdgeThresholds,
}

fn luma_fixture() -> LumaFixture {
    // A pseudo-random but fixed texture: a step edge with noise, not a flat
    // plane, so `filterSamplesFlag` and both `ap`/`aq` branches see real
    // variation across the 16 lines.
    let mut state = 0x243fu32;
    let mut next = || {
        state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        (state >> 16) as u8
    };
    let mut p0 = [0u8; 16];
    let mut p1 = [0u8; 16];
    let mut p2 = [0u8; 16];
    let mut p3 = [0u8; 16];
    let mut q0 = [0u8; 16];
    let mut q1 = [0u8; 16];
    let mut q2 = [0u8; 16];
    let mut q3 = [0u8; 16];
    let mut bs = [0u8; 16];
    for i in 0..16 {
        let base = 100u8.wrapping_add((i as u8).wrapping_mul(3));
        p0[i] = base.wrapping_add(next() % 8);
        p1[i] = base.wrapping_add(next() % 6);
        p2[i] = base.wrapping_add(next() % 6);
        p3[i] = base.wrapping_add(next() % 6);
        q0[i] = base.wrapping_add(2).wrapping_add(next() % 8);
        q1[i] = base.wrapping_add(2).wrapping_add(next() % 6);
        q2[i] = base.wrapping_add(2).wrapping_add(next() % 6);
        q3[i] = base.wrapping_add(2).wrapping_add(next() % 6);
        bs[i] = (i % 5) as u8; // 0,1,2,3,4 repeating -- every strength represented
    }
    LumaFixture {
        p0,
        p1,
        p2,
        p3,
        q0,
        q1,
        q2,
        q3,
        bs,
        edge: EdgeThresholds::derive(32, 32, 0, 0),
    }
}

pub(crate) mod probes {
    use super::{ChromaLine, LumaFixture, LumaLine, batch, filter_chroma_line, filter_luma_line};
    use core::num::NonZeroU8;
    use vaco_codec_dsp_deblock::EdgeThresholds;
    use vaco_simd::Caps;

    #[inline(never)]
    pub(crate) fn scalar_luma(
        f: &LumaFixture,
    ) -> ([u8; 16], [u8; 16], [u8; 16], [u8; 16], [u8; 16], [u8; 16]) {
        let mut op0 = [0u8; 16];
        let mut op1 = [0u8; 16];
        let mut op2 = [0u8; 16];
        let mut oq0 = [0u8; 16];
        let mut oq1 = [0u8; 16];
        let mut oq2 = [0u8; 16];
        for i in 0..16 {
            let mut line = LumaLine {
                p: [f.p0[i], f.p1[i], f.p2[i], f.p3[i]],
                q: [f.q0[i], f.q1[i], f.q2[i], f.q3[i]],
            };
            if let Some(bs) = NonZeroU8::new(f.bs[i]) {
                filter_luma_line(&mut line, bs, f.edge);
            }
            op0[i] = line.p[0];
            op1[i] = line.p[1];
            op2[i] = line.p[2];
            oq0[i] = line.q[0];
            oq1[i] = line.q[1];
            oq2[i] = line.q[2];
        }
        (op0, op1, op2, oq0, oq1, oq2)
    }

    #[inline(never)]
    pub(crate) fn batched_luma(
        caps: Caps,
        f: &LumaFixture,
    ) -> ([u8; 16], [u8; 16], [u8; 16], [u8; 16], [u8; 16], [u8; 16]) {
        let mut op0 = f.p0;
        let mut op1 = f.p1;
        let mut op2 = f.p2;
        let mut oq0 = f.q0;
        let mut oq1 = f.q1;
        let mut oq2 = f.q2;
        batch::filter_luma_edge(
            caps, &mut op0, &mut op1, &mut op2, &f.p3, &mut oq0, &mut oq1, &mut oq2, &f.q3, &f.bs,
            f.edge,
        );
        (op0, op1, op2, oq0, oq1, oq2)
    }

    pub(crate) struct ChromaFixture {
        pub p0: [u8; 8],
        pub p1: [u8; 8],
        pub q0: [u8; 8],
        pub q1: [u8; 8],
        pub bs: [u8; 8],
        pub edge: EdgeThresholds,
    }

    #[inline(never)]
    pub(crate) fn scalar_chroma(f: &ChromaFixture) -> ([u8; 8], [u8; 8]) {
        let mut op0 = [0u8; 8];
        let mut oq0 = [0u8; 8];
        for i in 0..8 {
            let mut line = ChromaLine {
                p: [f.p0[i], f.p1[i]],
                q: [f.q0[i], f.q1[i]],
            };
            if let Some(bs) = NonZeroU8::new(f.bs[i]) {
                filter_chroma_line(&mut line, bs, f.edge);
            }
            op0[i] = line.p[0];
            oq0[i] = line.q[0];
        }
        (op0, oq0)
    }

    #[inline(never)]
    pub(crate) fn batched_chroma(caps: Caps, f: &ChromaFixture) -> ([u8; 8], [u8; 8]) {
        let mut op0 = f.p0;
        let mut oq0 = f.q0;
        batch::filter_chroma_edge(caps, &mut op0, &f.p1, &mut oq0, &f.q1, &f.bs, f.edge);
        (op0, oq0)
    }
}

fn chroma_fixture() -> probes::ChromaFixture {
    let mut state = 0x1234_5678u32;
    let mut next = || {
        state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        (state >> 16) as u8
    };
    let mut p0 = [0u8; 8];
    let mut p1 = [0u8; 8];
    let mut q0 = [0u8; 8];
    let mut q1 = [0u8; 8];
    let mut bs = [0u8; 8];
    for i in 0..8 {
        let base = 90u8.wrapping_add((i as u8).wrapping_mul(5));
        p0[i] = base.wrapping_add(next() % 8);
        p1[i] = base.wrapping_add(next() % 6);
        q0[i] = base.wrapping_add(3).wrapping_add(next() % 8);
        q1[i] = base.wrapping_add(3).wrapping_add(next() % 6);
        bs[i] = (i % 5) as u8;
    }
    probes::ChromaFixture {
        p0,
        p1,
        q0,
        q1,
        bs,
        edge: EdgeThresholds::derive(34, 34, 0, 0),
    }
}

/// Spin for ~300ms so macOS promotes this process off an efficiency core
/// before anything is timed -- see `vaco-simd`'s own `adoption.rs` for the
/// measurement this habit came from (a 45ns row measuring 132ns without it).
fn promote_to_performance_core() {
    let t = Instant::now();
    let mut x = 1u64;
    while t.elapsed().as_millis() < 300 {
        for _ in 0..10_000 {
            x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        }
        black_box(x);
    }
}

/// Interleaved A/B timing, minimum of `REPS` samples each.
fn time_pair(mut a: impl FnMut(), mut b: impl FnMut()) -> (f64, f64) {
    for _ in 0..50 {
        a();
        b();
    }
    let (mut best_a, mut best_b) = (f64::INFINITY, f64::INFINITY);
    for _ in 0..REPS {
        let t = Instant::now();
        for _ in 0..ITERS {
            a();
        }
        let na = t.elapsed().as_nanos() as f64 / ITERS as f64;

        let t = Instant::now();
        for _ in 0..ITERS {
            b();
        }
        let nb = t.elapsed().as_nanos() as f64 / ITERS as f64;

        best_a = best_a.min(na);
        best_b = best_b.min(nb);
    }
    (best_a, best_b)
}

fn main() {
    promote_to_performance_core();

    let caps = vaco_simd::Caps::detect();
    println!("tier: {}", caps.tier());

    let luma = luma_fixture();
    let (scalar_ns, batched_ns) = time_pair(
        || {
            black_box(probes::scalar_luma(black_box(&luma)));
        },
        || {
            black_box(probes::batched_luma(caps, black_box(&luma)));
        },
    );
    println!(
        "luma edge (16 lines): scalar {scalar_ns:.1} ns, batched {batched_ns:.1} ns, ratio {:.3}x",
        batched_ns / scalar_ns
    );

    let chroma = chroma_fixture();
    let (scalar_ns, batched_ns) = time_pair(
        || {
            black_box(probes::scalar_chroma(black_box(&chroma)));
        },
        || {
            black_box(probes::batched_chroma(caps, black_box(&chroma)));
        },
    );
    println!(
        "chroma edge (8 lines): scalar {scalar_ns:.1} ns, batched {batched_ns:.1} ns, ratio {:.3}x",
        batched_ns / scalar_ns
    );
}
