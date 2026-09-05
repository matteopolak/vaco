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
//! improvement: re-pin [`BASELINE`] to the new count in the same commit,
//! and the ratchet has moved forward and cannot move back on its own. The
//! count prints on every run, success or failure, so the direction of
//! travel is never hidden behind a bare pass.
//!
//! A ratchet whose baseline creeps upward is an allowlist with extra steps.
//! The only legitimate reason to raise [`BASELINE`] is that the count above
//! it is wrong — recounted after a rename, a moved directory, a change to
//! what this file itself scans — never "there are more violations now and
//! that is fine."
//!
//! # The two blind spots checked for elsewhere do not apply here
//!
//! Audited against the same two shapes rule I found in itself while auditing
//! every other scanner in this file: this one has no "is X used/consumed
//! elsewhere" cross-file lookup at all, so there is no name to collide and
//! no scope to get wrong — every finding is local to the one file and line
//! it names. `#[cfg(test)]` code is deliberately **not** exempt (unlike
//! `dead_code`'s production/test split): a 40-line comment or a stale
//! planning-doc citation is exactly as much a style problem inside a test
//! module as outside one, so scanning it uniformly is the intended
//! behaviour here, not an oversight to fix.

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

/// The violation count as of the last commit to move this ratchet, measured
/// by this same scan. Fix violations and re-pin this to the new count in the
/// same commit — subtracting how many you fixed is wrong whenever the count
/// was already above the baseline, which is when anyone is looking. Never
/// raise it to make a new violation disappear — see the module doc above.
const BASELINE: usize = 641;

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
///
/// Also not an issue number: the reference's own stream-index notation --
/// `Stream #0:0`, `[vost#0:0]`, `[out#0/matroska ...]`, `[in#0]` -- pinned
/// verbatim in this crate's own regression comments as *measured output*,
/// not a pointer to a ticket. Distinguished from a real citation by shape,
/// not by an allowlist of strings: a `#` glued directly onto a preceding
/// word character (`vost#0`, `in#0`, `out#0` -- no space, no punctuation
/// between them) is always this crate's own log-tag prefix, never how a
/// citation is written anywhere in this tree (every real one seen here is
/// `#123` preceded by whitespace, `/`, `(`, or the start of the comment).
/// Likewise a digit run immediately followed by `:<digit>` (a second
/// stream index, `#0:0`) or by `/<letter>` (a container name, `#0/mp4`) is
/// the reference's own colon/slash-separated notation, not a citation --
/// no real issue reference in this tree is ever followed by either shape.
///
/// And a run that is numerically **zero** is never a citation either:
/// issue numbering starts at 1, so `#0` is always something else. All 26
/// occurrences in this tree are the reference's own index notation in a
/// shape the rules above do not reach (`Input #0, from …`, `Outputs: #0:
/// default`, `Slave muxer #0 failed`), a hex colour (`#00ff00`), a hex
/// stream id (`#0x10`) or a group specifier (`g:#0`).
fn issue_ref(line: &str) -> Option<&str> {
    let mut rest = line;
    let mut consumed = 0usize;
    while let Some(at) = rest.find('#') {
        let after = rest.get(at + 1..)?;
        let digits: &str = after
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap_or_default();
        if (1..=3).contains(&digits.len()) && digits.bytes().any(|b| b != b'0') {
            let hash_pos = consumed + at;
            let glued_to_a_word = line
                .get(..hash_pos)
                .and_then(|s| s.chars().next_back())
                .is_some_and(|c| c.is_ascii_alphanumeric());
            let tail = after.get(digits.len()..).unwrap_or_default();
            let stream_pair =
                tail.starts_with(':') && tail[1..].starts_with(|c: char| c.is_ascii_digit());
            let stream_name =
                tail.starts_with('/') && tail[1..].starts_with(|c: char| c.is_ascii_alphabetic());
            if !glued_to_a_word && !stream_pair && !stream_name {
                return Some(digits);
            }
        }
        consumed += at + 1;
        rest = after;
    }
    None
}

/// Collects every `.rs` file `git` tracks under `dir`, filtered against
/// [`crate::tracked_files`] for the same reason [`crate::rust_files`] is:
/// this is a ratchet gate (see this module's own doc), and an untracked,
/// in-progress scratch file sharing this tree with a concurrent agent could
/// otherwise push the count over [`BASELINE`] and fail the build over
/// comments nobody has committed.
fn collect(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let known = crate::tracked_files();
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs")
            && known.as_ref().is_none_or(|k| k.contains(&p))
        {
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

    /// Regression for the false positives found auditing `vaco-cli`'s own
    /// comments: the reference's stream-index notation reads exactly like a
    /// short issue number by digit count alone, and must not be flagged.
    #[test]
    fn the_references_own_stream_notation_is_not_an_issue_reference() {
        assert_eq!(
            issue_ref("/// `Stream #0:0 -> #0:0 (copy)`, one per output"),
            None
        );
        assert_eq!(
            issue_ref("/// [vost#0:0] Streamcopy requested for output"),
            None
        );
        assert_eq!(
            issue_ref("/// [out#0/matroska @ 0x…] Error opening output"),
            None
        );
        assert_eq!(
            issue_ref("/// [in#0] -to value smaller than -ss; aborting."),
            None
        );
        assert_eq!(issue_ref("/// [out#0/null] video:7KiB audio:16KiB"), None);
    }

    /// `#0` is not an issue number in any repository -- numbering starts at
    /// 1 -- so the reference's index notation is safe in every shape,
    /// including the ones the punctuation rules above do not reach.
    #[test]
    fn a_zero_is_never_an_issue_reference() {
        assert_eq!(issue_ref("//! `Input #0, from 'f.mp4':` — the dump"), None);
        assert_eq!(issue_ref("//! `Outputs: #0: default (video)`"), None);
        assert_eq!(issue_ref("//! measured: `<font color=\"#00ff00\">`"), None);
        assert_eq!(issue_ref("//! stream specifiers: `#0x10`, `g:#0`"), None);
        assert_eq!(issue_ref("// `Input #0` dump, see #641"), Some("641"));
    }

    /// A real citation preceded by `/`, `(`, or nothing at all (start of
    /// comment) must still be caught -- only a `#` glued directly onto a
    /// preceding word character is the reference's own notation.
    #[test]
    fn a_real_citation_next_to_punctuation_is_still_found() {
        assert_eq!(
            issue_ref("// CL-21/#222: `-fps_mode` carries bit::VIDEO"),
            Some("222")
        );
        assert_eq!(
            issue_ref("// (WHIP, `vaco-mux-whip`, #619) still need"),
            Some("619")
        );
        assert_eq!(
            issue_ref("//#7 at the very start of the comment"),
            Some("7")
        );
    }
}
