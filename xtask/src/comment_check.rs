//! Keep comments short and local.

use crate::{Task, repo_root};
use std::path::Path;

/// Longest run of consecutive comment lines a source file may carry.
const MAX_RUN: usize = 40;

/// Substrings that make a comment depend on something outside the file.
const CROSS_REFS: &[&str] = &[
    "CONFORMANCE-FINDINGS",
    "INTERFACE-GAPS",
    "AGENT-CONSTRAINTS",
    "TECH-DEBT",
    "planning/",
];

pub fn run(_check: bool) -> Task {
    let root = repo_root();
    let mut findings = Vec::new();
    let mut files = Vec::new();
    collect(&root.join("crates"), &mut files);
    collect(&root.join("xtask/src"), &mut files);
    files.sort();

    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .display()
            .to_string();
        if rel.ends_with("generated.rs") || rel.contains("/tests/") {
            continue;
        }

        let mut run_start = 0usize;
        let mut run_len = 0usize;
        for (i, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                if run_len == 0 {
                    run_start = i + 1;
                }
                run_len += 1;
                for r in CROSS_REFS {
                    if trimmed.contains(r) {
                        findings.push(format!(
                            "{rel}:{}: comment cites `{r}` — a comment that points at a \
                             planning document goes stale the moment that document is \
                             renumbered, and it has been, twice",
                            i + 1
                        ));
                    }
                }
                if let Some(num) = issue_ref(trimmed) {
                    findings.push(format!(
                        "{rel}:{}: comment cites issue `#{num}` — say what the code does, \
                         not which ticket asked for it",
                        i + 1
                    ));
                }
            } else {
                if run_len > MAX_RUN {
                    findings.push(format!(
                        "{rel}:{run_start}: {run_len} consecutive comment lines (limit \
                         {MAX_RUN}) — split the explanation out or cut it down"
                    ));
                }
                run_len = 0;
            }
        }
        if run_len > MAX_RUN {
            findings.push(format!(
                "{rel}:{run_start}: {run_len} consecutive comment lines (limit {MAX_RUN})"
            ));
        }
    }

    if findings.is_empty() {
        println!("comment-check: OK ({} files)", files.len());
        return Ok(());
    }
    let shown = findings.len().min(40);
    let mut msg = format!("{} comment problem(s):\n", findings.len());
    for f in findings.iter().take(shown) {
        msg.push_str("  ");
        msg.push_str(f);
        msg.push('\n');
    }
    if findings.len() > shown {
        msg.push_str(&format!("  … and {} more\n", findings.len() - shown));
    }
    Err(msg)
}

/// A `#123`-style issue reference, if the comment carries one.
///
/// Four or more digits are not issue numbers in this repository — they are
/// byte counts, sample rates and specification clause numbers.
fn issue_ref(line: &str) -> Option<&str> {
    let mut rest = line;
    while let Some(at) = rest.find('#') {
        let after = rest.get(at + 1..)?;
        let digits: &str = after
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap_or_default();
        if (1..=3).contains(&digits.len()) {
            return Some(digits);
        }
        rest = after;
    }
    None
}

fn collect(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_issue_reference_is_found_but_a_byte_count_is_not() {
        assert_eq!(issue_ref("// see #644 for why"), Some("644"));
        assert_eq!(issue_ref("// the reference writes 50760 bytes"), None);
        assert_eq!(issue_ref("// clause #5.3.2"), Some("5"));
        assert_eq!(issue_ref("// no hash here"), None);
    }

    #[test]
    fn a_four_digit_hash_is_not_an_issue() {
        assert_eq!(issue_ref("// pattern #1234"), None);
    }
}
