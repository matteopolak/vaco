//! `unsafe` may appear only where D2 and D13 permit.
//!
//! `#![forbid(unsafe_code)]` is inherited from `[workspace.lints]`, so the
//! compiler already enforces this for crates that inherit it. What the compiler
//! cannot catch is a crate quietly *stopping* inheriting — dropping `[lints]
//! workspace = true` is a one-line change that silently removes the guarantee.
//! This audit makes the exception set explicit.

use crate::{crates, rust_files, Task};

/// The only crates permitted `unsafe`, per D13. Hardware video decode and encode
/// reach fixed-function silicon through OS APIs; there is no safe portable
/// alternative today (wgpu does not expose video decode).
const ALLOWED_PREFIX: &str = "vaco-hw-";

pub fn run() -> Task {
    let mut violations = Vec::new();
    let mut exempt = Vec::new();

    for (_, name, path) in crates() {
        let manifest = std::fs::read_to_string(path.join("Cargo.toml")).unwrap_or_default();
        let inherits = manifest.contains("workspace = true")
            && manifest.contains("[lints]");
        let allowed = name.starts_with(ALLOWED_PREFIX);

        if allowed {
            exempt.push(name.clone());
            continue;
        }

        if !inherits {
            violations.push(format!(
                "  {name} does not inherit [lints] workspace = true, so forbid(unsafe_code) \
                 is not applied to it"
            ));
        }

        for f in rust_files(&path.join("src")) {
            let src = std::fs::read_to_string(&f).unwrap_or_default();
            for (n, line) in src.lines().enumerate() {
                let t = line.trim_start();
                if (t.starts_with("unsafe ") || t.contains(" unsafe {"))
                    && !t.starts_with("//")
                    && !t.starts_with("///")
                {
                    violations.push(format!("  {}:{} contains `unsafe`", f.display(), n + 1));
                }
            }
        }
    }

    if violations.is_empty() {
        println!(
            "unsafe-audit: clean — {} crates forbid unsafe, {} hardware crates exempt ({})",
            crates().len() - exempt.len(),
            exempt.len(),
            if exempt.is_empty() { "none yet".into() } else { exempt.join(", ") }
        );
        Ok(())
    } else {
        Err(format!(
            "unsafe outside the D13 allowlist:\n{}",
            violations.join("\n")
        ))
    }
}
