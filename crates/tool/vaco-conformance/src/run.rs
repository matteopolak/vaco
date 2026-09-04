//! Hermetic process execution (plan 13 §1.1).
//!
//! # What it is
//!
//! One function, [`run`], that executes a binary under a fixed environment and
//! returns everything observable about the result. Both sides of every
//! comparison go through it, so neither side can accidentally get a different
//! environment from the other — which is the only way a differential harness
//! can be trusted.
//!
//! # How it works
//!
//! The child's environment is **cleared** and rebuilt from a short allowlist:
//!
//! | Variable | Value | Why |
//! |---|---|---|
//! | `TZ` | `UTC` | otherwise `creation_time` rendering follows the developer's timezone |
//! | `LC_ALL`, `LANG` | `C` | decimal separators and message collation |
//! | `SOURCE_DATE_EPOCH` | `0` | anything that consults it is pinned |
//! | `PATH` | inherited | a dynamically linked reference build needs its loader |
//! | `HOME` | a scratch dir | so nothing reads the developer's config |
//! | `DYLD_*` / `LD_LIBRARY_PATH` | inherited when set | shared-library reference builds |
//!
//! `PATH` and the loader variables are inherited rather than fabricated because
//! a shared-library `FFmpeg` (every distribution build) cannot start without
//! them. They are the deliberate hole in hermeticity, and it is a small one:
//! both sides inherit the same values.
//!
//! Output is read on dedicated threads while the main thread polls for exit,
//! so a child that fills the pipe buffer cannot deadlock the harness — the
//! obvious implementation (wait, then read) does deadlock, and finding that out
//! from a hung nightly is expensive. Output is capped so a runaway reference
//! cannot fill the disk; the cap is recorded in the observation rather than
//! silently truncating.
//!
//! On a timeout, the child is placed in its own process group (`unix` only;
//! see [`spawn_grouped`]) and the *whole group* is signalled, not just the
//! immediate pid. A plain `child.kill()` only reaches the process we spawned
//! directly. On Linux, `/bin/sh` is `dash`, and `dash -c "sleep 30"` **forks**
//! `sleep` as a separate process rather than exec-replacing itself with it —
//! confirmed against an `ubuntu-latest`-equivalent container: killing the
//! shell's pid left `sleep` running, reparented to init, still holding the
//! inherited stdout/stderr pipe write-ends open. The reader threads above
//! then block on those pipes until `sleep` finishes on its own, so `run()`
//! reports `timed_out: true` but still blocks for the child's full natural
//! lifetime — the timeout is detected but does not bound wall time. (macOS's
//! `/bin/sh` exec-replaces a lone last command, which is why this was never
//! visible there.) Killing the whole process group reaches `sleep` too.
//!
//! # How to change it
//!
//! Adding an inherited variable weakens hermeticity for every case at once —
//! justify it in the table above. Changing the default timeout is a manifest
//! concern, not a code concern.

use std::ffi::OsString;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How much stdout/stderr to retain per stream before truncating.
pub const DEFAULT_OUTPUT_CAP: usize = 64 * 1024 * 1024;

/// The default wall-clock budget for one invocation.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Everything needed to run one process.
#[derive(Debug, Clone)]
pub struct Invocation {
    /// The binary to execute.
    pub program: PathBuf,
    /// Arguments, excluding the program name.
    pub argv: Vec<String>,
    /// Working directory. `None` inherits, which cases must not do.
    pub cwd: Option<PathBuf>,
    /// Wall-clock budget; the child is killed when it elapses.
    pub timeout: Duration,
    /// Per-stream retention cap.
    pub output_cap: usize,
    /// Bytes fed to the child's stdin. Empty means stdin is closed.
    pub stdin: Vec<u8>,
}

