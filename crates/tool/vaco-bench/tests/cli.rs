#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "integration-test diagnostics need exact process and output failures"
)]

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_path(label: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("vaco-bench-cli-{label}-{nonce}.jsonl"))
}

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vaco-bench"))
}

#[test]
fn list_is_registry_complete() {
    let output = command().arg("list").output().expect("run list command");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    let listed: std::collections::BTreeSet<_> = stdout.lines().collect();
    let expected: std::collections::BTreeSet<_> = vaco_registry::filters()
        .iter()
        .map(|filter| filter.name)
        .collect();
    assert_eq!(listed, expected);
}

#[cfg(not(target_os = "linux"))]
#[test]
fn machine_check_fails_closed_off_a_controlled_linux_runner() {
    let output = command()
        .arg("machine-check")
        .output()
        .expect("run machine check command");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 error output");
    assert!(stderr.contains("not ready"));
    assert!(stderr.contains("controlled Linux reference runner"));
}

#[test]
fn hidden_child_control_runs_one_registry_derived_batch() {
    let list = command().arg("list").output().expect("run list command");
    let stdout = String::from_utf8(list.stdout).expect("UTF-8 list output");
    let name = stdout.lines().next().expect("registry has one filter");
    let output = command()
        .args(["__filter-batch", "control", name, "3", "created"])
        .output()
        .expect("run hidden child command");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn filter_writes_one_jsonl_row_per_registered_filter() {
    let path = temp_path("measurement");
    let output = command()
        .args([
            "filter",
            "--warmup",
            "0",
            "--samples",
            "1",
            "--target-sample-ns",
            "1",
            "--max-iterations",
            "1",
            "--json",
        ])
        .arg(&path)
        .output()
        .expect("run filter command");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let jsonl = std::fs::read_to_string(&path).expect("read JSONL output");
    assert_eq!(jsonl.lines().count(), vaco_registry::filters().len());
    assert!(jsonl.lines().all(|line| {
        line.contains("\"scope\":\"instantiate\"")
            && line.contains("\"backend\":\"instant\"")
            && line.contains("\"unit\":\"ns\"")
            && line.contains("\"raw_median\":")
            && line.contains("\"control_median\":null")
    }));
    std::fs::remove_file(path).expect("remove JSONL output");
}

#[test]
fn filter_rejects_an_unknown_measurement_backend() {
    let output = command()
        .args(["filter", "--backend", "stopwatch"])
        .output()
        .expect("run filter command");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 error output");
    assert!(stderr.contains("--backend requires instant, auto, or perf-stat"));
}

#[cfg(not(target_os = "linux"))]
#[test]
fn forced_perf_stat_reports_that_the_platform_is_unsupported() {
    let output = command()
        .args([
            "filter",
            "--backend",
            "perf-stat",
            "--warmup",
            "0",
            "--samples",
            "1",
            "--target-sample-ns",
            "1",
            "--max-iterations",
            "1",
        ])
        .output()
        .expect("run filter command");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 error output");
    assert!(stderr.contains("perf-stat CPU cycles require Linux"));
}

#[cfg(not(target_os = "linux"))]
#[test]
fn auto_uses_explicit_instant_nanoseconds_off_linux() {
    let path = temp_path("auto-fallback");
    let output = command()
        .args([
            "filter",
            "--backend",
            "auto",
            "--warmup",
            "0",
            "--samples",
            "1",
            "--target-sample-ns",
            "1",
            "--max-iterations",
            "1",
            "--json",
        ])
        .arg(&path)
        .output()
        .expect("run automatic backend selection");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let jsonl = std::fs::read_to_string(&path).expect("read automatic JSONL output");
    assert!(jsonl.lines().all(|line| {
        line.contains("\"scope\":\"instantiate\"")
            && line.contains("\"backend\":\"instant\"")
            && line.contains("\"unit\":\"ns\"")
    }));
    std::fs::remove_file(path).expect("remove automatic JSONL output");
}

#[test]
fn missing_baseline_is_a_successful_incomparable_first_run() {
    let missing = temp_path("missing");
    let output = command()
        .args([
            "filter",
            "--warmup",
            "0",
            "--samples",
            "1",
            "--target-sample-ns",
            "1",
            "--max-iterations",
            "1",
            "--baseline",
        ])
        .arg(missing)
        .arg("--fail-under")
        .arg("0.95")
        .output()
        .expect("run filter comparison");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 output");
    assert!(stdout.contains("matched=0"));
}

#[test]
fn report_uses_the_same_trailing_history_for_a_latest_row() {
    let input = temp_path("report-history");
    let output = input.with_extension("html");
    std::fs::write(
        &input,
        concat!(
            "{\"schema\":1,\"suite\":\"filter\",\"benchmark\":\"scale\",\"scope\":\"instantiate\",\"outcome\":\"created\",\"backend\":\"instant\",\"unit\":\"ns\",\"samples\":11,\"iterations\":1,\"raw_median\":100,\"raw_mad\":1,\"raw_min\":99,\"raw_p95\":101,\"control_median\":null,\"control_mad\":null,\"control_min\":null,\"control_p95\":null,\"median\":100,\"mad\":1,\"min\":99,\"p95\":101,\"baseline_ratio\":null,\"machine\":\"runner-a\",\"os\":\"linux\",\"arch\":\"x86_64\",\"cpu\":\"test cpu\",\"rustc\":\"rustc test\",\"profile\":\"release\",\"git_sha\":\"first\",\"measured_unix_ms\":1,\"load_average_1m\":null}\n",
            "{\"schema\":1,\"suite\":\"filter\",\"benchmark\":\"scale\",\"scope\":\"instantiate\",\"outcome\":\"created\",\"backend\":\"instant\",\"unit\":\"ns\",\"samples\":11,\"iterations\":1,\"raw_median\":200,\"raw_mad\":2,\"raw_min\":198,\"raw_p95\":202,\"control_median\":null,\"control_mad\":null,\"control_min\":null,\"control_p95\":null,\"median\":200,\"mad\":2,\"min\":198,\"p95\":202,\"baseline_ratio\":null,\"machine\":\"runner-a\",\"os\":\"linux\",\"arch\":\"x86_64\",\"cpu\":\"test cpu\",\"rustc\":\"rustc test\",\"profile\":\"release\",\"git_sha\":\"second\",\"measured_unix_ms\":2,\"load_average_1m\":null}\n"
        ),
    )
    .expect("write report fixture");

    let command_output = command()
        .args([
            "report",
            "--input",
            input.to_str().expect("UTF-8 input path"),
            "--output",
            output.to_str().expect("UTF-8 output path"),
            "--generated-unix-ms",
            "1700000000123",
            "--fail-under",
            "0.95",
        ])
        .output()
        .expect("run report command");

    assert!(
        command_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&command_output.stderr)
    );
    let html = std::fs::read_to_string(&output).expect("read report output");
    assert!(html.contains("2023-11-14T22:13:20.123Z"));
    assert!(html.contains("<td>scale</td>"));
    assert!(html.contains("100.000 ns"));
    assert!(html.contains("0.5000"));
    assert!(html.contains("<td class=\"regression\">regression</td>"));

    std::fs::remove_file(input).expect("remove report fixture");
    std::fs::remove_file(output).expect("remove report output");
}
