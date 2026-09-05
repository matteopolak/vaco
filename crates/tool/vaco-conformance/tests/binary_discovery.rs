//! Exercise binary discovery without mutating the parallel test process's environment.

#![expect(clippy::expect_used, reason = "test failures must report their cause")]

use std::path::PathBuf;
use std::process::Command;

use vaco_conformance::runner::UnderTest;

#[test]
fn discovers_current_names_and_honours_explicit_overrides() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let debug = tmp.path().join("debug");
    std::fs::create_dir(&debug).expect("create debug directory");
    for name in ["vaco", "vaco-probe"] {
        std::fs::write(debug.join(name), b"obsolete executable").expect("legacy fixture");
    }

    let run = |mode: &str| {
        let mut child = Command::new(std::env::current_exe().expect("test executable"));
        child
            .args(["--ignored", "--exact", "discovery_child", "--nocapture"])
            .env("VACO_DISCOVERY_TEST_MODE", mode)
            .env("CARGO_TARGET_DIR", tmp.path())
            .env_remove("VACO_BIN_PROBE")
            .env_remove("VACO_BIN_VACO")
            .env_remove("VACO_BIN_PLAY");
        if matches!(mode, "override" | "missing-override") {
            let name = if mode == "override" {
                "custom"
            } else {
                "missing"
            };
            child
                .env("VACO_BIN_PROBE", tmp.path().join(name))
                .env("VACO_BIN_VACO", tmp.path().join(name));
        }
        let output = child.output().expect("run discovery child");
        assert!(
            output.status.success(),
            "{mode}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    };

    run("legacy-only");
    for name in ["vvmpeg", "vvprobe"] {
        std::fs::write(debug.join(name), b"current executable").expect("current fixture");
    }
    run("current");
    std::fs::write(tmp.path().join("custom"), b"custom executable").expect("override fixture");
    run("override");
    run("missing-override");
}

#[test]
#[ignore = "invoked by the parent test with an isolated environment"]
fn discovery_child() {
    let Ok(mode) = std::env::var("VACO_DISCOVERY_TEST_MODE") else {
        return;
    };
    let root = PathBuf::from(std::env::var_os("CARGO_TARGET_DIR").expect("target directory"));
    let found = UnderTest::discover();
    let expected = |name: &str| match mode.as_str() {
        "current" => Some(root.join("debug").join(name)),
        "override" => Some(root.join("custom")),
        _ => None,
    };
    assert_eq!(found.probe, expected("vvprobe"));
    assert_eq!(found.transcode, expected("vvmpeg"));
}
