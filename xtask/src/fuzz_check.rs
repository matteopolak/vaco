//! Every `fuzz/` target still compiles (D6's build gate).
//!
//! # Why this exists
//!
//! `fuzz/` is its own cargo workspace (deliberately), which means it is built by **neither**
//! `cargo test --workspace` nor `cargo clippy --workspace --all-targets` — the
//! two gates everything else in this project runs through. A target can rot
//! against an ordinary, correct API change in the crate it exercises and
//! nothing before this gate ever noticed: `cargo fuzz build` stops at the
//! first target that fails, so the workspace can accumulate any number of
//! broken targets behind one, and a sweep across all of them reports every one
//! as a "finding" indistinguishable from a real crash. That happened —
//! 217 targets deep, every one of them a build failure, not a bug.
//!
//! # Build-only, not run
//!
//! This runs `cargo +nightly fuzz build`, never `fuzz run`. Actually executing
//! every target for a meaningful duration belongs in a sweep (`just
//! fuzz-all`), not a gate every PR pays for. Compiling is enough to catch what
//! rot looks like: a stale field, a renamed method, a changed signature.
//!
//! # Degrading when nightly is absent
//!
//! Unlike `wasm-check`, which fails loudly when `wasm32-unknown-unknown` is
//! missing (that target is expected in every dev environment this project
//! supports), a nightly toolchain plus `cargo-fuzz` is a heavier, genuinely
//! optional prerequisite — plenty of contributors and CI legs never install
//! either. Failing the whole gate for an absent optional tool would make
//! `just ci` unusable on a stable-only machine, so this checks for both with
//! one probe (`cargo +nightly fuzz --version`) and skips with a clear message
//! rather than erroring when it is not satisfied. The `fuzz-regressions` CI
//! job already installs both, so that leg is where this gate has teeth; a
//! machine without them gets an honest "skipped", not a false pass folded
//! into a green check nobody reads closely enough to notice it proved
//! nothing.

use std::process::Command;

use crate::{Task, repo_root};

/// One probe that answers both "is nightly installed" and "is cargo-fuzz
/// installed" at once, since `cargo +nightly fuzz` fails the same way (a
/// non-zero exit, not a spawn error) whichever one is missing.
fn nightly_fuzz_available() -> bool {
    Command::new("cargo")
        .args(["+nightly", "fuzz", "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn run(_check: bool) -> Task {
    if !nightly_fuzz_available() {
        println!(
            "fuzz-check: skipped — needs a nightly toolchain and cargo-fuzz \
             (`rustup toolchain install nightly` and `cargo install cargo-fuzz \
             --locked`); the CI fuzz-regressions job installs both and runs \
             this gate for real."
        );
        return Ok(());
    }

    let fuzz_dir = repo_root().join("fuzz");
    let out = Command::new("cargo")
        .current_dir(&fuzz_dir)
        .args(["+nightly", "fuzz", "build"])
        .output()
        .map_err(|e| format!("cargo +nightly fuzz build: {e}"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // The last chunk of stderr carries the actual `error[...]` — cargo
        // fuzz's own wrapper messages (rustflags, ASAN_OPTIONS, the full
        // invocation) come after and are noise for whoever reads this gate's
        // failure, so keep the tail rather than the whole stream.
        let tail: Vec<&str> = stderr.lines().rev().take(60).collect();
        let tail: String = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
        return Err(format!(
            "one or more fuzz targets no longer compile (D6):\n{tail}\n\n\
             Reproduce with `cd fuzz && cargo +nightly fuzz build`. Fix the \
             TARGET file, not the crate it exercises — a target that fails \
             here fell behind a deliberate, correct API change; the crate is \
             not the thing to change unless you also own it and the change is \
             independently warranted."
        ));
    }

    let n = std::fs::read_dir(fuzz_dir.join("fuzz_targets"))
        .map(|d| {
            d.filter_map(Result::ok)
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("rs"))
                .count()
        })
        .unwrap_or(0);
    println!("fuzz-check: {n} fuzz targets compile clean");
    Ok(())
}
