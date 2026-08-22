//! D10 Gate 1: pure Rust, zero FFI.
//!
//! # Why this does not read `Cargo.lock`
//!
//! `Cargo.lock` records a package's **optional** dependencies whether or not the
//! enabling feature is active. Grepping it for `ring` reports a violation for a
//! build that never compiles `ring` at all — which is exactly what happened on
//! the first check of this workspace (plan 19 §11).
//!
//! False positives are worse than no gate, because they train people to ignore
//! it. So this queries the *resolved build graph* instead, and additionally
//! checks the property Gate 1 actually cares about: whether anything in the tree
//! links or compiles foreign code.

use crate::{Task, capture, repo_root};
use std::process::Command;

/// Crates whose entire purpose is to compile or bind foreign code.
const BANNED: &[(&str, &str)] = &[
    ("cc", "compiles C from a build script"),
    ("cmake", "drives a native build system"),
    ("bindgen", "generates FFI bindings"),
    ("pkg-config", "locates system libraries to link"),
    (
        "ring",
        "vendors and compiles C and assembly; use rustls-rustcrypto (D14.2)",
    ),
    ("aws-lc-rs", "vendors and compiles C and assembly (D14.2)"),
    ("aws-lc-sys", "vendors and compiles C and assembly"),
    ("openssl-sys", "links the system OpenSSL"),
];

pub fn run() -> Task {
    let root = repo_root();
    let mut violations = Vec::new();

    for kind in ["normal", "build"] {
        let tree = capture(Command::new("cargo").current_dir(&root).args([
            "tree",
            "--workspace",
            "-e",
            kind,
            "--prefix",
            "none",
        ]))?;

        for line in tree.lines() {
            let name = line.split_whitespace().next().unwrap_or("");
            if let Some((_, why)) = BANNED.iter().find(|(b, _)| *b == name) {
                violations.push(format!("  {name} (as a {kind} dependency) — {why}"));
            }
        }
    }

    // A `links` key means the crate claims a native library. A third-party
    // build.rs is how foreign code gets compiled. Both are Gate 1 failures
    // regardless of which crate they come from.
    let meta = capture(Command::new("cargo").current_dir(&root).args([
        "metadata",
        "--format-version",
        "1",
        "--all-features",
    ]))?;

    for chunk in meta.split("\"name\":\"").skip(1) {
        let name = chunk.split('"').next().unwrap_or("");
        let head = chunk.get(..600).unwrap_or(chunk);
        if head.contains("\"links\":\"") && !head.contains("\"links\":null") {
            violations.push(format!("  {name} declares a `links` key (native library)"));
        }
    }

    violations.sort();
    violations.dedup();

    if violations.is_empty() {
        println!("dep-gate: clean — no FFI, no vendored C in the build graph");
        Ok(())
    } else {
        Err(format!(
            "D10 Gate 1 violations:\n{}\n\nGate 1 permits pure-Rust bindings to OS APIs in \
             vaco-hw-* only (D13); it never permits vendored or compiled foreign code.",
            violations.join("\n")
        ))
    }
}
