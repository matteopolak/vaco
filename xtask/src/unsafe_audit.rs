//! `unsafe` may appear only where D2 and D13 permit.
//!
//! # How this checks, and why not by grepping
//!
//! A textual scan for `unsafe` cannot tell code from a string literal — an
//! earlier version of this file flagged *itself*, because it contains the word
//! in its own matcher. Worse, a scan is a weaker guarantee than the one already
//! available: a crate inheriting `unsafe_code = "forbid"` cannot compile with
//! `unsafe` in it at all, and the compiler is a better oracle than a regex.
//!
//! So the audit inverts. For crates that must be safe it verifies the *lint
//! inheritance* — which is the thing a one-line diff can silently remove and the
//! compiler cannot flag. For the hardware crates that are permitted `unsafe`, it
//! reports how much there is, so the audited surface stays visible and small.

use crate::{Task, crates, repo_root, rust_files};

/// The only crates permitted `unsafe`, per D13: hardware video decode and encode
/// reach fixed-function silicon through OS APIs, and no safe portable
/// alternative exists (`wgpu` does not expose video decode).
const ALLOWED_PREFIX: &str = "vaco-hw-";

/// Named exceptions, each for a reason that is not D13's.
///
/// `GlobalAlloc` is an `unsafe trait`: a counting allocator cannot be written
/// safely in any amount of effort, because the language does not offer a safe
/// spelling of it. This is narrower than D13's prefix rule deliberately — it
/// admits one crate by name, and that crate is a dependency of nothing, so no
/// shipped binary links it. It exists to measure allocation while fuzzing.
///
/// Add to this list only when `unsafe` is unavoidable *by language design* and
/// the crate stays out of the shipping build. "It was easier" is not a reason.
const ALLOWED_NAMES: &[&str] = &["vaco-fuzz-alloc"];

pub fn run() -> Task {
    // xtask is audited too. It enforces these policies, so it does not get to be
    // exempt from them, and sitting outside `crates/` it would otherwise be the
    // one place in the repository this cannot see.
    let mut targets = crates();
    targets.push(("xtask".into(), "xtask".into(), repo_root().join("xtask")));

    let mut violations = Vec::new();
    let mut exempt = Vec::new();

    for (_, name, path) in targets {
        let manifest = std::fs::read_to_string(path.join("Cargo.toml")).unwrap_or_default();

        if name.starts_with(ALLOWED_PREFIX) || ALLOWED_NAMES.contains(&name.as_str()) {
            // Permitted. Report the volume so the audited surface stays visible.
            let mut blocks = 0usize;
            for f in rust_files(&path.join("src")) {
                let src = std::fs::read_to_string(&f).unwrap_or_default();
                blocks += src.matches("unsafe ").count();
            }
            exempt.push(format!("{name} ({blocks} unsafe sites)"));
            continue;
        }

        // The guarantee we need is "unsafe is forbidden", which a crate can state
        // two ways: by inheriting the workspace lints, or by declaring it itself.
        // Checking only for inheritance would reject the second spelling, and a
        // gate that rejects a correct configuration teaches people to work around
        // it rather than to satisfy it.
        let inherits = manifest.contains("[lints]") && manifest.contains("workspace = true");
        let declares = manifest.contains(r#"unsafe_code = "forbid""#);
        if !inherits && !declares {
            violations.push(format!(
                "  {name} neither inherits `[lints] workspace = true` nor declares \
                 `unsafe_code = \"forbid\"`, so unsafe is not forbidden in it"
            ));
        }
    }

    if violations.is_empty() {
        let exempt_desc = if exempt.is_empty() {
            "none yet".to_owned()
        } else {
            exempt.join(", ")
        };
        println!("unsafe-audit: clean — all crates forbid unsafe; exempt: {exempt_desc}");
        Ok(())
    } else {
        Err(format!(
            "unsafe outside the D13 allowlist:\n{}",
            violations.join("\n")
        ))
    }
}
