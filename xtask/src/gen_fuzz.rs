//! Generate `fuzz/Cargo.toml` from front-matter in the fuzz target files.
//!
//! Hand-editing it meant touching three separate regions per target —
//! `[dependencies]`, `[features]` (twice), and a new `[[bin]]` block — with
//! every agent in the project editing it and none of them owning it. That
//! produced real damage in one wave: a substring replace on `core = []` that
//! also matched `codec-core`/`protocol-core`/`format-core`/`cli-core`; a
//! `default` line overwritten between one agent's read and its write;
//! `cli-core` and `conformance` silently dropped from `default`, leaving
//! three crates' fuzz targets unrunnable until someone noticed.
//!
//! Each target now declares its crate in its own module docs instead of a
//! separate fragment file:
//!
//! ```text
//! //! fuzz-crate: vaco-core
//! ```
//!
//! Everything else — the path dependency, the feature name, the `[[bin]]`
//! block, and `default` — is derived, so a target can never again be
//! silently unrunnable with a plain `cargo fuzz run <target>`.
//!
//! Every path dependency is `optional = true`, gated behind its own feature
//! (`dep:` syntax), because a single crate with a syntax error used to fail
//! every fuzz target in the tree — building any one target's default
//! feature set meant building all of them, and in a tree with several
//! agents writing crates at once a transiently-broken crate is the normal
//! state. `default` still lists every feature so the plain invocation keeps
//! working when the tree is healthy, but
//! `cargo fuzz run <target> --no-default-features --features <feature>`
//! builds only that target's own crate, isolating it from a sibling that
//! does not compile.

use crate::{Map, Set, Task, crates, repo_root};

/// The declaration line an agent writes in its own target file.
const KEY: &str = "fuzz-crate:";

pub fn run(check: bool) -> Task {
    let root = repo_root();
    let targets_dir = root.join("fuzz/fuzz_targets");

    // crate name -> layer directory, for the path dependencies.
    let layer_of: Map<String, String> = crates()
        .into_iter()
        .map(|(layer, name, _)| (name, layer))
        .collect();

    let mut entries = Vec::new();
    let mut missing = Vec::new();
    // Every crate a given target's body mentions, keyed by target. A superset
    // of what it declares: `frame_alloc` fuzzes `vaco-frame` but needs
    // `vaco-pixfmt` to build a frame at all. The front-matter says which crate
    // a target is a fuzz target FOR — it is not the dependency list.
    let mut referenced_by: Map<String, Set<String>> = Map::new();
    let read =
        std::fs::read_dir(&targets_dir).map_err(|e| format!("{}: {e}", targets_dir.display()))?;

    for f in read.flatten() {
        let path = f.path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let Some(target) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let body = std::fs::read_to_string(&path).unwrap_or_default();

        // Scan the source for `vaco_foo` paths. Deliberately crude: over-listing
        // a dependency costs a compile edge in a manifest that ships nowhere,
        // while under-listing one breaks the build — which is exactly what the
        // first version of this generator did by emitting only declared crates.
        let mut referenced = Set::new();
        for word in body.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
            if let Some(rest) = word.strip_prefix("vaco_") {
                let name = format!("vaco-{}", rest.replace('_', "-"));
                if layer_of.contains_key(&name) {
                    referenced.insert(name);
                }
            }
        }

        // Only the header comment counts for the DECLARATION, so a mention in
        // prose cannot claim a target fuzzes something it does not.
        let declared = body
            .lines()
            .take_while(|l| l.starts_with("//!") || l.trim().is_empty())
            .find_map(|l| l.split_once(KEY))
            .map(|(_, rest)| rest.trim().to_string());

        match declared {
            Some(c) if layer_of.contains_key(&c) => {
                // The declared crate is a dependency even if the header line
                // is the only place its name appears in the file.
                referenced.insert(c.clone());
                referenced_by.insert(target.to_string(), referenced);
                entries.push((target.to_string(), c));
            }
            Some(c) => {
                return Err(format!(
                    "fuzz/fuzz_targets/{target}.rs declares `{KEY} {c}`, \
                     which is not a crate under crates/*/"
                ));
            }
            None => missing.push(target.to_string()),
        }
    }

    if !missing.is_empty() {
        missing.sort();
        return Err(format!(
            "these fuzz targets have no `//! {KEY} <crate>` header, so nothing \
             can tell which crate they fuzz:\n  {}\n\
             Add the line to the target's module docs.",
            missing.join("\n  ")
        ));
    }
    entries.sort();

    let out = render(&entries, &referenced_by, &layer_of);

    let dest = root.join("fuzz/Cargo.toml");
    if check {
        let current = std::fs::read_to_string(&dest).unwrap_or_default();
        if current != out {
            return Err(format!(
                "{} is stale; run `cargo xtask gen-fuzz`",
                dest.display()
            ));
        }
        println!("gen-fuzz --check: up to date ({} targets)", entries.len());
        return Ok(());
    }
    std::fs::write(&dest, &out).map_err(|e| format!("{}: {e}", dest.display()))?;
    let used: Set<String> = entries.iter().map(|(_, c)| c.clone()).collect();
    println!(
        "gen-fuzz: {} targets across {} crates",
        entries.len(),
        used.len()
    );
    Ok(())
}

