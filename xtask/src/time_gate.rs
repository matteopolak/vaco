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
//! # Code that is `cfg`'d out of wasm is not a finding
//!
//! The gate's whole premise is "this compiles for wasm32 and panics when
//! called". That premise is **false** for an item behind
//! `#[cfg(not(target_family = \"wasm\"))]` — it does not compile for wasm at
//! all, so it cannot panic there.
//!
//! `vaco-sched`'s threaded driver is the worked example and the reason this
//! exists: `run_threaded` calls `std::thread::spawn`, and that is exactly
//! right. D18 asks for parallelism to be optional *at the API level*, which it
//! is — the same `Driver::run` compiles and works on wasm, reporting one
//! thread — and the threaded implementation is then correctly compiled out.
//! Reporting it would be telling the author to undo the thing D18 asked for.
//!
//! So before reporting a line, the gate walks up to the item that encloses it
//! and checks the attribute block above that item. It is a text scan, so it
//! sees the common shape (an attribute directly above a top-level `fn`, `impl`
//! or `mod`) and not an arbitrary one; the `// time-gate:` hatch covers the
//! rest.
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
    (
        "vaco-protocol-tls",
        "already NATIVE_ONLY for wasm-check: rustls-rustcrypto/getrandom. \
         `rustls` itself reaches the wall clock internally for certificate \
         expiry checks (not this crate's implementation to route through \
         vaco-time), and this crate's own test suite spawns a real thread to \
         drive an in-process TLS server for a loopback handshake test — a \
         differential-harness-shaped use, same reasoning as vaco-conformance \
         above, not production code running on a target this would matter for.",
    ),
];

/// Whether the item enclosing `line` is compiled out of wasm.
///
/// Walks up to the nearest item header at column 0 — `fn`, `impl`, `mod` — and
/// then reads the contiguous attribute-and-comment block directly above it.
/// That is the shape real code uses; anything more tangled should carry an
/// explicit `// time-gate:` note instead, because a reader will need one too.
fn cfg_excludes_wasm(lines: &[&str], at: usize) -> bool {
    let is_item = |t: &str| {
        t.starts_with("fn ")
            || t.starts_with("pub fn ")
            || t.starts_with("pub(crate) fn ")
            || t.starts_with("impl")
            || t.starts_with("mod ")
            || t.starts_with("pub mod ")
    };
    // The item header, at column 0 (so a method inside an `impl` resolves to
    // the `impl`, which is where the attribute conventionally sits).
    let mut i = at;
    loop {
        let Some(l) = lines.get(i) else { return false };
        if !l.starts_with(char::is_whitespace) && is_item(l) {
            break;
        }
        let Some(prev) = i.checked_sub(1) else {
            return false;
        };
        i = prev;
    }
    // The attribute block above it.
    while let Some(prev) = i.checked_sub(1) {
        let t = lines.get(prev).map_or("", |l| l.trim_start());
        if !(t.starts_with('#') || t.starts_with("//")) {
            return false;
        }
        // `not(target_family = "wasm")` and `not(target_arch = "wasm32")` both
        // mean the same thing here; so does a plain `unix`/`windows` gate.
        if t.starts_with("#[cfg")
            && (t.contains("not(target_family = \"wasm\")")
                || t.contains("not(target_arch = \"wasm32\")")
                || t.contains("target_family = \"unix\"")
                || t.contains("target_os = \"windows\""))
        {
            return true;
        }
        i = prev;
    }
    false
}

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
                    if waived || cfg_excludes_wasm(&lines, n) {
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
