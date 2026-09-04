#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing assertion in a test is a failing test"
)]

use std::process::Command;

#[test]
fn bench_cli_measures_a_real_kernel_and_writes_jsonl() {
    let path = std::env::temp_dir().join(format!(
        "vaco-checkasm-real-kernel-{}.jsonl",
        std::process::id()
    ));
    let output = Command::new(env!("CARGO_BIN_EXE_vaco-checkasm"))
        .args([
            "bench",
            "--test",
            "vaco-simd::ops::select_u8",
            "--bench-cache",
            "hot",
            "--min-samples",
            "30",
            "--budget",
            "50",
            "--json",
            path.to_str().expect("temporary path is UTF-8"),
        ])
        .output()
        .expect("run checkasm bench mode");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("vaco-simd::ops::select_u8"));
    assert!(stdout.contains("backend=instant unit=ns"));

    let jsonl = std::fs::read_to_string(&path).expect("read benchmark JSONL");
    std::fs::remove_file(path).expect("remove benchmark JSONL");
    assert_eq!(jsonl.lines().count(), 2);
    assert!(
        jsonl
            .lines()
            .all(|line| line.starts_with('{') && line.ends_with('}'))
    );
    assert!(jsonl.contains("\"nop_median\":"));
    assert!(jsonl.contains("\"nop_iterations\":"));
    assert!(jsonl.contains("\"cache\":\"hot\""));
    assert!(jsonl.contains("\"unit\":\"ns\""));
}
