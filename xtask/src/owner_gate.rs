//! Every third-party media crate is reachable from exactly one Vaco crate (D11).
//!
//! # What the rule is actually protecting
//!
//! Bit-identical output against the reference is a project requirement (D6), and
//! an external crate was written to satisfy its own correctness criteria rather
//! than ours. It may round differently, handle an edge case differently, or
//! simply produce different-but-valid output. So every external codec, container
//! or signal-processing crate is **provisional**: useful now, possibly wrong for
//! us later.
//!
//! D11's answer is that swapping a backend must mean rewriting one crate's
//! internals and nothing else. That only holds if exactly one crate can reach
//! the dependency — a second caller turns a contained replacement into a
//! migration.
//!
//! # Why there is a list rather than "every external dependency"
//!
//! Because the unfiltered rule is wrong, and measurably so. Five external
//! crates in this workspace have more than one owner today:
//!
//! ```text
//! bitflags     10 crates
//! smallvec      6
//! thiserror     6
//! tracing       2
//! parking_lot   2
//! ```
//!
//! None of them is what D11 guards. A `bitflags` in ten crates is a language
//! convenience with no bearing on output fidelity; there is no "swap the
//! bitflags backend" migration to contain. Failing on those would make the gate
//! fire ten times on its first run for reasons nobody can act on, and a gate
//! that cries wolf is one people learn to suppress — the same reasoning that
//! keeps `encumbered` separate from `default = false` in [`crate::patent_gate`].
//!
//! So [`MEDIA`] names the crates whose *output* we could disagree with. Adding a
//! row is part of adopting one, and the D10 review that admits a media crate is
//! the moment to add it.
//!
//! # What this cannot see
//!
//! It reads `[dependencies]` tables, so it catches a second *declared* owner. It
//! cannot see a crate re-exporting an external type through its own public API,
//! which is D11's other half ("no external type appears in its public API — not
//! in a signature, not in an error variant, not in a re-export"). That still
//! needs a person, and the wrapping crate's own review is where it happens.

use std::collections::BTreeMap;

use crate::{Task, crates};

/// Third-party crates that implement media functionality, and therefore whose
/// output we could disagree with.
///
/// Deliberately **not** every external dependency — see the module note. The
/// test is "could swapping this change a byte of our output?", not "is this
/// third-party?".
///
/// Empty of true codecs today: nothing in the tree wraps an external decoder or
/// demuxer. The three entries are the closest thing to it — a decompressor a
/// container calls, a TLS stack, and the hash functions `-show_data_hash`
/// prints. Each would change observable output if replaced.
const MEDIA: &[(&str, &str)] = &[
    (
        "miniz_oxide",
        "zlib inflate for Matroska ContentCompression. A different inflate that \
         disagreed on a malformed stream would change what we demux.",
    ),
    (
        "rustls",
        "TLS for https. A transport swap changes what bytes arrive.",
    ),
    // The crypto provider (`rustls-rustcrypto` until 2026-08-28, now `ring`)
    // is NOT its own row: it arrives via `rustls`'s own `ring` Cargo feature
    // rather than as a directly-declared dependency (see
    // `vaco-protocol-tls/src/crypto.rs`), so it never appears in a manifest's
    // `[dependencies]` table for this scan to find. `cargo xtask dep-gate`
    // (D10 Gate 1) tracks it instead, by resolved build graph rather than by
    // manifest text, which can see a feature-activated dependency this scan
    // cannot.
    (
        "ureq",
        "the HTTP client. Range handling and redirect policy are observable.",
    ),
    (
        "crc",
        "CRC-32 for -show_data_hash. The checksum IS the printed output, so a \
         disagreeing implementation is a byte diff by definition.",
    ),
    (
        "md-5",
        "MD5 for -show_data_hash and the framemd5 comparison mode, which is one \
         of the differential harness's own oracles (D6).",
    ),
    (
        "sha1",
        "SHA-1 for -show_data_hash. Printed verbatim, same as crc.",
    ),
    (
        "sha2",
        "SHA-256/512 for -show_data_hash. Printed verbatim, same as crc.",
    ),
    (
        "aes",
        "AES-128 block cipher behind the crypto: protocol. A disagreeing \
         implementation decrypts to different bytes, full stop.",
    ),
    (
        "cbc",
        "CBC chaining behind the crypto: protocol. Added alongside aes: \
         measured against ffmpeg 8.1 to be CBC, not the CTR the work package \
         naming it assumed — see docs/io/vaco-protocol-crypto.md.",
    ),
];

/// Read the `[dependencies]` table of one manifest.
///
/// Deliberately not `[dev-dependencies]` or `[build-dependencies]`: neither
/// ships, so neither can change a byte of output, and counting them would make
/// a test helper look like a second owner.
fn declared_deps(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_deps = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_deps = t == "[dependencies]";
            continue;
        }
        if !in_deps || t.is_empty() || t.starts_with('#') {
            continue;
        }
        let name: String = t
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if !name.is_empty() && !name.starts_with("vaco") {
            out.push(name);
        }
    }
    out
}

pub fn run(_check: bool) -> Task {
    let mut owners: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (_layer, name, path) in crates() {
        let Ok(text) = std::fs::read_to_string(path.join("Cargo.toml")) else {
            continue;
        };
        for dep in declared_deps(&text) {
            owners.entry(dep).or_default().push(name.clone());
        }
    }

    let mut violations = Vec::new();
    for (dep, why) in MEDIA {
        let Some(cs) = owners.get(*dep) else {
            continue;
        };
        if cs.len() > 1 {
            violations.push(format!("  {dep}: {} — {why}", cs.join(", ")));
        }
    }

    if !violations.is_empty() {
        violations.sort();
        return Err(format!(
            "{} third-party media crate(s) are reachable from more than one \
             Vaco crate (D11):\n{}\n\nD11 exists so that swapping a backend \
             means rewriting one crate's internals and nothing else. A second \
             caller turns a contained replacement into a migration. Put the \
             dependency behind the crate that owns it and expose only our own \
             types.",
            violations.len(),
            violations.join("\n")
        ));
    }

    let tracked = MEDIA
        .iter()
        .filter(|(d, _)| owners.contains_key(*d))
        .count();
    println!(
        "owner-gate: {tracked} media crate(s) tracked, each with exactly one \
         owner ({} external deps in total; the rest are infrastructure and are \
         not D11's concern)",
        owners.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_dependencies_table_counts() {
        // A dev-dependency is not a second owner: it does not ship, so it
        // cannot change a byte of output. Counting it would make a test helper
        // look like a D11 violation.
        let manifest = "[package]\nname = \"x\"\n\n[dependencies]\nureq = \"3\"\n\
                        \n[dev-dependencies]\ntempfile = \"3\"\nureq = \"3\"\n";
        assert_eq!(declared_deps(manifest), vec!["ureq".to_owned()]);
    }

    #[test]
    fn vaco_crates_are_not_third_party() {
        let manifest = "[dependencies]\nvaco-core = { path = \"..\" }\nureq = \"3\"\n";
        assert_eq!(declared_deps(manifest), vec!["ureq".to_owned()]);
    }

    #[test]
    fn every_media_row_has_a_reason() {
        // The reason is the point: it is what stops the list becoming a place
        // to park a name without deciding anything. Same rule as `dup-check`'s
        // DISTINCT and `wasm-check`'s NATIVE_ONLY.
        for (name, why) in MEDIA {
            assert!(why.len() > 20, "{name} needs a real reason, got {why:?}");
        }
    }
}
