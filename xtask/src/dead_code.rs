//! Public API that only tests use.
//!
//! # The gap this fills
//!
//! `dead_code` already catches a *private* item used only from `#[cfg(test)]`:
//! the `cfg` block is not compiled for the lib target, so the item is unused
//! there, and CI's `-D warnings` denies it. Verified by planting one.
//!
//! It does **not** fire for `pub` items, because a public item is reachable in
//! principle. `unreachable_pub` does not fire either — the item genuinely is
//! reachable. So a `pub fn` that no other crate calls and only tests exercise is
//! invisible to every lint in the toolchain. Also verified by planting one.
//!
//! That is worth catching. In a workspace of internal crates, public API with no
//! caller is either an interface nobody adopted, or a helper that should have
//! been `pub(crate)` — and the second is the common case, because a test in
//! `tests/` can only reach `pub` items, so authors widen visibility to test
//! something and never narrow it again.
//!
//! # It is a name-based heuristic, and reports rather than fails
//!
//! References are found by scanning text for the identifier. That cannot see a
//! method called through a trait object, an item reached via a re-export under a
//! different path, or a name built by a macro. So this **prints** and returns
//! success; it is a report to read at a wave boundary, not a gate to block on.
//! Making it a hard gate would need an allowlist that would quickly become the
//! place real findings go to hide — the failure mode [`crate::dup_check`]
//! guards against by demanding a written reason per row.

use crate::{Map, Set, Task, crates};

/// Items whose name is too common to attribute, or whose use is structural.
const IGNORE: &[&str] = &[
    "new", "default", "fmt", "from", "into", "len", "is_empty", "next", "parse", "name", "kind",
    "get", "set", "run", "open", "read", "write", "flush",
];

fn is_test_path(p: &std::path::Path) -> bool {
    p.components().any(|c| {
        matches!(
            c.as_os_str().to_str(),
            Some("tests" | "benches" | "examples" | "fuzz_targets")
        )
    })
}

/// Every `.rs` file under `dir`, with its text.
fn rust_files(dir: &std::path::Path) -> Vec<(std::path::PathBuf, String)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|x| x.to_str()) == Some("rs")
                && let Ok(t) = std::fs::read_to_string(&p)
            {
                out.push((p, t));
            }
        }
    }
    out
}

/// Strip `#[cfg(test)]`-guarded modules, crudely: from the attribute to the
/// matching brace at the same depth. Good enough to stop a unit test counting
/// as production use, which is the whole question.
fn strip_cfg_test(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("#[cfg(test)]") {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        let Some(brace) = after.find('{') else {
            break;
        };
        let mut depth = 0_i32;
        let mut end = None;
        for (n, c) in after.char_indices().skip(brace) {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(n + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        match end {
            Some(e) => rest = &after[e..],
            None => break,
        }
    }
    out.push_str(rest);
    out
}

pub fn run(_check: bool) -> Task {
    let all = crates();

    // name -> defining crate, for public items.
    let mut defined: Map<String, String> = Map::new();
    // Production text per crate, and test text for the whole workspace.
    let mut prod: Map<String, String> = Map::new();
    let mut test_text = String::new();

    for (_layer, name, path) in &all {
        let mut body = String::new();
        for (p, text) in rust_files(path) {
            if is_test_path(&p) {
                test_text.push_str(&text);
                continue;
            }
            let stripped = strip_cfg_test(&text);
            // The stripped-out part is still test usage.
            if stripped.len() < text.len() {
                test_text.push_str(&text);
            }
            for line in stripped.lines() {
                let t = line.trim_start();
                for kw in [
                    "pub fn ",
                    "pub const fn ",
                    "pub struct ",
                    "pub enum ",
                    "pub trait ",
                    "pub const ",
                    "pub type ",
                ] {
                    if let Some(r) = t.strip_prefix(kw) {
                        let id: String = r
                            .chars()
                            .take_while(|c| c.is_alphanumeric() || *c == '_')
                            .collect();
                        if id.len() > 2 && !IGNORE.contains(&id.as_str()) {
                            defined.entry(id).or_insert_with(|| name.clone());
                        }
                    }
                }
            }
            body.push_str(&stripped);
        }
        prod.insert(name.clone(), body);
    }

    let mut findings: Vec<(String, String)> = Vec::new();
    for (id, owner) in &defined {
        // Used anywhere in production code other than its own definition line?
        let mut used = false;
        for (crate_name, text) in &prod {
            let hits = text.matches(id.as_str()).count();
            let threshold = usize::from(crate_name == owner); // its own definition
            if hits > threshold {
                used = true;
                break;
            }
        }
        if !used && test_text.contains(id.as_str()) {
            findings.push((owner.clone(), id.clone()));
        }
    }

    findings.sort();
    if findings.is_empty() {
        println!("dead-code: no public item is used only by tests");
        return Ok(());
    }

    let crates_hit: Set<&str> = findings.iter().map(|(c, _)| c.as_str()).collect();
    println!(
        "dead-code: {} public item(s) across {} crate(s) appear only in tests.",
        findings.len(),
        crates_hit.len()
    );
    println!(
        "  Name-based, so expect false positives from trait dispatch, re-exports\n  \
         and macro-built names. Each is a question, not a verdict: should it be\n  \
         `pub(crate)`, or is it interface nobody has adopted yet?\n"
    );
    let mut last = String::new();
    for (owner, id) in &findings {
        if *owner != last {
            println!("  {owner}");
            last.clone_from(owner);
        }
        println!("    {id}");
    }
    Ok(())
}
