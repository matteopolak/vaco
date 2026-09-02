//! Keep comments short and local.
//!
//! # Why this is a ratchet, not a pass/fail count
//!
//! This gate existed for a while before anything ran it: it is a real,
//! exit-1 check, but it was wired into neither `just ci` nor
//! `.github/workflows/ci.yml`'s policy job, so nobody could ever have seen it
//! fail. By the time that was noticed, 1232 violations had accumulated —
//! almost entirely comments citing a planning document or an issue number,
//! the two things this codebase's own writing habits reach for constantly.
//! Wiring the gate in at a hard zero would turn the very next commit red for
//! a backlog that predates it, which is a tree-wide decision nobody made on
//! purpose.
//!
//! [`BASELINE`] is that number, pinned once, committed like any other source
//! constant. The gate fails only when the current count *exceeds* it — a new
//! violation is caught the moment it lands, and the 1232 that already exist
//! are grandfathered, not blessed. Cleaning any of them up is a strict
//! improvement: lower [`BASELINE`] by the same amount in the same commit,
//! and the ratchet has moved forward and cannot move back on its own. The
//! count prints on every run, success or failure, so the direction of
//! travel is never hidden behind a bare pass.
//!
//! A ratchet whose baseline creeps upward is an allowlist with extra steps.
//! The only legitimate reason to raise [`BASELINE`] is that the count above
//! it is wrong — recounted after a rename, a moved directory, a change to
//! what this file itself scans — never "there are more violations now and
//! that is fine."

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

/// The violation count as of the commit that added this ratchet, measured by
/// this same scan. Fix a violation and lower this number in the same commit;
/// never raise it to make a new one disappear — see the module doc above.
const BASELINE: usize = 1232;

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

    let count = findings.len();
    if count <= BASELINE {
        println!(
            "comment-check: {count} problem(s), at or under the baseline ({BASELINE}) — {} \
             file(s) scanned",
            files.len()
        );
        return Ok(());
    }
    let shown = findings.len().min(40);
    let mut msg = format!(
        "{count} comment problem(s), {} over the baseline ({BASELINE}):\n",
        count - BASELINE
    );
    for f in findings.iter().take(shown) {
        msg.push_str("  ");
        msg.push_str(f);
        msg.push('\n');
    }
    if findings.len() > shown {
        msg.push_str(&format!("  … and {} more\n", findings.len() - shown));
    }
    msg.push_str(&format!(
        "\nBASELINE in xtask/src/comment_check.rs is {BASELINE}; the scan just found {count}. \
         Either fix a violation above (and lower BASELINE by the same amount in the same \
         commit) or leave BASELINE where it is and fix the new one you just added — never \
         raise BASELINE to make this pass. It exists to let the count only go down.\n"
    ));
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
