#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "a failing assertion in a test is a failing test"
)]

use std::process::{Command, Output};

fn command(name: &str) -> Command {
    match name {
        "vvmpeg" => Command::new(env!("CARGO_BIN_EXE_vvmpeg")),
        "vvprobe" => Command::new(env!("CARGO_BIN_EXE_vvprobe")),
        _ => panic!("unknown facade command: {name}"),
    }
}

fn output(name: &str, arguments: &[&str]) -> Output {
    command(name)
        .args(arguments)
        .output()
        .expect("run facade command")
}

#[test]
fn installed_commands_brand_their_help() {
    let vvmpeg = output("vvmpeg", &["--help"]);
    let vvprobe = output("vvprobe", &["--help"]);
    let vvmpeg_stderr = String::from_utf8_lossy(&vvmpeg.stderr);
    let vvprobe_stdout = String::from_utf8_lossy(&vvprobe.stdout);
    assert!(vvmpeg.status.success(), "{vvmpeg:?}");
    assert!(vvprobe.status.success(), "{vvprobe:?}");
    assert!(vvmpeg_stderr.contains("vvmpeg version"), "{vvmpeg_stderr}");
    assert!(
        vvprobe_stdout.contains("usage: vvprobe "),
        "{vvprobe_stdout}"
    );
}

#[test]
fn vvmpeg_preserves_a_user_path_containing_legacy_name() {
    let output = output(
        "vvmpeg",
        &["-i", "vaco-user-path-does-not-exist.mp4", "-f", "null", "-"],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{output:?}");
    assert!(stderr.contains("vvmpeg version"), "{stderr}");
    assert!(
        stderr.contains("vaco-user-path-does-not-exist.mp4"),
        "{stderr}"
    );
}
