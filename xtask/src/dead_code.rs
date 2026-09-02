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
///
/// `pub(crate)`, not private: [`crate::option_consumption`] reuses this
/// rather than re-deriving it, after the same gap (test code silently
/// counting as production dispatch) turned up there during rule I's
/// cross-scanner audit of this file's own two blind spots.
pub(crate) fn strip_cfg_test(text: &str) -> String {
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

/// How many times `id` occurs in `text` as a whole identifier — not merely
/// as a substring of a longer one.
///
/// A plain `text.matches(id).count()` (this scan's first version) counts
/// `resolve` inside `resolve_pcm`, `unresolved`, or a string/comment that
/// happens to contain the letters, as "used" — a false negative for dead
/// code, and a worse one than the false positives this module's own doc
/// already warns about (trait dispatch, re-exports, macro-built names): a
/// false positive here is silence about something that might be fine, a
/// false negative is `dead-code` not reporting an item its whole reason for
/// existing is to report. Found during rule I's cross-scanner audit of the
/// two blind spots that fooled it (test code counted as real usage, and
/// scope wide enough to let an unrelated symbol vouch for a dead one) — this
/// is the second shape in a different gate, not the same bug repeated.
fn identifier_occurrences(text: &str, id: &str) -> usize {
    let is_ident_char = |c: char| c.is_alphanumeric() || c == '_';
    let bytes = text.as_bytes();
    let mut count = 0;
    let mut i = 0;
    while let Some(rel) = text.get(i..).and_then(|s| s.find(id)) {
        let start = i + rel;
        let end = start + id.len();
        let before_ok = start == 0 || !is_ident_char(bytes[start - 1] as char);
        let after_ok = end >= bytes.len() || !is_ident_char(bytes[end] as char);
        if before_ok && after_ok {
            count += 1;
        }
        i = start + 1;
    }
    count
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
    // Strictly deader: not used in production *and* not used in tests either.
    // The first version of this pass dropped these on the floor — the report
    // only listed items `test_text` mentioned — so an item with no reference
    // anywhere was invisible while an item a test exercises was flagged.
    // `vaco_mux_mp4::meta::build_chapter_tref` sat here unseen.
    let mut orphans: Vec<(String, String)> = Vec::new();
    for (id, owner) in &defined {
        // Used anywhere in production code other than its own definition line?
        let mut used = false;
        for (crate_name, text) in &prod {
            let hits = identifier_occurrences(text, id.as_str());
            let threshold = usize::from(crate_name == owner); // its own definition
            if hits > threshold {
                used = true;
                break;
            }
        }
        if !used {
            if identifier_occurrences(&test_text, id.as_str()) > 0 {
                findings.push((owner.clone(), id.clone()));
            } else {
                orphans.push((owner.clone(), id.clone()));
            }
        }
    }

    findings.sort();
    orphans.sort();
    report_orphans(&orphans);
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

/// Public items with no reference anywhere — not in production, not in a test.
///
/// Reported above the test-only list and separately from it, because the
/// question is different. A test-only item is asking "should this be
/// `pub(crate)`?". An item with no reference at all is asking "why is this
/// here?" — and the honest answers are usually "a feature that was written
/// before the thing that would call it" or "a leftover". Neither is a
/// verdict: the same name-based limits apply, so a trait method, a re-export
/// or a macro-built name can land here innocently.
fn report_orphans(orphans: &[(String, String)]) {
    if orphans.is_empty() {
        println!("dead-code: every public item has at least one reference");
        return;
    }
    let crates_hit: Set<&str> = orphans.iter().map(|(c, _)| c.as_str()).collect();
    println!(
        "dead-code: {} public item(s) across {} crate(s) have NO reference at all \
         — not even a test.",
        orphans.len(),
        crates_hit.len()
    );
    let mut last = String::new();
    for (owner, id) in orphans {
        if *owner != last {
            println!("  {owner}");
            last.clone_from(owner);
        }
        println!("    {id}");
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_occurrences_requires_a_word_boundary() {
        // `resolve` must not count inside `resolve_pcm` or `unresolved` —
        // the false-negative this scan's first version had.
        assert_eq!(identifier_occurrences("resolve_pcm(x)", "resolve"), 0);
        assert_eq!(identifier_occurrences("unresolved", "resolve"), 0);
        assert_eq!(identifier_occurrences("let y = resolve(x);", "resolve"), 1);
        assert_eq!(
            identifier_occurrences("resolve(a); resolve(b);", "resolve"),
            2
        );
    }

    #[test]
    fn identifier_occurrences_finds_a_leading_or_trailing_match() {
        assert_eq!(identifier_occurrences("resolve", "resolve"), 1);
        assert_eq!(identifier_occurrences("(resolve)", "resolve"), 1);
    }

    #[test]
    fn strip_cfg_test_removes_a_test_module_body() {
        let text = "fn a() {}\n#[cfg(test)]\nmod tests {\n    fn b() {}\n}\nfn c() {}\n";
        let stripped = strip_cfg_test(text);
        assert!(stripped.contains("fn a()"));
        assert!(stripped.contains("fn c()"));
        assert!(!stripped.contains("fn b()"));
    }
}
