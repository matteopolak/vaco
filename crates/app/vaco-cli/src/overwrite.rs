//! `-y`/`-n`: the overwrite policy (CLI-option audit). Neither option had a
//! consumer anywhere in this build, and there was no overwrite prompt or
//! refusal of any kind — every run unconditionally truncated its output.
//! `-y` matched that by accident (it asks for exactly the behaviour this
//! build already had); `-n` did not — its whole point is refusing to
//! overwrite, and silently not honouring it is real, quiet data loss for
//! anyone who passed it to protect an existing file.
//!
//! # Measured against `ffmpeg` 9.0.1, deliberately not reproduced everywhere
//!
//! `ffmpeg -n <existing output>` prints
//!
//! ```text
//! File 'out.wav' already exists. Exiting.
//! Error opening output file out.wav.
//! ```
//!
//! and **exits 0** — refusing to overwrite is treated as a successful,
//! intentional outcome, not a failure. With neither `-y` nor `-n`, the
//! reference always prints the interactive prompt and reads a line from
//! stdin regardless of whether stdin is a terminal: redirected from
//! `/dev/null`, that read hits EOF immediately and is treated as "no" —
//! ```text
//! File 'out.wav' already exists. Overwrite? [y/N] Not overwriting - exiting
//! Error opening output file out.wav.
//! ```
//! also exit 0. But a non-tty stdin that does *not* EOF immediately (a pipe
//! still open, an inherited terminal fd in a detached process) blocks on
//! that read forever, which is exactly the failure mode this project's own
//! benchmark harness has hit repeatedly against the reference. [`guard`]
//! deliberately does not reproduce that: it checks
//! [`std::io::IsTerminal`] *before* attempting to read anything, and on a
//! non-terminal stdin refuses immediately with no read at all — the same
//! observable outcome (refuse, exit 0, no prompt text) with no way to hang.
//!
//! # `-y`/`-n` precedence
//!
//! Both are global, argument-less flags; when both are given, whichever
//! occurs later in argv wins, the same "last occurrence wins" rule this
//! crate already applies to every other repeatable option
//! ([`vaco_cli_core::split::CommandLine::last_global`]).

use std::io::{BufRead, IsTerminal, Write};

use vaco_cli_core::split::CommandLine;
use vaco_io::CancelToken;
use vaco_protocol_core::{Access, ProtocolEnv, ProtocolError};

use crate::exit::Diagnostic;

/// What `-y`/`-n` (or neither) resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverwritePolicy {
    /// `-y`, or `-y` given after a `-n`: overwrite without asking.
    Always,
    /// `-n`, or `-n` given after a `-y`: refuse if the destination exists.
    Never,
    /// Neither given: the reference's own default, prompt-or-refuse.
    Ask,
}

/// Resolve `-y`/`-n` from the parsed command line.
#[must_use]
pub fn resolve_policy(line: &CommandLine) -> OverwritePolicy {
    let y = line.last_global("y").map(|o| o.argv_index);
    let n = line.last_global("n").map(|o| o.argv_index);
    match (y, n) {
        (Some(y), Some(n)) => {
            if y > n {
                OverwritePolicy::Always
            } else {
                OverwritePolicy::Never
            }
        }
        (Some(_), None) => OverwritePolicy::Always,
        (None, Some(_)) => OverwritePolicy::Never,
        (None, None) => OverwritePolicy::Ask,
    }
}

/// The two lines the reference prints, reproduced verbatim, for every
/// "not overwriting" outcome regardless of which one triggered it — the
/// reference does not distinguish `-n` from a declined prompt in its own
/// wording either.
fn refuse(url: &str) -> Diagnostic {
    Diagnostic {
        lines: vec![
            format!("File '{url}' already exists. Exiting."),
            format!("Error opening output file {url}."),
        ],
        exit: crate::exit::ExitCode::OK,
    }
}

