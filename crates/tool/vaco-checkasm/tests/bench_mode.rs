#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing assertion in a test is a failing test"
)]

use std::time::Duration;

use vaco_checkasm::Kernel;
use vaco_checkasm::bench::{
    BenchConfig, CacheState, apply_baseline, benchmark, load_baseline, summarize, write_jsonl,
};

#[derive(Debug, Clone, Copy)]
struct SumKernel;

impl Kernel for SumKernel {
    const NAME: &'static str = "test::sum";
    type Case = Vec<u8>;
    type Lane = u64;

    fn cases() -> Vec<Self::Case> {
        vec![(0..=255).collect()]
    }

    fn scalar(case: &Self::Case) -> Vec<Self::Lane> {
        vec![case.iter().map(|&value| u64::from(value)).sum()]
    }

    fn vector(case: &Self::Case) -> Vec<Self::Lane> {
        vec![
            case.chunks(16)
                .flatten()
                .map(|&value| u64::from(value))
                .sum(),
        ]
    }
}

#[test]
fn statistics_include_median_mad_min_and_p95() {
    let stats = summarize(&[9.0, 1.0, 3.0, 5.0, 7.0]).expect("non-empty sample set");
    assert!((stats.median - 5.0).abs() < f64::EPSILON);
    assert!((stats.mad - 2.0).abs() < f64::EPSILON);
    assert!((stats.min - 1.0).abs() < f64::EPSILON);
    assert!((stats.p95 - 9.0).abs() < f64::EPSILON);
}

#[test]
fn benchmark_reports_one_honest_metric_nop_and_both_cache_states() {
    let config = BenchConfig {
        min_samples: 3,
        budget: Duration::from_millis(100),
        cache_states: vec![CacheState::Hot, CacheState::Cold],
        warmup_calls: 2,
        cold_bytes: 64 * 1024,
        target_sample_time: Duration::from_micros(20),
    };

    let results = benchmark::<SumKernel>(&config).expect("synthetic benchmark runs");

    assert_eq!(results.len(), 4);
    assert!(results.iter().all(|result| {
        (result.backend == "instant" && result.unit == "ns")
            || (result.backend == "perf-event" && result.unit == "cycles")
    }));
    assert!(results.iter().all(|result| result.samples >= 3));
    assert!(results.iter().all(|result| result.iterations > 0));
    assert!(results.iter().all(|result| result.nop_iterations > 0));
    assert!(results.iter().all(|result| result.nop.median >= 0.0));
    assert!(
        results
            .iter()
            .any(|result| result.cache_state == CacheState::Hot)
    );
    assert!(
        results
            .iter()
            .any(|result| result.cache_state == CacheState::Cold)
    );
}

#[test]
fn jsonl_baseline_comparison_is_unit_and_identity_matched() {
    let config = BenchConfig {
        min_samples: 3,
        budget: Duration::from_millis(100),
        cache_states: vec![CacheState::Hot],
        warmup_calls: 2,
        cold_bytes: 64 * 1024,
        target_sample_time: Duration::from_micros(20),
    };
    let mut results = benchmark::<SumKernel>(&config).expect("synthetic benchmark runs");
    let path = std::env::temp_dir().join(format!(
        "vaco-checkasm-bench-mode-{}.jsonl",
        std::process::id()
    ));
    write_jsonl(&path, &results).expect("write JSONL");
    let baseline = load_baseline(&path).expect("read our own JSONL");
    apply_baseline(&mut results, &baseline);

    assert!(
        results
            .iter()
            .all(|result| result.baseline_ratio == Some(1.0))
    );

    let mismatched = std::fs::read_to_string(&path)
        .expect("read JSONL")
        .replace("\"unit\":\"ns\"", "\"unit\":\"cycles\"");
    std::fs::write(&path, mismatched).expect("write mismatched baseline");
    let baseline = load_baseline(&path).expect("parse mismatched baseline");
    for result in &mut results {
        result.baseline_ratio = None;
    }
    apply_baseline(&mut results, &baseline);
    std::fs::remove_file(path).expect("remove test JSONL");
    assert!(results.iter().all(|result| result.baseline_ratio.is_none()));
}
