//! No patent-encumbered component is in the published build (D4).
//!
//! # Why this asserts on the compiled list rather than on the manifests
//!
//! D4's own wording is "assert on the compiled feature list, **not on intent**",
//! and the distinction is the whole design. A manifest reader answers "is this
//! component marked `default = false`?", which is a claim about what somebody
//! wrote down. The question that matters is "is this component in the binary?",
//! and only the compiler can answer it.
//!
//! So `gen-registry` emits [`ENCUMBERED_ENABLED`], where every row is
//! `#[cfg]`-gated on its own feature. The slice a build produces *is* the
//! compiler's answer. This gate builds the default configuration and asserts the
//! slice is empty.
//!
//! [`ENCUMBERED_ENABLED`]: https://docs.rs/vaco-registry
//!
//! # Non-empty is not automatically wrong
//!
//! D4 explicitly supports building encumbered codecs yourself — that is the
//! point of putting them behind non-default features rather than leaving them
//! out. A developer's build with `--features patent-encumbered-hevc-encode` is
//! working as designed. What must never happen is a *published* binary
//! containing one, so this gate checks the default feature set specifically.
//!
//! # The denominator matters
//!
//! A gate that only read the enabled list could not distinguish "nothing is
//! encumbered" from "the table generator is broken and reports nothing". So
//! `ENCUMBERED_ALL` is emitted ungated, and this reports both numbers. Today
//! they are 0 and 0, which is the honest state of a tree with no encoders in it
//! — and the test below plants a row to prove the mechanism fires when there is
//! something to fire on.

use std::process::Command;

use crate::{Task, repo_root};

/// A tiny program that prints the two slices. Compiled against the default
/// feature set, so its output is the compiler's answer rather than ours.
const PROBE: &str = r#"fn main() {
    println!("enabled={}", vaco_registry::ENCUMBERED_ENABLED.len());
    println!("all={}", vaco_registry::ENCUMBERED_ALL.len());
    for name in vaco_registry::ENCUMBERED_ENABLED {
        println!("row={name}");
    }
}
"#;

pub fn run(_check: bool) -> Task {
    let root = repo_root();

    // The example lives in `vaco-registry` itself so it inherits exactly that
    // crate's resolved features — running it any other way would resolve a
    // different feature graph and answer a different question.
    let dir = root.join("crates/registry/vaco-registry/examples");
    std::fs::create_dir_all(&dir).map_err(|e| format!("examples dir: {e}"))?;
    let path = dir.join("patent_gate_probe.rs");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing != PROBE {
        std::fs::write(&path, PROBE).map_err(|e| format!("probe: {e}"))?;
    }

    let out = Command::new("cargo")
        .current_dir(&root)
        .args([
            "run",
            "-q",
            "-p",
            "vaco-registry",
            "--example",
            "patent_gate_probe",
        ])
        .output()
        .map_err(|e| format!("cargo: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "the patent probe did not build:\n{}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let field = |k: &str| -> usize {
        text.lines()
            .find_map(|l| l.strip_prefix(k))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(usize::MAX)
    };
    let (enabled, all) = (field("enabled="), field("all="));
    if enabled == usize::MAX || all == usize::MAX {
        return Err(format!("could not read the probe's output:\n{text}"));
    }

    if enabled > 0 {
        let rows: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("row="))
            .collect();
        return Err(format!(
            "{enabled} patent-encumbered component(s) are compiled into the \
             DEFAULT build (D4):\n  {}\n\nD4 puts these behind non-default \
             features and keeps them out of every binary we publish. Building \
             one yourself is supported and expected; shipping one is not. \
             Either the component's fragment is missing `default = false`, or a \
             feature it names has been added to the workspace `default` set.",
            rows.join("\n  ")
        ));
    }

    println!(
        "patent-gate: 0 of {all} known encumbered component(s) compiled into the \
         default build"
    );
    if all == 0 {
        println!(
            "  (no component in the tree is marked `encumbered = true` yet — \
             unencumbered encoders exist since C-13, but none patent-encumbered. \
             The gate is in place for when one lands; `encumbered_rows_are_gated` \
             plants one and proves the mechanism fires.)"
        );
    }
    Ok(())
}