/// Whether `url` already exists, for the protocols that can answer —
/// `Ok(None)` for a protocol with no [`vaco_protocol_core::Protocol::check`]
/// (a pipe, a network sink): the reference's own overwrite prompt only ever
/// fires for a real, `stat`-able destination, never for those, so "cannot
/// tell" and "does not exist" have the same effect here.
fn exists(url: &str) -> Option<bool> {
    let mut protocols = vaco_registry::protocol_registry();
    vaco_protocol_file::register(&mut protocols);
    let cancel = CancelToken::new();
    let env = ProtocolEnv::new(&protocols, &cancel);
    match protocols.check(crate::output::normalize(url), &env) {
        Ok(Access { .. }) => Some(true),
        Err(ProtocolError::Io(vaco_core::Error::Io(e)))
            if e.kind() == std::io::ErrorKind::NotFound =>
        {
            Some(false)
        }
        Err(_) => None,
    }
}

/// Enforce `policy` against `url`, before the real destination is ever
/// opened for writing.
///
/// # Errors
/// [`Diagnostic`] (exit 0, matching the reference) when the destination
/// exists and `policy` is [`OverwritePolicy::Never`], or resolves to "no"
/// under [`OverwritePolicy::Ask`].
pub fn guard(url: &str, policy: OverwritePolicy) -> Result<(), Diagnostic> {
    if policy == OverwritePolicy::Always {
        return Ok(());
    }
    if exists(url) != Some(true) {
        return Ok(());
    }
    match policy {
        OverwritePolicy::Always => unreachable!("handled above"),
        OverwritePolicy::Never => Err(refuse(url)),
        OverwritePolicy::Ask => {
            let stdin = std::io::stdin();
            if !stdin.is_terminal() {
                // Deliberately no read at all -- see the module doc for why
                // this diverges from the reference here.
                return Err(refuse(url));
            }
            eprint!("File '{url}' already exists. Overwrite? [y/N] ");
            let _ = std::io::stderr().flush();
            let mut line = String::new();
            let answered_yes = stdin
                .lock()
                .read_line(&mut line)
                .is_ok_and(|_| matches!(line.trim().chars().next(), Some('y' | 'Y')));
            if answered_yes {
                Ok(())
            } else {
                Err(refuse(url))
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_cli_core::split::CommandLine;

    fn parse(argv: &[&str]) -> CommandLine {
        let table = vaco_cli_core::table::ffmpeg();
        vaco_cli_core::split_with(&table, argv, &crate::cli::Oracle).unwrap()
    }

    #[test]
    fn neither_flag_asks() {
        let line = parse(&["-i", "in.mp4", "out.mp4"]);
        assert_eq!(resolve_policy(&line), OverwritePolicy::Ask);
    }

    #[test]
    fn y_alone_is_always() {
        let line = parse(&["-y", "-i", "in.mp4", "out.mp4"]);
        assert_eq!(resolve_policy(&line), OverwritePolicy::Always);
    }

    #[test]
    fn n_alone_is_never() {
        let line = parse(&["-n", "-i", "in.mp4", "out.mp4"]);
        assert_eq!(resolve_policy(&line), OverwritePolicy::Never);
    }

    #[test]
    fn last_of_y_and_n_wins() {
        let line = parse(&["-n", "-y", "-i", "in.mp4", "out.mp4"]);
        assert_eq!(resolve_policy(&line), OverwritePolicy::Always);
        let line = parse(&["-y", "-n", "-i", "in.mp4", "out.mp4"]);
        assert_eq!(resolve_policy(&line), OverwritePolicy::Never);
    }

    #[test]
    fn guard_allows_a_destination_that_does_not_exist() {
        let dir = std::env::temp_dir().join(format!("vaco-overwrite-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&dir);
        assert!(guard(dir.to_str().unwrap(), OverwritePolicy::Never).is_ok());
        assert!(guard(dir.to_str().unwrap(), OverwritePolicy::Ask).is_ok());
    }

    #[test]
    fn guard_refuses_an_existing_destination_under_never() {
        let path =
            std::env::temp_dir().join(format!("vaco-overwrite-exists-{}.tmp", std::process::id()));
        std::fs::write(&path, b"x").unwrap();
        let d = guard(path.to_str().unwrap(), OverwritePolicy::Never).unwrap_err();
        assert_eq!(d.exit, crate::exit::ExitCode::OK);
        assert!(d.lines[0].contains("already exists"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn guard_always_overwrites_without_checking() {
        // A path with no parent directory would fail `exists()`'s own check
        // (`ProtocolError` other than `NotFound`), which must not matter
        // for `Always`: it returns before calling `exists` at all.
        assert!(guard("/does/not/exist/at/all.mp4", OverwritePolicy::Always).is_ok());
    }
}
