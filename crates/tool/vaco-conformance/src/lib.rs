#![forbid(unsafe_code)]
//! Differential conformance harness.
//!
//! Vaco claims its output is identical to the reference's for the same input
//! and the same flags. This crate is the machinery that either proves that or
//! reports precisely where it fails.
//!
//! A case is a triple — `(media, argument vector, comparison mode)` — run
//! against both binaries under a fixed environment, with the results compared
//! and any difference either explained by a governed allowlist entry or
//! reported as a failure.
//!
//! # Clean-room boundary
//!
//! > **You may run the reference binary as often as you like. You may not read
//! > its source. When our output differs and you cannot explain why, you
//! > escalate — you do not go looking in the source for the answer.**
//!
//! The reference is an oracle, never a source. Expected values are computed at
//! run time and discarded; there are no golden files or update-golden mode.
//! Reading reference source to explain a divergence crosses the boundary; use
//! the triage ladder in `docs/tool/vaco-conformance.md`.
//!
//! [`manifest`] expands cases, [`run`] executes them hermetically,
//! [`normalise`] applies declared transformations, [`compare`] decides the
//! result, and [`divergence`] governs accepted differences. [`extract`] checks
//! static tables directly. `vaco-conformance tables` is the entry point for
//! pixel-format, colour, frame-size, and frame-rate table comparisons.

pub mod case;
pub mod compare;
pub mod deviation;
pub mod divergence;
pub mod extract;
pub mod filterexec;
pub mod manifest;
pub mod metrics;
pub mod normalise;
pub mod refbin;
pub mod refhelp;
pub mod registries;
pub mod report;
pub mod run;
pub mod runner;
pub mod suites;
pub mod toml;

/// Where suite manifests are looked for, in order.
///
/// Repository-level `tests/conformance/` wins when present. Crate-local
/// `suites/` is the fallback so the harness ships with runnable examples.
#[must_use]
pub fn suite_roots() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = std::env::var_os("VACO_CONFORMANCE_SUITES") {
        out.push(std::path::PathBuf::from(p));
        return out;
    }
    let crate_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = crate_dir.join("..").join("..").join("..");
    let plan_location = repo_root.join("tests").join("conformance");
    if plan_location.is_dir() {
        out.push(plan_location);
    }
    out.push(crate_dir.join("suites"));
    out
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "a failing expectation in a test is a failing test"
)]
mod tests {
    #[test]
    fn suite_roots_always_include_a_directory_that_exists() {
        let roots = super::suite_roots();
        assert!(!roots.is_empty());
        assert!(
            roots.iter().any(|r| r.is_dir()),
            "at least one suite root must exist: {roots:?}"
        );
    }

    #[test]
    fn the_shipped_register_and_pin_both_load() {
        // The two files the whole harness depends on. If either stops parsing,
        // everything downstream reports nonsense, so they are checked first.
        super::divergence::Allowlist::load().expect("divergences.toml loads");
        super::refbin::RefSpec::load().expect("refspec.toml loads");
    }
}
