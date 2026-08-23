//! The `vaco` binary: argv, stdio and an exit code. Everything else is the
//! library, so that a test and a fuzz target can run a whole invocation
//! in-process.

#![forbid(unsafe_code)]

use std::io::Write;

fn main() {
    let argv: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let mut err = stderr.lock();
    let code = vaco_cli::run(&argv, &mut out, &mut err);
    // Flush before exiting: `std::process::exit` does not run destructors, so a
    // buffered writer's contents would be lost — a silent truncation of the
    // program's entire output.
    let _ = out.flush();
    let _ = err.flush();
    std::process::exit(code.code());
}
