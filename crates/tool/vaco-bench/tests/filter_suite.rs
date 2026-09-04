#![allow(
    clippy::expect_used,
    clippy::float_cmp,
    clippy::indexing_slicing,
    reason = "test diagnostics and exact fixture assertions need these operations"
)]

use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use vaco_bench::{
    BenchResult, FilterBenchConfig, MachineFingerprint, MeasurementBackend, Statistics,
    apply_baseline, regressions, run_filter_suite, summarize, write_jsonl,
};

#[test]
fn every_registered_filter_has_one_successful_benchmark_row() {
    let escaped_output = std::env::current_dir()
        .expect("read test working directory")
        .join("transforms.trf");
    assert!(
        !escaped_output.exists(),
        "fixture starts with a clean working directory"
    );
    let config = FilterBenchConfig {
        warmup_calls: 0,
        samples: 1,
        target_sample_ns: 1,
        max_iterations: 1,
        backend: MeasurementBackend::Instant,
    };
    let rows = run_filter_suite(&config).expect("measure the registry");
    let expected: BTreeSet<_> = vaco_registry::filters()
        .iter()
        .map(|filter| filter.name)
        .collect();
    let measured: BTreeSet<_> = rows.iter().map(|row| row.benchmark.as_str()).collect();

    assert_eq!(rows.len(), expected.len());
    assert_eq!(measured, expected);
    assert!(rows.iter().all(|row| row.samples == 1));
    assert!(rows.iter().all(|row| row.scope == "instantiate"));
    assert!(
        rows.iter()
            .all(|row| matches!(row.outcome, "created" | "rejected"))
    );
    assert!(
        !escaped_output.exists(),
        "side-effecting constructors must stay inside the benchmark sandbox"
    );
}

fn result(benchmark: &str, median_ns: f64, machine: &str) -> BenchResult {
    let stats = Statistics {
        median: median_ns,
        mad: 1.0,
        min: median_ns - 1.0,
        p95: median_ns + 1.0,
    };
    BenchResult {
        benchmark: benchmark.to_owned(),
        scope: "instantiate",
        outcome: "created",
        backend: "instant",
        unit: "ns",
        samples: 11,
        iterations: 2,
        stats,
        raw_stats: stats,
        control_stats: None,
        fingerprint: MachineFingerprint {
            machine: machine.to_owned(),
            os: "linux".to_owned(),
            arch: "x86_64".to_owned(),
            cpu: "test cpu".to_owned(),
            rustc: "rustc test".to_owned(),
            profile: "release".to_owned(),
        },
        git_sha: "abc123".to_owned(),
        measured_unix_ms: 1,
        load_average_1m: Some(1.0),
        baseline_ratio: None,
    }
}

#[test]
fn summaries_report_median_dispersion_and_tail() {
    let stats = summarize(&[5.0, 1.0, 3.0, 2.0, 4.0]).expect("non-empty sample set");
    assert_eq!(stats.median, 3.0);
    assert_eq!(stats.mad, 1.0);
    assert_eq!(stats.min, 1.0);
    assert_eq!(stats.p95, 5.0);
}

fn temp_path(label: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("vaco-bench-{label}-{nonce}.jsonl"))
}

#[test]
fn jsonl_baselines_match_the_complete_measurement_identity() {
    let path = temp_path("identity");
    write_jsonl(&path, &[result("scale", 100.0, "runner-a")]).expect("write baseline");

    let mut comparable = vec![result("scale", 50.0, "runner-a")];
    apply_baseline(&mut comparable, &path).expect("read comparable baseline");
    assert_eq!(comparable[0].baseline_ratio, Some(2.0));

    let mut other_machine = vec![result("scale", 50.0, "runner-b")];
    apply_baseline(&mut other_machine, &path).expect("read incomparable baseline");
    assert_eq!(other_machine[0].baseline_ratio, None);

    std::fs::remove_file(path).expect("remove JSONL fixture");
}

#[test]
fn absent_or_incomparable_baselines_never_create_regressions() {
    let missing = temp_path("missing");
    let mut rows = vec![result("scale", 100.0, "runner-a")];
    apply_baseline(&mut rows, &missing).expect("a missing baseline is a first run");
    assert!(regressions(&rows, 0.95).is_empty());

    let path = temp_path("incomparable");
    write_jsonl(&path, &[result("scale", 50.0, "runner-b")]).expect("write baseline");
    apply_baseline(&mut rows, &path).expect("read incomparable baseline");
    assert!(regressions(&rows, 0.95).is_empty());
    std::fs::remove_file(path).expect("remove JSONL fixture");
}

#[test]
fn an_explicit_threshold_only_flags_matched_slower_rows() {
    let path = temp_path("regression");
    write_jsonl(&path, &[result("scale", 80.0, "runner-a")]).expect("write baseline");
    let mut rows = vec![result("scale", 100.0, "runner-a")];
    apply_baseline(&mut rows, &path).expect("read baseline");

    let slower = regressions(&rows, 0.95);
    assert_eq!(slower.len(), 1);
    assert_eq!(slower[0].benchmark, "scale");
    assert_eq!(slower[0].baseline_ratio, Some(0.8));

    std::fs::remove_file(path).expect("remove JSONL fixture");
}

#[test]
fn comparison_uses_the_trailing_seven_matching_measurements() {
    let path = temp_path("rolling-seven");
    let baseline: Vec<_> = (1_i32..=8)
        .map(|value| {
            let mut row = result("scale", f64::from(value), "runner-a");
            row.measured_unix_ms = i64::from(value);
            row
        })
        .collect();
    write_jsonl(&path, &baseline).expect("write baseline history");
    let mut rows = vec![result("scale", 10.0, "runner-a")];
    apply_baseline(&mut rows, &path).expect("read baseline history");

    assert_eq!(rows[0].baseline_ratio, Some(0.5));

    std::fs::remove_file(path).expect("remove JSONL fixture");
}
