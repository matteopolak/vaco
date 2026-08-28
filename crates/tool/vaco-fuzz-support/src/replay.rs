//! Replaying a directory of stored inputs as an ordinary test (plan 13 §2.5.4
//! point 5): "`fuzz/regressions` is replayed as an ordinary `cargo test`... so
//! PR CI catches reintroduction in seconds without a fuzzer."
//!
//! # Where the inputs actually live
//!
//! The plan's own text names `fuzz/regressions/<target>/`, but this
//! project's committed convention (see AGENT-CONSTRAINTS.md's Fuzzing
//! section) is `fuzz/seeds/<target>/`: "move the input to
//! `fuzz/seeds/<target>/` — that directory is committed, while
//! `fuzz/corpus/` is gitignored, so it is the only place a regression seed
//! survives." This module is agnostic to which directory a caller points it
//! at — [`replay_dir`] takes any path — so it works with the plan's name,
//! this project's actual name, or a test's own temp directory.
//!
//! # What is and is not wired up here
//!
//! This module provides the *mechanism* — walk a directory, run a caller's
//! closure over each file, collect which ones panicked. Wiring an actual
//! `#[test] fn fuzz_regressions()` per fuzz target that calls the target's
//! own body against `fuzz/seeds/<target>/` is a separate, per-target
//! integration this crate does not do: today's `fuzz_target!` bodies are
//! closures inside `fuzz/fuzz_targets/*.rs`, not exported functions this
//! crate could call, and there are on the order of 190 of them. That wiring
//! is a real next step, not implied to be done by this module's existence.

use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};

/// One file that panicked during a [`replay_dir`] run.
#[derive(Debug)]
pub struct ReplayFailure {
    pub path: PathBuf,
    /// The panic payload, downcast to a string where possible (`panic!` with
    /// a `&str` or `String` message covers the overwhelming majority of
    /// cases; anything else renders as a fixed placeholder rather than
    /// silently dropping the failure).
    pub message: String,
}

/// Run `body` over every regular file directly inside `dir` (not recursive —
/// a fuzz corpus/seed directory is flat), catching a panic per file rather
/// than aborting the whole replay at the first one, so a run reports every
/// regression at once instead of one per `cargo test` invocation.
///
/// An empty or missing `dir` is not an error: a target with no seeds yet
/// (or a project that has not created `fuzz/seeds/<target>/` at all) simply
/// replays nothing, which is the right behaviour for a check that should
/// start passing trivially and gain teeth as regressions accumulate.
///
/// # Errors
/// An I/O error reading `dir` itself (permissions, not-a-directory). A
/// missing directory is treated as empty, not an error — see above.
pub fn replay_dir(dir: &Path, mut body: impl FnMut(&[u8])) -> io::Result<Vec<ReplayFailure>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    let mut failures = Vec::new();
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            paths.push(entry.path());
        }
    }
    paths.sort();

    for path in paths {
        let bytes = std::fs::read(&path)?;
        let result = panic::catch_unwind(AssertUnwindSafe(|| body(&bytes)));
        if let Err(payload) = result {
            let message = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic payload was not a string".to_owned());
            failures.push(ReplayFailure { path, message });
        }
    }

    Ok(failures)
}

/// [`replay_dir`], but panics itself (with every failing path and message
/// listed) if anything failed — the shape a `#[test]` wants, so reintroducing
/// a fixed bug fails CI with the regressed file's name in the output.
///
/// # Panics
/// If `replay_dir` reports any [`ReplayFailure`], or if reading `dir` itself fails.
/// Panicking on both is this function's entire documented contract — it
/// exists to turn "a stored regression input misbehaves again" into a test
/// failure, so `#[allow(clippy::panic)]` here is the API working as
/// designed, not an escape from the workspace's untrusted-input policy
/// (this never runs on untrusted input; `dir` and its contents are the
/// caller's own committed fixtures).
#[expect(
    clippy::panic,
    reason = "this function's entire contract is failing loudly; see its own doc comment"
)]
pub fn replay_dir_or_panic(dir: &Path, body: impl FnMut(&[u8])) {
    use std::fmt::Write as _;

    let failures = match replay_dir(dir, body) {
        Ok(f) => f,
        Err(e) => panic!("replay_dir_or_panic: could not read {}: {e}", dir.display()),
    };
    if !failures.is_empty() {
        let mut report = format!(
            "{} of the regression seeds in {} panicked again:\n",
            failures.len(),
            dir.display()
        );
        for f in &failures {
            let _ = writeln!(report, "  {}: {}", f.path.display(), f.message);
        }
        panic!("{report}");
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "a failing expectation in a test is a failing test, and this file's tests \
              deliberately panic inside a replayed closure to prove `replay_dir` catches it"
)]
mod tests {
    use std::fs;

    use super::{replay_dir, replay_dir_or_panic};

    #[test]
    fn missing_directory_replays_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let failures = replay_dir(&missing, |_| {}).unwrap();
        assert!(failures.is_empty());
    }

    #[test]
    fn every_file_is_visited_in_a_stable_order() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("b.bin"), b"two").unwrap();
        fs::write(dir.path().join("a.bin"), b"one").unwrap();
        let mut seen = Vec::new();
        let failures = replay_dir(dir.path(), |bytes| seen.push(bytes.to_vec())).unwrap();
        assert!(failures.is_empty());
        assert_eq!(seen, vec![b"one".to_vec(), b"two".to_vec()]);
    }

    #[test]
    fn a_panicking_file_is_reported_and_does_not_stop_the_others() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("bad.bin"), b"bad").unwrap();
        fs::write(dir.path().join("good.bin"), b"good").unwrap();
        let mut visited = 0;
        let failures = replay_dir(dir.path(), |bytes| {
            visited += 1;
            assert_ne!(bytes, b"bad", "synthetic regression");
        })
        .unwrap();
        assert_eq!(visited, 2, "both files must be attempted");
        assert_eq!(failures.len(), 1);
        assert!(failures[0].path.ends_with("bad.bin"));
    }

    #[test]
    #[should_panic(expected = "panicked again")]
    fn replay_dir_or_panic_panics_on_any_failure() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("bad.bin"), b"x").unwrap();
        replay_dir_or_panic(dir.path(), |_| panic!("synthetic"));
    }

    #[test]
    fn replay_dir_or_panic_is_silent_on_success() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("ok.bin"), b"x").unwrap();
        replay_dir_or_panic(dir.path(), |_| {});
    }
}
