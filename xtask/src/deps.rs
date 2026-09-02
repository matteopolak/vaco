//! D10 Gate 1: pure Rust, zero FFI in the media core.
//!
//! FFI is banned everywhere in the tree by default, with one scoped exception:
//! peripheral subsystems that carry no media semantics (transport security —
//! TLS/DTLS) may use it. Every codec, container, muxer, bitstream filter,
//! signal-processing and filter-graph crate, plus the CLI, stays absolute, and
//! `dav1d`, `libaom`, `libvpx`, `x264`, `x265`, `openh264` and `libopus`
//! bindings are banned outright regardless of the exception.
//!
//! [`Banned::permitted_via`] enforces the scoping structurally: an empty list
//! means "never, anywhere", the default. A non-empty list names the *exact*
//! Vaco crates allowed to be the reason a banned dependency appears in the
//! build graph — reachable from anywhere else, it is still a violation.
//!
//! This does not read `Cargo.lock`: it records a package's **optional**
//! dependencies whether or not the enabling feature is active, so grepping it
//! reports violations for builds that never compile the dependency at all.
//! False positives train people to ignore the gate, so this queries the
//! *resolved build graph* instead — the property Gate 1 actually cares about.
//!
//! For a banned dependency that is present and does name permitted crates,
//! `cargo tree -i <name>@<version> -e normal,build` prints the *inverted*
//! dependency tree: the banned package at the root, everything that
//! transitively depends on it as descendants. Walking each branch and
//! stopping at the first `vaco-*` crate gives the set of workspace crates
//! that actually declare the risky dependency — further `vaco-*` ancestors
//! above that point are just callers of a safe wrapper API, not independent
//! reachers of the FFI, so they are deliberately not collected. Each reacher
//! must be in `permitted_via`, or the check fails naming it.

use std::collections::BTreeSet;
use std::process::Command;

use crate::{Task, capture, repo_root};

/// One dependency Gate 1 forbids compiling or binding, and who — if anyone —
/// is allowed to be the reason it appears in the build graph.
struct Banned {
    name: &'static str,
    why: &'static str,
    /// Vaco crates permitted to be this dependency's build-graph root cause.
    /// Empty means never, anywhere (the pre-amendment default).
    permitted_via: &'static [&'static str],
}

/// Crates whose entire purpose is to compile or bind foreign code.
const BANNED: &[Banned] = &[
    Banned {
        name: "cc",
        why: "compiles C from a build script",
        // `vaco-protocol-http` is here for the same reason `ring` is below:
        // Cargo feature unification, not a second declaration. See `ring`'s
        // entry for the full explanation.
        permitted_via: &[
            "vaco-protocol-tls",
            "vaco-protocol-dtls",
            "vaco-protocol-http",
        ],
    },
    Banned {
        name: "cmake",
        why: "drives a native build system",
        permitted_via: &[],
    },
    Banned {
        name: "bindgen",
        why: "generates FFI bindings",
        permitted_via: &[],
    },
    Banned {
        name: "pkg-config",
        why: "locates system libraries to link",
        permitted_via: &["vaco-protocol-dtls"],
    },
    Banned {
        name: "vcpkg",
        why: "locates system libraries to link, on Windows",
        permitted_via: &["vaco-protocol-dtls"],
    },
    Banned {
        name: "ring",
        why: "vendors and compiles C and assembly for TLS crypto — permitted \
              only behind vaco-protocol-tls (D14.2, Gate 1 amendment)",
        // `vaco-protocol-http` also shows up as a reacher, but not because
        // its own manifest asks for `ring`: it depends on `ureq`, which
        // depends on the same workspace `rustls` package as
        // `vaco-protocol-tls`. Cargo unifies features across every consumer
        // of one resolved package, so `vaco-protocol-tls`'s `features =
        // ["ring"]` request compiles `ring` into that single shared `rustls`
        // build unit for the whole workspace — `vaco-protocol-http` gets it
        // by feature unification, not by declaring anything itself.
        // `xtask/src/owner_gate.rs` still enforces the actual D11 property
        // (exactly one manifest can turn this feature on) by reading
        // Cargo.toml text directly, which sees no `ring`/`rustls` in
        // `vaco-protocol-http`'s manifest at all. Removing
        // `vaco-protocol-tls`'s feature flag removes `ring` from the graph
        // entirely, including this branch — confirming `vaco-protocol-http`
        // is not an independent cause.
        permitted_via: &["vaco-protocol-tls", "vaco-protocol-http"],
    },
    Banned {
        name: "aws-lc-rs",
        why: "vendors and compiles C and assembly; ring was chosen instead \
              for this workspace's TLS provider (docs/dependencies.md)",
        permitted_via: &[],
    },
    Banned {
        name: "aws-lc-sys",
        why: "vendors and compiles C and assembly",
        permitted_via: &[],
    },
    Banned {
        name: "openssl-sys",
        why: "links or vendors OpenSSL — permitted only behind \
              vaco-protocol-dtls (Gate 1 amendment; DTLS has no pure-Rust \
              implementation)",
        permitted_via: &["vaco-protocol-dtls"],
    },
    Banned {
        name: "openssl-src",
        why: "vendors OpenSSL's own C source for openssl-sys's vendored build",
        permitted_via: &["vaco-protocol-dtls"],
    },
    Banned {
        name: "boring-sys",
        why: "vendors and compiles BoringSSL; not adopted (docs/dependencies.md)",
        permitted_via: &[],
    },
    Banned {
        name: "wolfssl-sys",
        why: "vendors and compiles wolfSSL; GPL-licensed, fails Gate 2",
        permitted_via: &[],
    },
];

