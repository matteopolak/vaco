//! Generate `fuzz/Cargo.toml` from front-matter in the fuzz target files.
//!
//! # Why this is generated
//!
//! `fuzz/Cargo.toml` was the last hand-edited shared file, and it had the worst
//! contention profile of any of them: adding one target meant editing *three*
//! separate regions — `[dependencies]`, `[features]` (twice, once for the
//! feature and once to append to `default`), and a new `[[bin]]` block. Every
//! agent in the project edits it, and none of them owns it.
//!
//! Three separate failures came out of that in one wave:
//!
//! - An agent patched `core = []` with a substring replace, which also matched
//!   `codec-core`, `protocol-core`, `format-core` and `cli-core`. It caught the
//!   damage itself, but nothing would have caught it for anyone else.
//! - The `default` line changed between one agent's read and its write.
//! - `cli-core` and `conformance` ended up missing from `default` entirely, so
//!   `cargo fuzz run cli_specifier` failed with "requires the features" —
//!   three crates' targets unrunnable, from lost edits nobody noticed.
//!
//! # Why the target file is the fragment
//!
//! The registry and the docs index take their fragments from per-crate files.
//! Fuzzing does not need a new file at all: `fuzz/fuzz_targets/<name>.rs`
//! already exists, and it already has exactly one author — the agent that owns
//! the crate under test. Putting the declaration in the target's own header
//! means the thing an agent writes and the thing it declares are the same file,
//! so there is nothing left to contend for.
//!
//! Each target declares its crate in its module docs:
//!
//! ```text
//! //! fuzz-crate: vaco-core
//! ```
//!
//! Everything else — the path dependency, the feature name, the `[[bin]]`
//! block, and `default` — is derived. `default` lists every feature, so a target
//! can never again be silently unrunnable.

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
    // Every crate any target *mentions*, which is a superset of the crates the
    // targets *declare*: `frame_alloc` fuzzes `vaco-frame` but needs
    // `vaco-pixfmt` to build a frame at all. The front-matter says which crate a
    // target is a fuzz target FOR — it is not the dependency list.
    let mut referenced = Set::new();
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
            Some(c) if layer_of.contains_key(&c) => entries.push((target.to_string(), c)),
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

    // Features come from what targets declare; dependencies from what they use.
    let used: Set<String> = entries.iter().map(|(_, c)| c.clone()).collect();
    referenced.extend(used.iter().cloned());
    let feature_of = |c: &str| c.strip_prefix("vaco-").unwrap_or(c).to_string();

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
        out.push_str(&format!("{c} = {{ path = \"../crates/{layer}/{c}\" }}\n"));
    }

    // Every feature is in `default`. The gate exists so `cargo fuzz build
    // --no-default-features --features x` can compile one target instead of all
    // of them (plan 13 §2.1) — not to make targets unrunnable by omission,
    // which is exactly what hand-editing this list caused.
    out.push_str("\n[features]\ndefault = [\n");
    for c in &used {
        out.push_str(&format!("    \"{}\",\n", feature_of(c)));
    }
    out.push_str("]\n");
    for c in &used {
        out.push_str(&format!("{} = []\n", feature_of(c)));
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

    for (target, c) in &entries {
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
    std::fs::write(&dest, out).map_err(|e| format!("{}: {e}", dest.display()))?;
    println!(
        "gen-fuzz: {} targets across {} crates",
        entries.len(),
        used.len()
    );
    Ok(())
}
