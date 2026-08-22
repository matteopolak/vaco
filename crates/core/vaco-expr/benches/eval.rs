//! Evaluation throughput of an already-compiled expression.
//!
//! This is the number that matters: filters evaluate the same expression once
//! per frame — or, for `aeval`, once per *sample* — so the per-evaluation cost
//! is on a hot path while the parse cost is paid once at graph setup.
//!
//! The design consequence being measured here is the split between [`Expr`]
//! (parsed once, immutable, shareable) and [`Context`] (borrowed per call, no
//! allocation). Variable names are resolved to slice indices at parse time, so
//! evaluation never compares a string.
//!
//! Run with `cargo bench -p vaco-expr`.
#![allow(
    missing_debug_implementations,
    unreachable_pub,
    reason = "benchmark harness"
)]

use divan::counter::ItemsCount;
use vaco_expr::{Bindings, Context, Expr, Registers};

fn main() {
    divan::main();
}

/// Expressions taken from shapes that appear on real command lines.
const CASES: &[(&str, &str)] = &[
    ("const_fold_free", "1280"),
    ("scale_dar", "if(gt(a,16/9),1280,-1)"),
    ("drawtext_centre", "(w-tw)/2"),
    ("fade_ramp", "min(max((t-1)/2,0),1)"),
    ("trig", "sin(t*PI*2)*h/4+h/2"),
    ("registers", "st(0,t*2);ld(0)+ld(0)*ld(0)"),
    (
        "deep",
        "clip(lerp(w,h,mod(t,1))+between(t,0,10)*gcd(w,h),0,4096)",
    ),
];

const NAMES: &[&str] = &["a", "w", "h", "t", "tw"];
const VALUES: &[f64] = &[16.0 / 9.0, 1920.0, 1080.0, 3.5, 240.0];

#[divan::bench(args = CASES)]
fn eval(bencher: divan::Bencher<'_, '_>, case: &(&str, &str)) {
    let expr = compile(case.1);
    bencher
        .counter(ItemsCount::new(1usize))
        .with_inputs(Registers::new)
        .bench_local_refs(|regs| expr.eval_with(&mut Context::new(VALUES, regs)));
}

/// The same expressions parsed, so the setup cost is visible next to the
/// per-frame cost rather than guessed at.
#[divan::bench(args = CASES)]
fn parse(bencher: divan::Bencher<'_, '_>, case: &(&str, &str)) {
    let src = case.1;
    let bindings = Bindings::new(NAMES);
    bencher
        .counter(ItemsCount::new(1usize))
        .bench(|| Expr::parse(divan::black_box(src), &bindings).map(|e| e.node_count()));
}

/// A per-frame loop: one compiled expression, 10 000 evaluations with changing
/// variables and a register file that survives between them. This is the shape
/// a filter actually runs.
#[divan::bench]
fn frame_loop(bencher: divan::Bencher<'_, '_>) {
    const FRAMES: usize = 10_000;
    let expr = compile("st(0,t*2);min(max(ld(0),0),h)");
    bencher
        .counter(ItemsCount::new(FRAMES))
        .with_inputs(Registers::new)
        .bench_local_refs(|regs| {
            let mut acc = 0.0f64;
            for n in 0..FRAMES {
                #[allow(clippy::cast_precision_loss, reason = "benchmark input")]
                let t = n as f64 / 25.0;
                let vars = [16.0 / 9.0, 1920.0, 1080.0, t, 240.0];
                acc += expr.eval_with(&mut Context::new(&vars, regs));
            }
            acc
        });
}

fn compile(src: &str) -> Expr {
    match Expr::parse(src, &Bindings::new(NAMES)) {
        Ok(e) => e,
        // A benchmark that silently measured nothing would be worse than a
        // loud failure, and `unwrap` is denied workspace-wide.
        Err(e) => {
            eprintln!("benchmark expression {src:?} failed to parse: {e}");
            std::process::exit(1)
        }
    }
}
