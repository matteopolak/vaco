//! One generated Divan case per filter in the production registry.

#![allow(
    clippy::unit_arg,
    reason = "black_box must consume a successful benchmark iteration"
)]

use divan::{Bencher, black_box};
use vaco_bench::{BenchmarkSandbox, FilterCase};

fn main() -> std::process::ExitCode {
    let Ok(_sandbox) = BenchmarkSandbox::enter() else {
        eprintln!("vaco-bench: could not enter temporary benchmark directory");
        return std::process::ExitCode::from(2);
    };
    divan::main();
    std::process::ExitCode::SUCCESS
}

#[divan::bench(args = vaco_bench::filter_cases())]
fn instantiate(bencher: Bencher<'_, '_>, case: &FilterCase) {
    bencher.bench(|| black_box(vaco_bench::instantiate_filter(black_box(case))));
}
