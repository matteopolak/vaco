#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "a failing assertion in a test is a failing test"
)]

use std::fs;
use std::process::Command;

#[test]
fn remarks_subcommand_reports_a_configured_passed_remark() {
    let root = std::env::temp_dir().join(format!("vaco-vecheck-cli-{}", std::process::id()));
    fs::create_dir_all(&root).expect("create temporary directory");
    let config = root.join("vecheck.toml");
    let remarks = root.join("remarks.yaml");
    fs::write(
        &config,
        "[[kernel]]\nid = \"demo\"\nvariant = \"x8\"\nsymbol = \"demo::hot\"\npackage = \"demo\"\n",
    )
    .expect("write config");
    fs::write(
        &remarks,
        "--- !Passed\nPass: loop-vectorize\nFunction: \"demo::hot::h1\"\n...\n",
    )
    .expect("write remarks");

    let output = Command::new(env!("CARGO_BIN_EXE_vaco-vecheck"))
        .args([
            "remarks",
            "--config",
            config.to_str().expect("UTF-8 path"),
            "--remarks",
            remarks.to_str().expect("UTF-8 path"),
            "--today",
            "2026-09-04",
        ])
        .output()
        .expect("run vecheck");
    fs::remove_dir_all(root).expect("remove temporary directory");

    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("demo: vectorized"));
}
