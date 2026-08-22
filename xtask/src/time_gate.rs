//! The OS clock is reached through `vaco-time` and nowhere else (D18).
//!
//! # The hole this closes
//!
//! [`crate::wasm`] answers "does this crate compile for wasm32?", and that is a
//! weaker question than it sounds. `std::time::Instant::now()` **compiles**
//! for `wasm32-unknown-unknown` and then panics when called, so a crate can
//! pass `wasm-check` and still be unusable on the target it just passed for.
//! A compile gate cannot see a runtime panic.
//!
//! Found by reading, not by a gate: `vaco-protocol-file`'s `follow` read built
//! its deadline with `std::time::Instant`, and `vaco-conformance` still does.
//! Both passed `wasm-check` every time it has run.
//!
//! # What counts as OS coupling, and what does not
//!
//! `Duration` is **not** flagged. It is arithmetic over two integers with no
//! syscall behind it, `vaco_time` re-exports `core::time::Duration`, and the
//! two spellings name the same type — flagging it would be noise that trains
//! people to ignore the gate.
//!
//! The flagged set is the part that talks to the operating system: reading a
//! clock, and blocking or spawning a thread.
//!
//! # The type is on the list, and there is a line-level escape hatch
//!
//! Strictly, `SystemTime::now()` is what panics; `SystemTime` the *type* is
//! inert, and a value handed to you by `fs::Metadata::modified()` costs
//! nothing to convert. So flagging the type over-reports.
//!
//! It stays on the list anyway, because the finding that motivated this gate
//! was a type in a **trait's data model** — `DirEntry.modified` was
//! `Option<SystemTime>`, which obliged every implementer of `Protocol` to
//! produce an OS type on a target that cannot make one. No `cfg` reaches into
//! an interface. Catching that is worth some noise.
//!
//! The noise is paid for with a line-level escape hatch rather than by
//! loosening the rule: a trailing `// time-gate: <reason>` on the line, or a
//! `// time-gate: <reason>` on the line above, permits one use and leaves the
//! reason in the source. Same shape as the unsafe audit's exemptions. Use it
//! for converting a value the OS already gave you; do not use it for `now()`.
//!
//! # Why an allowlist of crates rather than of lines
//!
//! Same reasoning as [`crate::wasm`]'s `NATIVE_ONLY`, and the entries overlap
//! for the same reason: a crate whose whole job is to run the reference binary
//! and diff its output is native by nature, and pretending otherwise would put
//! a suppression on every line instead of one honest note in one place.

use crate::{Task, crates};

/// OS-coupled time and threading APIs, and what to use instead.
const FORBIDDEN: &[(&str, &str)] = &[
    ("std::time::Instant", "vaco_time::Instant"),
    ("std::time::SystemTime", "vaco_time::unix_nanos"),
    ("std::time::UNIX_EPOCH", "vaco_time::unix_nanos"),
    ("SystemTime::now", "vaco_time::unix_nanos"),
    ("std::thread::sleep", "vaco_time::sleep"),
    ("thread::sleep", "vaco_time::sleep"),
    ("std::thread::spawn", "a driver the caller supplies (D18)"),
];

/// Crates that may reach the OS clock directly, each with the reason.
///
/// Keep this short. An entry is a statement that the crate can never run on a
/// target without an OS, which is true of a tool that shells out to another
/// binary and of almost nothing else.
const NATIVE_ONLY: &[(&str, &str)] = &[
    (
        "vaco-time",
        "it *is* the door; the cfg'd backends are the whole point.",
    ),
    (
        "vaco-conformance",
        "spawns the reference binary and diffs its output. A differential \
         harness cannot exist on a target with no processes, so its clock use \
         is not a portability question.",
    ),
    (
        "vaco-protocol-http",
        "already NATIVE_ONLY for wasm-check: sockets and TLS. Socket timeouts \
         are std::net's own API and take std durations.",
    ),
];

/// Ignore `tests/`, `benches/`, `examples/` and fuzz targets: none of them ship,
/// and a test timing itself is not a portability claim.
fn is_shipped(p: &std::path::Path) -> bool {
    !p.components().any(|c| {
        matches!(
            c.as_os_str().to_str(),
            Some("tests" | "benches" | "examples" | "fuzz_targets")
        )
    })
}

pub fn run(_check: bool) -> Task {
    let mut findings = Vec::new();
    let mut scanned = 0_usize;

    for (_layer, name, path) in crates() {
        if NATIVE_ONLY.iter().any(|(n, _)| *n == name) {
            continue;
        }
        let mut stack = vec![path];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if p.file_name().and_then(|x| x.to_str()) != Some("target") {
                        stack.push(p);
                    }
                    continue;
                }
                if p.extension().and_then(|x| x.to_str()) != Some("rs") || !is_shipped(&p) {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&p) else {
                    continue;
                };
                scanned += 1;
                let lines: Vec<&str> = text.lines().collect();
                for (n, line) in lines.iter().enumerate() {
                    let t = line.trim_start();
                    // Escape hatch: the marker on this line, or anywhere in
                    // the contiguous comment block directly above it. The
                    // block, not just one line, because rustfmt moves a
                    // trailing comment off a long signature and a gate that
                    // rustfmt can defeat is not a gate.
                    let mut waived = line.contains("// time-gate:");
                    let mut i = n;
                    while !waived {
                        let Some(prev) = i.checked_sub(1).and_then(|k| lines.get(k)) else {
                            break;
                        };
                        let p = prev.trim_start();
                        if !p.starts_with("//") {
                            break;
                        }
                        waived = p.starts_with("// time-gate:");
                        i -= 1;
                    }
                    if waived {
                        continue;
                    }
                    // A mention in prose is not a use. This crate's own module
                    // doc names every forbidden path, and so does
                    // `vaco-protocol-file`'s comment explaining why it stopped
                    // using one — a gate that fires on its own explanation is
                    // a gate people delete.
                    if t.starts_with("//") || t.starts_with("#!") {
                        continue;
                    }
                    for (bad, instead) in FORBIDDEN {
                        if line.contains(bad) {
                            findings.push(format!(
                                "  {}:{}: {bad} — use {instead}",
                                p.display(),
                                n + 1
                            ));
                        }
                    }
                }
            }
        }
    }

    if !findings.is_empty() {
        findings.sort();
        findings.dedup();
        return Err(format!(
            "{} use(s) of an OS-coupled time or threading API outside \
             `vaco-time` (D18):\n{}\n\nThese compile for wasm32 and panic at \
             run time, so `wasm-check` cannot see them. Route through \
             `vaco-time`, or — if the crate genuinely cannot exist without an \
             OS — add it to NATIVE_ONLY in xtask/src/time_gate.rs with the \
             reason.\n\nNote that `vaco_time::Instant` is a *stopped* clock \
             where there is no monotonic source, so a loop bounded only by a \
             deadline becomes a hang rather than a panic. Bound polling loops \
             by an iteration count as well.",
            findings.len(),
            findings.join("\n")
        ));
    }

    println!(
        "time-gate: {scanned} shipped files reach the clock only through \
         vaco-time ({} crate(s) exempt)",
        NATIVE_ONLY.len()
    );
    Ok(())
}