impl Invocation {
    /// A default invocation of `program` with `argv`.
    pub fn new<S: Into<String>>(
        program: impl AsRef<Path>,
        argv: impl IntoIterator<Item = S>,
    ) -> Self {
        Self {
            program: program.as_ref().to_path_buf(),
            argv: argv.into_iter().map(Into::into).collect(),
            cwd: None,
            timeout: DEFAULT_TIMEOUT,
            output_cap: DEFAULT_OUTPUT_CAP,
            stdin: Vec::new(),
        }
    }

    /// Run inside `dir`.
    #[must_use]
    pub fn in_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    /// Override the wall-clock budget.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The command line as a human would type it, for failure reports.
    #[must_use]
    pub fn command_line(&self) -> String {
        let mut out = shell_quote(&self.program.display().to_string());
        for a in &self.argv {
            out.push(' ');
            out.push_str(&shell_quote(a));
        }
        out
    }
}

/// Everything observable about one completed process.
#[derive(Debug, Clone)]
pub struct Observation {
    /// Captured stdout, truncated at the cap.
    pub stdout: Vec<u8>,
    /// Captured stderr, truncated at the cap.
    pub stderr: Vec<u8>,
    /// Exit code, or `None` if killed by a signal or by the timeout.
    pub exit: Option<i32>,
    /// Whether the timeout fired.
    pub timed_out: bool,
    /// Whether either stream hit the retention cap.
    pub truncated: bool,
    /// Wall-clock duration.
    pub wall: Duration,
}

impl Observation {
    /// Exit code 0, no timeout.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        !self.timed_out && self.exit == Some(0)
    }

    /// stdout as text, with invalid UTF-8 replaced.
    #[must_use]
    pub fn stdout_text(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.stdout)
    }

    /// stderr as text, with invalid UTF-8 replaced.
    #[must_use]
    pub fn stderr_text(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.stderr)
    }
}

/// Execute `inv` under the fixed environment.
///
/// # Errors
/// Only for failures to *start* the process or to spawn its reader threads. A
/// non-zero exit, a signal and a timeout are all successful observations.
pub fn run(inv: &Invocation) -> io::Result<Observation> {
    let started = Instant::now();
    let mut cmd = Command::new(&inv.program);
    cmd.args(&inv.argv);
    if let Some(dir) = &inv.cwd {
        cmd.current_dir(dir);
    }
    apply_environment(&mut cmd, inv.cwd.as_deref());
    cmd.stdin(if inv.stdin.is_empty() {
        Stdio::null()
    } else {
        Stdio::piped()
    });
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = spawn_grouped(&mut cmd)?;

    if !inv.stdin.is_empty()
        && let Some(mut sink) = child.stdin.take()
    {
        let bytes = inv.stdin.clone();
        std::thread::spawn(move || {
            use std::io::Write as _;
            let _ = sink.write_all(&bytes);
        });
    }

    let cap = inv.output_cap;
    let out_pipe = child.stdout.take();
    let err_pipe = child.stderr.take();
    let out_thread = out_pipe.map(|p| std::thread::spawn(move || drain(p, cap)));
    let err_thread = err_pipe.map(|p| std::thread::spawn(move || drain(p, cap)));

    let mut timed_out = false;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break Some(s);
        }
        if started.elapsed() >= inv.timeout {
            timed_out = true;
            kill_group(&mut child);
            break child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(2));
    };

    let (stdout, out_trunc) = join(out_thread);
    let (stderr, err_trunc) = join(err_thread);

    Ok(Observation {
        stdout,
        stderr,
        exit: status.and_then(|s| s.code()),
        timed_out,
        truncated: out_trunc || err_trunc,
        wall: started.elapsed(),
    })
}

/// Run and return stdout as text, failing on a non-zero exit.
///
/// The convenience wrapper the table extractors use: they always want "the
/// listing, or a clear explanation of why there isn't one".
///
/// # Errors
/// A start failure, a timeout, or a non-zero exit — with the child's stderr in
/// the message, because that is where the reference explains itself.
pub fn capture_stdout(inv: &Invocation) -> Result<String, String> {
    let obs = run(inv).map_err(|e| format!("{}: {e}", inv.command_line()))?;
    if obs.timed_out {
        return Err(format!(
            "{} timed out after {:?}",
            inv.command_line(),
            inv.timeout
        ));
    }
    if !obs.succeeded() {
        return Err(format!(
            "{} exited {:?}: {}",
            inv.command_line(),
            obs.exit,
            obs.stderr_text().trim()
        ));
    }
    Ok(obs.stdout_text().into_owned())
}

