//! The `ffprobe`-equivalent binary.
//!
//! Deliberately thin. Everything is in the library so that the whole program —
//! argument parsing, probing, section emission, byte output — is reachable from
//! a test and from a fuzz target without a process. `main` owns exactly three
//! things the library must not: the real argv, the real stdio, and the exit
//! code.

#![forbid(unsafe_code)]

use std::io::Write as _;
use std::process::ExitCode;

fn main() -> ExitCode {
    let argv: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();

    // Locking once rather than per write: `println!` re-locks each call, which
    // both costs time on a `-show_packets` run and lets another thread
    // interleave. Neither is acceptable when the output is compared byte for
    // byte.
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut err = stderr.lock();

    let exit = vaco_probe::run(&argv, &mut out, &mut err);

    // A broken pipe on flush is how `vaco-probe … | head` ends, and it is not
    // an error worth a message; anything else is reported so the user is not
    // left with silently truncated output.
    if let Err(e) = out.flush()
        && e.kind() != std::io::ErrorKind::BrokenPipe
    {
        let _ = writeln!(err, "{e}");
        return ExitCode::FAILURE;
    }

    match exit {
        vaco_probe::Exit::Ok => ExitCode::SUCCESS,
        vaco_probe::Exit::Failure => ExitCode::FAILURE,
    }
}
