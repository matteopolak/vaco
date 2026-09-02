//! Vaco developer tasks.
//!
//! Deliberately dependency-free: this binary gates the build, so it must compile
//! before anything else and must not itself be able to violate the policies it
//! enforces.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

mod comment_check;
mod coverage;
mod dead_code;
mod decoder_coverage;
mod deps;
mod docs;
mod dup_check;
mod fuzz_check;
mod gen_fuzz;
mod gen_pixfmt;
mod layers;
mod option_consumption;
mod owner_gate;
mod patent_gate;
mod provenance;
mod reachability_check;
mod registry;
mod similarity;
mod time_gate;
mod toml;
mod unsafe_audit;
mod vlc_scan;
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
        "comment-check" => comment_check::run(check),
        "dead-code" => dead_code::run(check),
        "wasm-check" => wasm::run(check),
        "time-gate" => time_gate::run(check),
        "patent-gate" => patent_gate::run(check),
        "provenance-check" => provenance::run(check),
        "vlc-scan" => vlc_scan::run(check),
        "fuzz-check" => fuzz_check::run(check),
        "check-message" => check_message(args.get(1).map(String::as_str)),
        "owner-gate" => owner_gate::run(check),
        "reachability-check" => reachability_check::run(check),
        "decoder-coverage" => decoder_coverage::run(check),
        "option-consumption-check" => option_consumption::run(check),
        "gen-registry" => registry::run(check),
        "similarity-scan" => similarity::run(&args[1..]),
        "gen-docs-index" => docs::run(check),
        "gen-coverage" => coverage::run(check),
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
            eprintln!("  owner-gate      each third-party media crate has exactly one owner (D11)");
            eprintln!("  dup-check       one definition per concept (D19)");
            eprintln!(
                "  reachability-check  a component that exists and cannot be reached from the CLI"
            );
            eprintln!(
                "  decoder-coverage  every registered decoder has a decode case or a reviewed reason it does not"
            );
            eprintln!("  comment-check   comments stay short and self-contained");
            eprintln!("  provenance-check  every large constant table names its source (D15)");
            eprintln!(
                "  vlc-scan        hand-transcribed VLC tables are pairwise prefix-free (tier 1 of 3)"
            );
            eprintln!(
                "  fuzz-check      every fuzz/ target still compiles (D6); skips \
                 without nightly+cargo-fuzz"
            );
            eprintln!("  dead-code       public API that only tests use (report, not a gate)");
            eprintln!(
                "  option-consumption-check  a CliOptionTable entry that parses and does \
                 nothing (report, not a gate)"
            );
            eprintln!("  gen-registry    assemble the registry from crate fragments");
            eprintln!("  gen-docs-index  generate docs/README.md");
            eprintln!(
                "  gen-pixfmt      expand the pixel-format families into the committed table"
            );
            eprintln!("  gen-fuzz        assemble fuzz/Cargo.toml from target front-matter");
            eprintln!("  gen-coverage    generate docs/format-coverage.md");
            eprintln!(
                "  similarity-scan winnowing fingerprint scan (QA-08, plan 13 §6.4); \
                 needs --against <corpus-dir>"
            );
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

/// Validate one commit message file's trailers — the `commit-msg` hook.
///
/// Separate from `provenance-check` because it runs before the commit exists,
/// so it reads the message from a file and asks git what is staged.
fn check_message(path: Option<&str>) -> Task {
    let path = path.ok_or("check-message needs a path to the message file")?;
    let body = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    // Comment lines are stripped by git after this hook runs, so strip them here.
    let body: String = body
        .lines()
        .filter(|l| !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let staged = capture(
        Command::new("git")
            .args(["diff", "--cached", "--name-only"])
            .current_dir(repo_root()),
    )?;
    const CODE: &[&str] = &[
        "crates/codec/",
        "crates/format/",
        "crates/filter/",
        "crates/signal/",
    ];
    let touches_code = staged
        .lines()
        .any(|f| CODE.iter().any(|p| f.starts_with(p)));
    provenance::check_message(&repo_root(), &body, touches_code)
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

/// Every path `git ls-files` names, repo-relative, resolved to absolute
/// paths once and cached for the life of this process.
///
/// Every working-tree scan in this binary (`rust_files`'s callers,
/// `dup_check`, `comment_check`, `dead_code`'s own file walk) needs this:
/// a plain filesystem walk sees any agent's untracked, in-flight scratch
/// crate in this shared tree exactly like a real one, and reds a shared
/// gate (`provenance-check`, first found this way) over work that has no
/// commit to point at and that its own author has not finished. `git
/// ls-files` lists tracked files only — no untracked, no ignored — which
/// is exactly "real content in a real path," including a tracked file
/// that has since been *modified*: a scan must still see those, since
/// that is exactly the content the gate exists to check. This is **not**
/// a substitute for the `commit-msg`/`reference-transaction` provenance
/// check on what actually lands in a commit -- an untracked file about to
/// be committed still needs its own provenance entry before that commit
/// succeeds; this cache only keeps a *pre-commit scan* from failing on
/// content nobody has committed yet.
fn tracked_files() -> &'static Option<Set<PathBuf>> {
    static CACHE: std::sync::OnceLock<Option<Set<PathBuf>>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        let root = repo_root();
        let out = Command::new("git")
            .current_dir(&root)
            .args(["ls-files", "-z"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(
            String::from_utf8_lossy(&out.stdout)
                .split('\0')
                .filter(|s| !s.is_empty())
                .map(|rel| root.join(rel))
                .collect(),
        )
    })
}

/// All `.rs` files under a directory that `git` tracks. An untracked file
/// under `dir` — an agent's brand-new, in-progress crate, most often, per
/// [`tracked_files`]'s own doc — is invisible to this on purpose; a
/// tracked file that has since been modified is not filtered at all. Falls
/// back to every `.rs` file on disk when `git ls-files` could not run (no
/// git, or not a repo) rather than filtering against an empty set, which
/// would make every scan using this report nothing instead of everything.
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    let known = tracked_files();
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs")
                && known.as_ref().is_none_or(|k| k.contains(&p))
            {
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