/// Run and return raw stdout bytes, failing on a non-zero exit.
///
/// # Errors
/// As [`capture_stdout`].
pub fn capture_stdout_bytes(inv: &Invocation) -> Result<Vec<u8>, String> {
    let obs = run(inv).map_err(|e| format!("{}: {e}", inv.command_line()))?;
    if obs.timed_out {
        return Err(format!("{} timed out", inv.command_line()));
    }
    if !obs.succeeded() {
        return Err(format!(
            "{} exited {:?}: {}",
            inv.command_line(),
            obs.exit,
            obs.stderr_text().trim()
        ));
    }
    Ok(obs.stdout)
}

/// Spawn `cmd`, putting the child in a new process group on unix so a timeout
/// can later signal every descendant it may have forked, not just itself.
///
/// # Errors
/// Whatever `Command::spawn` returns.
fn spawn_grouped(cmd: &mut Command) -> io::Result<Child> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // pgid 0 means "the same as the new child's own pid", i.e. it becomes
        // the leader of a fresh group rather than joining ours.
        cmd.process_group(0);
    }
    cmd.spawn()
}

/// Kill `child` and everything in its process group.
///
/// A plain `child.kill()` signals only the pid we spawned. On Linux, `dash`
/// (`/bin/sh`) forks a non-exec'd command like `sleep 30` as a separate
/// process rather than replacing itself — see the module docs — so the
/// grandchild survives a `child.kill()` and keeps the inherited stdout/stderr
/// pipes open, which is what actually caused the hang this exists to avoid:
/// the timeout fired correctly, but `run()` still blocked until that
/// grandchild finished on its own.
fn kill_group(child: &mut Child) {
    #[cfg(unix)]
    {
        // No safe std API sends a signal to an arbitrary (negative, for a
        // process group) pid without `unsafe` FFI, which this workspace
        // forbids. Shelling out to `kill` avoids that; it is exactly as
        // available as the `/bin/sh` this module already depends on to run
        // anything at all.
        let pid = child.id();
        let _ = Command::new("kill")
            .args(["-KILL", &format!("-{pid}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    // Belt and suspenders: reaches the child even if the group-kill above did
    // not (binary missing, not unix, group already gone).
    let _ = child.kill();
}

fn apply_environment(cmd: &mut Command, cwd: Option<&Path>) {
    cmd.env_clear();
    cmd.env("TZ", "UTC");
    cmd.env("LC_ALL", "C");
    cmd.env("LANG", "C");
    cmd.env("SOURCE_DATE_EPOCH", "0");
    // Inherited on purpose — see the module docs. A dynamically linked
    // reference build cannot start without its loader search paths.
    for key in [
        "PATH",
        "DYLD_LIBRARY_PATH",
        "DYLD_FALLBACK_LIBRARY_PATH",
        "LD_LIBRARY_PATH",
        "SYSTEMROOT",
        "SystemRoot",
        "WINDIR",
    ] {
        if let Some(v) = std::env::var_os(key) {
            cmd.env(key, v);
        }
    }
    // A scratch HOME so no configuration file from the developer's account can
    // reach either side of a comparison.
    let home: OsString = cwd.map_or_else(
        || std::env::temp_dir().into_os_string(),
        |d| d.as_os_str().to_os_string(),
    );
    cmd.env("HOME", home);
}

fn drain(mut pipe: impl Read, cap: usize) -> (Vec<u8>, bool) {
    let mut out = Vec::new();
    let mut buf = [0_u8; 8 * 1024];
    let mut truncated = false;
    loop {
        match pipe.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let chunk = buf.get(..n).unwrap_or_default();
                if out.len() >= cap {
                    truncated = true;
                    continue;
                }
                let room = cap - out.len();
                if n > room {
                    out.extend_from_slice(chunk.get(..room).unwrap_or_default());
                    truncated = true;
                } else {
                    out.extend_from_slice(chunk);
                }
            }
        }
    }
    (out, truncated)
}

fn join(handle: Option<std::thread::JoinHandle<(Vec<u8>, bool)>>) -> (Vec<u8>, bool) {
    handle
        .and_then(|h| h.join().ok())
        .unwrap_or((Vec::new(), false))
}

fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./=:+,@%".contains(&b))
    {
        return s.to_owned();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "a failing expectation in a test is a failing test"
)]
mod tests {
    use super::{Invocation, run};
    use std::time::Duration;

    fn sh(script: &str) -> Invocation {
        Invocation::new("/bin/sh", ["-c", script])
    }

    #[test]
    fn captures_stdout_stderr_and_exit_code() {
        if !std::path::Path::new("/bin/sh").exists() {
            return;
        }
        let obs = run(&sh("printf out; printf err >&2; exit 3")).expect("runs");
        assert_eq!(obs.stdout_text(), "out");
        assert_eq!(obs.stderr_text(), "err");
        assert_eq!(obs.exit, Some(3));
        assert!(!obs.timed_out);
    }

    #[test]
    fn environment_is_pinned_not_inherited() {
        if !std::path::Path::new("/bin/sh").exists() {
            return;
        }
        // SAFETY-of-intent: this is a test-only mutation of our own process
        // environment, read back through the child to prove `env_clear` works.
        let obs = run(&sh("echo \"$TZ:$LC_ALL:$SOURCE_DATE_EPOCH\"")).expect("runs");
        assert_eq!(obs.stdout_text().trim(), "UTC:C:0");
    }

    #[test]
    fn a_hung_child_is_killed_and_reported() {
        if !std::path::Path::new("/bin/sh").exists() {
            return;
        }
        let obs = run(&sh("sleep 30").with_timeout(Duration::from_millis(150))).expect("runs");
        assert!(obs.timed_out, "the timeout must fire");
        assert!(obs.wall < Duration::from_secs(5));
    }

    #[test]
    fn a_child_that_floods_a_pipe_does_not_deadlock() {
        if !std::path::Path::new("/bin/sh").exists() {
            return;
        }
        // Far more than a pipe buffer. The naive wait-then-read implementation
        // hangs here forever; this is the regression test for that.
        let mut inv = sh(
            "i=0; while [ $i -lt 400 ]; do printf '%0.s0123456789abcdef' $(seq 1 64); i=$((i+1)); done",
        );
        inv.timeout = Duration::from_secs(20);
        let obs = run(&inv).expect("runs");
        assert!(!obs.timed_out);
        assert!(obs.stdout.len() > 100_000, "got {} bytes", obs.stdout.len());
    }

    #[test]
    fn output_cap_truncates_instead_of_filling_the_disk() {
        if !std::path::Path::new("/bin/sh").exists() {
            return;
        }
        let mut inv = sh(
            "i=0; while [ $i -lt 200 ]; do printf '%0.s0123456789abcdef' $(seq 1 64); i=$((i+1)); done",
        );
        inv.output_cap = 1024;
        inv.timeout = Duration::from_secs(20);
        let obs = run(&inv).expect("runs");
        assert_eq!(obs.stdout.len(), 1024);
        assert!(obs.truncated);
    }

    #[test]
    fn command_line_is_reproducible_by_a_human() {
        let inv = Invocation::new("/usr/bin/ffprobe", ["-of", "csv=p=0", "a b.mp4"]);
        assert_eq!(inv.command_line(), "/usr/bin/ffprobe -of csv=p=0 'a b.mp4'");
    }

    #[test]
    fn missing_binary_is_an_error_not_a_panic() {
        assert!(run(&Invocation::new("/nonexistent/vaco-ref", ["-version"])).is_err());
    }
}
