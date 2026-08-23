//! Vaco developer tasks.
//!
//! Deliberately dependency-free: this binary gates the build, so it must compile
//! before anything else and must not itself be able to violate the policies it
//! enforces.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

mod dead_code;
mod deps;
mod docs;
mod dup_check;
mod gen_fuzz;
mod gen_pixfmt;
mod layers;
mod patent_gate;
mod registry;
mod time_gate;
mod unsafe_audit;
mod wasm;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let task = args.first().map(String::as_str).unwrap_or_default();
    let check = args.iter().any(|a| a == "--check");

    let result = match task {
        "layer-check" => layers::run(),
        "dep-gate" => deps::run(),
        "unsafe-audit" => unsafe_audit::run(),
        "dup-check" => dup_check::run(check),
        "dead-code" => dead_code::run(check),
        "wasm-check" => wasm::run(check),
        "time-gate" => time_gate::run(check),
        "patent-gate" => patent_gate::run(check),
        "gen-registry" => registry::run(check),
        "gen-docs-index" => docs::run(check),
        "gen-pixfmt" => gen_pixfmt::run(check),
        "gen-fuzz" => gen_fuzz::run(check),
        other => {
            eprintln!("unknown task: {other}\n");
            eprintln!("tasks:");
            eprintln!("  layer-check     crate graph is acyclic and points downward");
            eprintln!("  dep-gate        D10 Gate 1: no FFI, no vendored C");
            eprintln!("  unsafe-audit    `unsafe` only where D2/D13 permit");
            eprintln!("  wasm-check      every library still builds for wasm32 (D18)");
            eprintln!("  time-gate       the OS clock is reached only through vaco-time (D18)");
            eprintln!("  patent-gate     no encumbered component is in the default build (D4)");
            eprintln!("  dup-check       one definition per concept (D19)");
            eprintln!("  dead-code       public API that only tests use (report, not a gate)");
            eprintln!("  gen-registry    assemble the registry from crate fragments");
            eprintln!("  gen-docs-index  generate docs/README.md");
            eprintln!(
                "  gen-pixfmt      expand the pixel-format families into the committed table"
            );
            eprintln!("  gen-fuzz        assemble fuzz/Cargo.toml from target front-matter");
            std::process::exit(2);
        }
    };

    match result {
        Ok(()) => {}
        Err(e) => {
            eprintln!("\x1b[31m{task} FAILED\x1b[0m\n{e}");
            std::process::exit(1);
        }
    }
}

/// Repository root, found by walking up from the manifest directory.
fn repo_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p
}

/// Every crate directory under `crates/`, as (layer_dir, crate_name, path).
fn crates() -> Vec<(String, String, PathBuf)> {
    let root = repo_root().join("crates");
    let mut out = Vec::new();
    let Ok(layers) = std::fs::read_dir(&root) else {
        return out;
    };
    for layer in layers.flatten() {
        if !layer.path().is_dir() {
            continue;
        }
        let layer_name = layer.file_name().to_string_lossy().into_owned();
        let Ok(members) = std::fs::read_dir(layer.path()) else {
            continue;
        };
        for m in members.flatten() {
            if m.path().join("Cargo.toml").exists() {
                let name = m.file_name().to_string_lossy().into_owned();
                out.push((layer_name.clone(), name, m.path()));
            }
        }
    }
    out.sort();
    out
}

/// All `.rs` files under a directory.
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Run a command and capture stdout, or explain why it could not.
fn capture(cmd: &mut Command) -> Result<String, String> {
    let out = cmd.output().map_err(|e| format!("could not run: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

type Task = Result<(), String>;

pub(crate) use {BTreeMap as Map, BTreeSet as Set};
