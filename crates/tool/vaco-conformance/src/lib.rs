#![forbid(unsafe_code)]
//! The differential conformance harness (plan 13 §1, issues QA-02 / QA-03).
//!
//! # What it is
//!
//! Vaco claims its output is identical to the reference's for the same input
//! and the same flags. This crate is the machinery that either proves that or
//! reports precisely where it fails. It is the primary acceptance criterion for
//! the whole project: nothing is correct until this says so.
//!
//! A case is a triple — `(media, argument vector, comparison mode)` — run
//! against both binaries under a fixed environment, with the results compared
//! and any difference either explained by a governed allowlist entry or
//! reported as a failure.
//!
//! # THE BRIGHT-LINE RULE
//!
//! > **You may run the reference binary as often as you like. You may not read
//! > its source. When our output differs and you cannot explain why, you
//! > escalate — you do not go looking in the source for the answer.**
//!
//! That is plan 13 §1.7.2 verbatim and it is the reason this design is
//! clean-room compatible. The reference is an **oracle we query, never a source
//! we read**. Two consequences shape everything here:
//!
//! - **There are no golden files.** Every expected value is computed by running
//!   the reference at test time and discarded afterwards. Nothing
//!   `FFmpeg`-derived ever enters the repository, which defeats both the *access*
//!   and the *substantial similarity* elements rather than arguing about one of
//!   them (§1.7.1). There is deliberately no `--update-expected` mode, because
//!   there is nothing to update.
//! - **Opening a reference source file to explain a divergence crosses the
//!   line.** Doing so makes you a dirty-team member for that module and you may
//!   no longer author implementation code in it. Use the triage ladder in
//!   `docs/tool/vaco-conformance.md` instead.
//!
//! # Layout
//!
//! | Module | Role |
//! |---|---|
//! | [`toml`] | the small TOML reader the manifests and the register are written in |
//! | [`refbin`] | reference pinning and discovery (QA-03) |
//! | [`run`] | hermetic process execution |
//! | [`case`] | the case model and the ten comparison modes |
//! | [`manifest`] | declarative suites and matrix expansion |
//! | [`normalise`] | the named, per-case normalisation chain |
//! | [`divergence`] | the governed allowlist and its anti-rot machinery |
//! | [`compare`] | the comparators |
//! | [`extract`] | differential checks on our static tables |
//! | [`runner`] | executing cases |
//! | [`report`] | turning verdicts into something a human acts on |
//!
//! # Where to start
//!
//! `vaco-conformance tables` is the useful entry point today: it holds
//! `vaco-pixfmt`'s format table and `vaco-core`'s colour, frame-size and
//! frame-rate tables to the reference, which is the first external validation
//! anything in the project has had.

pub mod case;
pub mod compare;
pub mod divergence;
pub mod extract;
pub mod filterexec;
pub mod manifest;
pub mod normalise;
pub mod refbin;
pub mod refhelp;
pub mod report;
pub mod run;
pub mod runner;
pub mod toml;

/// Where suite manifests are looked for, in order.
///
/// `tests/conformance/` at the repository root is where plan 13 §1.5.1 puts
/// them, and it wins when it exists. The crate-local `suites/` directory is the
/// fallback so the harness ships with runnable examples and its own tests do
/// not depend on a directory another crate owns.
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
