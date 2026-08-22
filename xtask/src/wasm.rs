//! Every library still builds for `wasm32-unknown-unknown` (D18).
//!
//! # Why per-crate and not `--workspace`
//!
//! `cargo check --workspace --target wasm32-unknown-unknown` fails, and not for
//! a reason that matters: `proptest` pulls `rand_core` and `tempfile` pull
//! `getrandom`, which `compile_error!`s on wasm without its `js` feature. Those
//! are **dev**-dependencies — `cargo tree -e normal -i getrandom` finds nothing
//! — so no shipped library is affected, but a workspace-wide check resolves one
//! unified feature graph and drags them in anyway.
//!
//! Building each library on its own is therefore both the accurate question and
//! the one we can actually answer: *does this crate, as shipped, compile for
//! wasm?* Test binaries are explicitly out of scope; we do not run the suite on
//! wasm.
//!
//! # The allowlist is the interesting part
//!
//! A crate is portable unless it is on [`NATIVE_ONLY`]. That default is the
//! point: a new crate is portable, and making one native-only is a deliberate,
//! reviewed act that leaves a note saying why — the same shape as the unsafe
//! audit's exemption list. The alternative default would let OS coupling spread
//! silently, which is exactly what D18 exists to prevent.

use std::process::Command;

use crate::{Task, crates, repo_root};

/// Crates that legitimately cannot build for wasm, each with the reason.
///
/// Empty today, and worth keeping that way. `vaco-time` exists so that the
/// clock — the one thing that genuinely panics on wasm — is behind a single
/// door instead of being a reason to add entries here.
const NATIVE_ONLY: &[(&str, &str)] = &[(
    "vaco-protocol-http",
    "ureq + rustls is a socket-and-TLS stack; wasm32-unknown-unknown has no \
     sockets, and a browser port would go through fetch rather than this crate. \
     Portability here means a *different* protocol implementation behind the \
     same `vaco-protocol-core` trait, which is the D11 adapter rule doing its \
     job — not a wasm build of this one.",
)];

const TARGET: &str = "wasm32-unknown-unknown";

pub fn run(_check: bool) -> Task {
    let root = repo_root();

    // Fail loudly rather than passing vacuously when the target is missing.
    let installed = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .map_err(|e| format!("rustup: {e}"))?;
    if !String::from_utf8_lossy(&installed.stdout).contains(TARGET) {
        return Err(format!(
            "the {TARGET} target is not installed; run `rustup target add {TARGET}`"
        ));
    }

    let mut failed = Vec::new();
    let mut checked = 0_usize;

    for (_layer, name, path) in crates() {
        if let Some((_, why)) = NATIVE_ONLY.iter().find(|(n, _)| *n == name) {
            println!("  skip {name}: {why}");
            continue;
        }
        // Binary-only crates have no library to check; `--lib` errors on them.
        if !path.join("src/lib.rs").exists() {
            continue;
        }
        let out = Command::new("cargo")
            .current_dir(&root)
            .args([
                "build",
                "-p",
                &name,
                "--lib",
                "--target",
                TARGET,
                "--target-dir",
                "/tmp/vaco-wasm",
                "-q",
            ])
            .output()
            .map_err(|e| format!("cargo: {e}"))?;
        checked += 1;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let first = err
                .lines()
                .find(|l| l.starts_with("error"))
                .unwrap_or("(no error line)")
                .to_string();
            // Blame the crate the error is IN, not the one we asked cargo for.
            // Building `-p a` also builds `a`'s dependencies, so the first error
            // is often in a different crate — and a gate that names the wrong
            // one sends people to the wrong file. Take it from the `-->` path.
            let blame = err
                .lines()
                .find_map(|l| l.trim().strip_prefix("--> "))
                .and_then(|loc| loc.split('/').nth(2))
                .map_or_else(|| name.clone(), str::to_string);
            failed.push((blame, first));
        }
    }

    if !failed.is_empty() {
        let mut msg = format!(
            "{} crate(s) no longer build for {TARGET} (D18):\n",
            failed.len()
        );
        for (name, why) in &failed {
            msg.push_str(&format!("  {name}: {why}\n"));
        }
        msg.push_str(
            "\nPut the OS-coupled part behind an abstraction rather than adding \
             an entry to NATIVE_ONLY. `vaco-time` is the worked example: the \
             clock is the one API that genuinely panics on wasm, and it lives in \
             one crate so the port is one file.",
        );
        return Err(msg);
    }
    println!("wasm-check: {checked} libraries build for {TARGET}");
    // Mid-wave this can report an agent's half-written crate as a wasm failure.
    // That is not a false positive worth engineering away: the gate's job is the
    // committed tree, and CI is where it runs.
    Ok(())
}