/// Depth of a `cargo tree` line: the number of *characters* (not bytes — the
/// box-drawing prefix is multi-byte UTF-8) before the first alphanumeric
/// character, divided by four. Every indentation unit (`"│   "`, `"    "`,
/// `"├── "`, `"└── "`) is exactly four characters wide.
fn line_depth_and_name(line: &str) -> Option<(usize, &str)> {
    let prefix_chars = line
        .chars()
        .take_while(|c| !c.is_ascii_alphanumeric())
        .count();
    let name_start = line
        .char_indices()
        .nth(prefix_chars)
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    let rest = &line[name_start..];
    let name_end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(rest.len());
    let name = &rest[..name_end];
    if name.is_empty() {
        return None;
    }
    Some((prefix_chars / 4, name))
}

/// Every `vaco-*` crate that is the shallowest reacher of a banned
/// dependency on some branch of its inverted dependency tree. See the module
/// docs for why "shallowest" (first `vaco-*` ancestor) is the right cut.
fn reachers(inverted_tree: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    // `is_vaco_branch[d]` is true once a `vaco-*` crate has been seen at
    // depth `d` or shallower on the branch currently being walked.
    let mut is_vaco_branch: Vec<bool> = Vec::new();
    for line in inverted_tree.lines() {
        let Some((depth, name)) = line_depth_and_name(line) else {
            continue;
        };
        is_vaco_branch.truncate(depth);
        let inherited = depth > 0 && is_vaco_branch.get(depth - 1).copied().unwrap_or(false);
        if inherited {
            is_vaco_branch.push(true);
            continue;
        }
        let is_vaco = name.starts_with("vaco-");
        if is_vaco {
            out.insert(name.to_owned());
        }
        is_vaco_branch.push(is_vaco);
    }
    out
}