/// Turn the collected (target, declared-crate) pairs and each target's
/// referenced crates into the full manifest text.
///
/// Kept separate from the filesystem walk in [`run`] so the mapping from
/// front-matter to manifest text can be exercised directly, on synthetic
/// input, without touching `fuzz/fuzz_targets` or `crates/`.
fn render(
    entries: &[(String, String)],
    referenced_by: &Map<String, Set<String>>,
    layer_of: &Map<String, String>,
) -> String {
    let used: Set<String> = entries.iter().map(|(_, c)| c.clone()).collect();
    let feature_of = |c: &str| c.strip_prefix("vaco-").unwrap_or(c).to_string();

    // What a feature must switch on: the union, over every target that
    // declares this crate, of what that target's own body references. Two
    // targets can declare the same crate with different extra references —
    // the feature enables the union, which is the same over-list-rather-than-
    // under-list call the per-target scan already makes.
    let mut deps_of: Map<String, Set<String>> = Map::new();
    for (target, c) in entries {
        if let Some(refs) = referenced_by.get(target) {
            deps_of.entry(c.clone()).or_default().extend(refs.clone());
        }
    }

    // The [dependencies] table lists every crate any target references at
    // all, since a crate with no feature pointing at it is simply never built.
    let mut referenced: Set<String> = Set::new();
    for refs in referenced_by.values() {
        referenced.extend(refs.iter().cloned());
    }

    let mut out = String::from(
        "# Fuzz targets for the whole workspace.\n\
         #\n\
         # GENERATED by `cargo xtask gen-fuzz`. Do not edit.\n\
         #\n\
         # To add a target: create `fuzz/fuzz_targets/<name>.rs` and give it the\n\
         # header line `//! fuzz-crate: <your-crate>`. Then run\n\
         # `cargo xtask gen-fuzz`. The dependency, the feature, the [[bin]] block\n\
         # and the `default` entry are all derived from that one line, so no two\n\
         # agents ever edit the same region of this file.\n\
         #\n\
         # EXCLUDED from the root workspace (see the `exclude` key in /Cargo.toml),\n\
         # so this manifest has its own lockfile and its own dependency set.\n\
         # `libfuzzer-sys` and `arbitrary` live here and nowhere else — they never\n\
         # enter a shipped artifact's dependency graph, which is what the D2 unsafe\n\
         # audit checks.\n\
         #\n\
         # Every path dependency below is `optional = true`, gated behind the\n\
         # feature named after the crate that declares it. `default` enables every\n\
         # feature, so a plain `cargo fuzz run <target>` still builds everything —\n\
         # but `cargo fuzz run <target> --no-default-features --features <feature>`\n\
         # builds only that target's own crate and whatever it references, so a\n\
         # syntax error in an unrelated crate cannot block it.\n\
         \n\
         [package]\n\
         name = \"vaco-fuzz\"\n\
         version = \"0.0.0\"\n\
         publish = false\n\
         edition = \"2024\"\n\
         \n\
         [package.metadata]\n\
         cargo-fuzz = true\n\
         \n\
         [dependencies]\n\
         libfuzzer-sys = \"0.4\"\n\
         arbitrary = { version = \"1\", features = [\"derive\"] }\n",
    );
    for c in &referenced {
        let layer = layer_of.get(c).map_or("core", String::as_str);
        out.push_str(&format!(
            "{c} = {{ path = \"../crates/{layer}/{c}\", optional = true }}\n"
        ));
    }

    out.push_str("\n[features]\ndefault = [\n");
    for c in &used {
        out.push_str(&format!("    \"{}\",\n", feature_of(c)));
    }
    out.push_str("]\n");
    for c in &used {
        let deps = deps_of.get(c).cloned().unwrap_or_default();
        let dep_list = deps
            .iter()
            .map(|d| format!("\"dep:{d}\""))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("{} = [{}]\n", feature_of(c), dep_list));
    }

    out.push_str(
        "\n# Overflow checks ON in the profile the fuzzer actually runs, so arithmetic\n\
         # overflow is a finding rather than a silent wrap (plan 13 §2.2.1). The shipped\n\
         # release profile keeps them off for speed; the nightly `release-overflow` job\n\
         # closes the gap for everything else.\n\
         [profile.release]\n\
         overflow-checks = true\n\
         debug-assertions = true\n",
    );

    for (target, c) in entries {
        out.push_str(&format!(
            "\n[[bin]]\n\
             name = \"{target}\"\n\
             path = \"fuzz_targets/{target}.rs\"\n\
             test = false\n\
             doc = false\n\
             bench = false\n\
             required-features = [\"{}\"]\n",
            feature_of(c)
        ));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layers(pairs: &[(&str, &str)]) -> Map<String, String> {
        pairs
            .iter()
            .map(|(c, l)| (c.to_string(), l.to_string()))
            .collect()
    }

    fn refs(pairs: &[(&str, &[&str])]) -> Map<String, Set<String>> {
        pairs
            .iter()
            .map(|(t, cs)| (t.to_string(), cs.iter().map(|c| c.to_string()).collect()))
            .collect()
    }

    /// Every path dependency the generator emits must be optional, or a
    /// `--no-default-features --features x` build still pulls in crates that
    /// have nothing to do with `x`.
    #[test]
    fn every_dependency_is_optional() {
        let entries = vec![("frame_alloc".to_string(), "vaco-frame".to_string())];
        let referenced_by = refs(&[("frame_alloc", &["vaco-frame", "vaco-pixfmt"])]);
        let layer_of = layers(&[("vaco-frame", "model"), ("vaco-pixfmt", "model")]);

        let out = render(&entries, &referenced_by, &layer_of);
        for line in out.lines().filter(|l| l.starts_with("vaco-")) {
            assert!(
                line.contains("optional = true"),
                "dependency line is not optional: {line}"
            );
        }
    }

    /// A feature must enable `dep:` for exactly the crates its own targets
    /// reference — no more (or a healthy sibling crate is still pulled in)
    /// and no less (or the target fails to link).
    #[test]
    fn feature_enables_exactly_its_targets_dependencies() {
        let entries = vec![("frame_alloc".to_string(), "vaco-frame".to_string())];
        let referenced_by = refs(&[(
            "frame_alloc",
            &["vaco-frame", "vaco-pixfmt", "vaco-limits", "vaco-pool"],
        )]);
        let layer_of = layers(&[
            ("vaco-frame", "model"),
            ("vaco-pixfmt", "model"),
            ("vaco-limits", "core"),
            ("vaco-pool", "model"),
        ]);

        let out = render(&entries, &referenced_by, &layer_of);
        let line = out
            .lines()
            .find(|l| l.starts_with("frame = ["))
            .expect("frame feature line");
        for dep in ["vaco-frame", "vaco-pixfmt", "vaco-limits", "vaco-pool"] {
            assert!(
                line.contains(&format!("\"dep:{dep}\"")),
                "{line} is missing dep:{dep}"
            );
        }
        // An unrelated crate declared by nobody must not sneak into this
        // feature's dependency list.
        assert!(!line.contains("dep:vaco-registry"));
    }

    /// Two targets declaring the same crate contribute the union of their
    /// references to that crate's feature, so enabling the feature is enough
    /// to build either one.
    #[test]
    fn shared_feature_unions_both_targets_references() {
        let entries = vec![
            ("core_a".to_string(), "vaco-core".to_string()),
            ("core_b".to_string(), "vaco-core".to_string()),
        ];
        let referenced_by = refs(&[
            ("core_a", &["vaco-core", "vaco-expr"]),
            ("core_b", &["vaco-core", "vaco-bitstream"]),
        ]);
        let layer_of = layers(&[
            ("vaco-core", "core"),
            ("vaco-expr", "core"),
            ("vaco-bitstream", "core"),
        ]);

        let out = render(&entries, &referenced_by, &layer_of);
        let line = out
            .lines()
            .find(|l| l.starts_with("core = ["))
            .expect("core feature line");
        assert!(line.contains("dep:vaco-expr"));
        assert!(line.contains("dep:vaco-bitstream"));
    }

    /// `default` must still list every feature: the plain `cargo fuzz run
    /// <target>` invocation is not allowed to regress into "requires the
    /// features" the way hand-editing this file once did.
    #[test]
    fn default_lists_every_feature() {
        let entries = vec![
            ("a_target".to_string(), "vaco-core".to_string()),
            ("b_target".to_string(), "vaco-frame".to_string()),
        ];
        let referenced_by = refs(&[
            ("a_target", &["vaco-core"]),
            ("b_target", &["vaco-frame"]),
        ]);
        let layer_of = layers(&[("vaco-core", "core"), ("vaco-frame", "model")]);

        let out = render(&entries, &referenced_by, &layer_of);
        let default_block = out
            .split("default = [")
            .nth(1)
            .and_then(|s| s.split(']').next())
            .expect("default block");
        assert!(default_block.contains("\"core\""));
        assert!(default_block.contains("\"frame\""));
    }

    /// A dependency nothing references must not appear at all, so a crate
    /// removed from every target's body drops out of the manifest instead of
    /// lingering as dead weight nothing can enable.
    #[test]
    fn unreferenced_crate_is_absent() {
        let entries = vec![("a_target".to_string(), "vaco-core".to_string())];
        let referenced_by = refs(&[("a_target", &["vaco-core"])]);
        let layer_of = layers(&[("vaco-core", "core"), ("vaco-frame", "model")]);

        let out = render(&entries, &referenced_by, &layer_of);
        assert!(!out.contains("vaco-frame"));
    }
}
