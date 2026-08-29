//! Own a spawned child process's stdin/stdout/stderr without deadlocking.
//!
//! The hazard this module exists to avoid: an OS pipe has a bounded buffer.
//! If nobody reads the child's stdout while we are blocked writing to its
//! stdin, and the child's stdout buffer fills up, the child blocks writing
//! its own output — which means it stops reading our input, which is what we
//! were blocked writing. Two blocked writers, neither able to make progress.
//! `x264`/`x265` do interleave reading input with writing output (they are
//! not "read everything, then encode, then write everything"), so this is a
//! real risk for any frame count large enough to fill a pipe buffer (a few
//! hundred KB on most systems — a handful of 1080p frames), not a
//! theoretical one.
//!
//! The fix is the standard one: a dedicated thread drains stdout for the
//! entire lifetime of the child, independent of whether or when
//! [`Encoder::receive_packet`](vaco_codec_core::Encoder::receive_packet) is
//! actually called. `send_frame`/`write_stdin` write directly from the
//! caller's thread — that direction is safe to block on, because the only
//! thing that can make the child stop reading stdin is the stdout hazard
//! above, which the reader thread already forecloses.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use vaco_core::{Error, Result};

/// How many trailing stderr lines to keep for an error message. Bounded so a
/// chatty or hostile encoder cannot grow this without limit — this is
/// diagnostic text, not stream data, so `vaco_limits::Budget` is the wrong
/// tool here; a fixed cap is the right one.
const STDERR_TAIL_LINES: usize = 64;

/// A spawned child, with its stdout continuously drained on a background
/// thread and its stderr tail kept for diagnostics.
///
/// No derived `Debug`: `Receiver<Vec<u8>>` does not implement it. A manual
/// impl below prints the one field a caller debugging a stuck encode
/// actually wants — the captured stderr tail.
pub struct ExecProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout_rx: Receiver<Vec<u8>>,
    stdout_thread: Option<JoinHandle<()>>,
    stderr_thread: Option<JoinHandle<()>>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
}

impl std::fmt::Debug for ExecProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecProcess").field("stderr_tail", &self.stderr_tail()).finish_non_exhaustive()
    }
}

impl ExecProcess {
    /// Spawn `program args`, wiring stdin/stdout/stderr as pipes.
    ///
    /// # Errors
    /// [`Error::Unsupported`] naming `program` when it is not on `PATH` (the
    /// one error this crate expects routinely — the user has not installed
    /// the tool); [`Error::Io`] for any other spawn failure.
    pub fn spawn(program: &str, args: &[String]) -> Result<Self> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Error::Unsupported("vaco-codec-exec: encoder binary not found on PATH")
                } else {
                    Error::Io(e)
                }
            })?;

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let (tx, stdout_rx) = mpsc::channel();
        let stdout_thread = stdout.map(|mut out| {
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 65536].into_boxed_slice();
                loop {
                    match out.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if tx.send(buf.get(..n).unwrap_or(&[]).to_vec()).is_err() {
                                break;
                            }
                        }
                    }
                }
            })
        });

        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        let stderr_thread = stderr.map(|err| {
            let tail = Arc::clone(&stderr_tail);
            std::thread::spawn(move || {
                let reader = BufReader::new(err);
                for line in reader.lines() {
                    let Ok(line) = line else { break };
                    if let Ok(mut tail) = tail.lock() {
                        if tail.len() >= STDERR_TAIL_LINES {
                            tail.pop_front();
                        }
                        tail.push_back(line);
                    }
                }
            })
        });

        Ok(Self {
            child,
            stdin,
            stdout_rx,
            stdout_thread,
            stderr_thread,
            stderr_tail,
        })
    }

    /// Write raw bytes to the child's stdin. Blocks under normal pipe
    /// backpressure, which is safe here — see the module doc.
    ///
    /// # Errors
    /// [`Error::Io`] on a write failure (including a child that already
    /// exited, which surfaces as a broken pipe); [`Error::Unsupported`] if
    /// stdin was already closed by [`ExecProcess::close_stdin`].
    pub fn write_stdin(&mut self, bytes: &[u8]) -> Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or(Error::Unsupported("vaco-codec-exec: stdin already closed"))?;
        stdin.write_all(bytes).map_err(Error::Io)
    }

    /// Signal end of input by closing the child's stdin, without waiting for
    /// it to exit.
    pub fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// Non-blocking: whatever stdout bytes have arrived since the last call.
    #[must_use]
    pub fn try_recv_stdout(&self) -> Vec<u8> {
        let mut out = Vec::new();
        while let Ok(chunk) = self.stdout_rx.try_recv() {
            out.extend_from_slice(&chunk);
        }
        out
    }

    /// Block until either more stdout bytes arrive or the child's stdout
    /// closes (in which case `None`, meaning "no more will ever arrive").
    #[must_use]
    pub fn recv_stdout_blocking(&self) -> Option<Vec<u8>> {
        self.stdout_rx.recv().ok()
    }

    /// Wait for the child to exit and join both reader threads, returning an
    /// error if it exited non-zero (with the captured stderr tail attached).
    ///
    /// # Errors
    /// [`Error::Io`] if the child exited non-zero or could not be waited on.
    pub fn wait(&mut self) -> Result<ExitStatus> {
        self.close_stdin();
        let status = self.child.wait().map_err(Error::Io)?;
        if let Some(t) = self.stdout_thread.take() {
            let _ = t.join();
        }
        if let Some(t) = self.stderr_thread.take() {
            let _ = t.join();
        }
        if status.success() {
            Ok(status)
        } else {
            let tail = self.stderr_tail();
            Err(Error::Io(std::io::Error::other(format!(
                "encoder exited with {status}: {}",
                tail.join(" | ")
            ))))
        }
    }

    /// The last [`STDERR_TAIL_LINES`] lines the child wrote to stderr.
    #[must_use]
    pub fn stderr_tail(&self) -> Vec<String> {
        self.stderr_tail.lock().map(|t| t.iter().cloned().collect()).unwrap_or_default()
    }
}

/// Whether `program` resolves on `PATH` — checked eagerly by
/// [`crate::encoder::ExecEncoder`] so a missing tool fails at construction
/// with a clear message rather than partway through an encode.
#[must_use]
pub fn is_on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            let candidate = dir.join(program);
            candidate.is_file()
        })
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code exercising the crate, not the untrusted-input surface"
)]
mod tests {
    use super::*;

    #[test]
    fn spawning_a_nonexistent_binary_is_unsupported_not_a_panic() {
        let err = ExecProcess::spawn("vaco-codec-exec-definitely-not-a-real-binary", &[]).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }

    #[test]
    fn pipes_a_trivial_echo_round_trip() {
        // `cat` is not a video encoder, but it exercises the exact hazard
        // this module exists for: write more than one pipe buffer's worth
        // while a background thread drains the other side.
        let mut proc = ExecProcess::spawn("cat", &[]).unwrap();
        let chunk = vec![0x5au8; 300_000];
        proc.write_stdin(&chunk).unwrap();
        proc.close_stdin();
        let mut received = Vec::new();
        while let Some(bytes) = proc.recv_stdout_blocking() {
            received.extend_from_slice(&bytes);
        }
        proc.wait().unwrap();
        assert_eq!(received, chunk);
    }

    #[test]
    fn is_on_path_finds_a_real_tool() {
        assert!(is_on_path("cat"));
        assert!(!is_on_path("vaco-codec-exec-definitely-not-a-real-binary"));
    }
}