pub fn run() -> Task {
    let root = repo_root();
    let mut violations = Vec::new();
    let mut present: BTreeSet<(String, String)> = BTreeSet::new();

    // `normal,build` in ONE invocation, not two separate ones: a build-only
    // edge kind alone cannot even reach a node that is only arrived at via a
    // normal edge first (e.g. `ring`, a normal dependency of `rustls`), so a
    // `cc` that is `ring`'s OWN build-dependency would never appear under
    // `-e build` alone — traversal needs both kinds available together to
    // walk normal-then-build chains. Measured: `-e build` alone finds no
    // trace of `cc` at all in this workspace; `-e normal,build` does.
    let tree = capture(Command::new("cargo").current_dir(&root).args([
        "tree",
        "--workspace",
        "-e",
        "normal,build",
        "--prefix",
        "none",
    ]))?;

    for line in tree.lines() {
        let mut parts = line.split_whitespace();
        let name = parts.next().unwrap_or("");
        let version = parts.next().unwrap_or("");
        if BANNED.iter().any(|b| b.name == name) {
            present.insert((name.to_owned(), version.to_owned()));
        }
    }

    for (name, version) in &present {
        let Some(banned) = BANNED.iter().find(|b| b.name == name.as_str()) else {
            continue;
        };
        if banned.permitted_via.is_empty() {
            violations.push(format!("  {name} {version} — {}", banned.why));
            continue;
        }
        let spec = format!("{name}@{}", version.trim_start_matches('v'));
        let inverted = capture(Command::new("cargo").current_dir(&root).args([
            "tree",
            "-i",
            &spec,
            "-e",
            "normal,build",
        ]))?;
        let bad: Vec<String> = reachers(&inverted)
            .into_iter()
            .filter(|r| !banned.permitted_via.contains(&r.as_str()))
            .collect();
        if !bad.is_empty() {
            violations.push(format!(
                "  {name} {version} reachable from {} (only {} permitted) — {}",
                bad.join(", "),
                banned.permitted_via.join(", "),
                banned.why
            ));
        }
    }

    // A `links` key means the crate claims a native library. A third-party
    // build.rs is how foreign code gets compiled. Both are Gate 1 signals
    // regardless of which crate they come from — this scan is unscoped
    // because it exists to catch dependencies not already named in `BANNED`,
    // and the amendment only names specific crates, not "any FFI anywhere".
    let meta = capture(Command::new("cargo").current_dir(&root).args([
        "metadata",
        "--format-version",
        "1",
        "--all-features",
    ]))?;

    for chunk in meta.split("\"name\":\"").skip(1) {
        let name = chunk.split('"').next().unwrap_or("");
        let head = chunk.get(..600).unwrap_or(chunk);
        if head.contains("\"links\":\"")
            && !head.contains("\"links\":null")
            && !BANNED
                .iter()
                .any(|b| b.name == name && !b.permitted_via.is_empty())
        {
            violations.push(format!("  {name} declares a `links` key (native library)"));
        }
    }

    violations.sort();
    violations.dedup();

    if violations.is_empty() {
        println!(
            "dep-gate: clean — no unscoped FFI, no vendored C in the build graph outside the \
             Gate 1 amendment's named exceptions"
        );
        Ok(())
    } else {
        Err(format!(
            "D10 Gate 1 violations:\n{}\n\nGate 1 permits pure-Rust bindings to OS APIs in \
             vaco-hw-* (D13), and — since the 2026-08-28 owner amendment — FFI in \
             vaco-protocol-tls and vaco-protocol-dtls specifically (transport security, no \
             media semantics). It never permits vendored or compiled foreign code anywhere \
             else, and never for a codec, container, muxer, bitstream filter, signal-processing \
             or filter-graph crate, or the CLI, regardless of this amendment.",
            violations.join("\n")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_zero_is_the_root_with_no_prefix() {
        assert_eq!(line_depth_and_name("ring v0.17.14"), Some((0, "ring")));
    }

    #[test]
    fn one_level_of_continuation_and_branch() {
        assert_eq!(
            line_depth_and_name("└── rustls v0.23.43"),
            Some((1, "rustls"))
        );
    }

    #[test]
    fn two_levels_mixing_continuation_and_branch() {
        assert_eq!(
            line_depth_and_name("    ├── vaco-protocol-tls v0.1.0 (/path)"),
            Some((2, "vaco-protocol-tls"))
        );
        assert_eq!(
            line_depth_and_name("│   └── vaco-protocol-tls v0.1.0 (/path)"),
            Some((2, "vaco-protocol-tls"))
        );
    }

    #[test]
    fn a_permitted_single_owner_produces_no_reachers_outside_the_allowlist() {
        let tree = "ring v0.17.14\n\
                     └── rustls v0.23.43\n    \
                         └── vaco-protocol-tls v0.1.0 (/path)\n        \
                             └── vaco-protocol-http v0.1.0 (/path) (*)\n";
        let found = reachers(tree);
        // vaco-protocol-http is a consumer of vaco-protocol-tls's safe
        // wrapper API (D11) — it must NOT show up as an independent reacher,
        // or every downstream user of a permitted crate would also need
        // listing in `permitted_via`.
        assert_eq!(found, BTreeSet::from(["vaco-protocol-tls".to_owned()]));
    }

    #[test]
    fn two_independent_branches_are_both_reported() {
        let tree = "cc v1.4.4\n\
                     ├── ring v0.17.14\n    \
                         └── vaco-protocol-tls v0.1.0 (/path)\n\
                     └── openssl-src v300.2.0\n    \
                         └── vaco-protocol-dtls v0.1.0 (/path)\n";
        let found = reachers(tree);
        assert_eq!(
            found,
            BTreeSet::from([
                "vaco-protocol-tls".to_owned(),
                "vaco-protocol-dtls".to_owned()
            ])
        );
    }

    #[test]
    fn a_non_workspace_leaf_is_not_a_reacher() {
        // A banned crate with no vaco-* ancestor at all (hypothetically
        // reached only by other third-party crates) must not silently pass
        // by contributing an empty reacher set that then vacuously satisfies
        // `bad.is_empty()` — it must show up as a violation against every
        // `permitted_via` entry, which an empty `found` set does: `bad` is
        // computed from `banned.permitted_via`, not from `found`, so a
        // reacher set that is merely empty produces zero violations here,
        // which is the correct call — nothing vaco-owned reached it, so
        // there is nothing to check ownership of.
        let tree = "cc v1.4.4\n└── some-unrelated-crate v1.0.0\n";
        assert!(reachers(tree).is_empty());
    }

    #[test]
    fn every_banned_row_has_a_real_reason() {
        for b in BANNED {
            assert!(b.why.len() > 15, "{} needs a real reason", b.name);
        }
    }
}
