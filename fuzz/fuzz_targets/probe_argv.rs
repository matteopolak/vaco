//! The whole of `vaco-probe`, driven from an arbitrary argument vector.
//!
//! `argv` is untrusted in the plainest sense (D6), and this binary reaches an
//! unusual amount of code from it: the option table, the stream-specifier
//! grammar, `-show_entries`, every writer's option parser, the protocol layer,
//! the probe engine, and the section emitters. One target covers all of it
//! because they are only reachable in combination — an `-of` spec is not parsed
//! until the run gets that far, and a `-show_entries` filter only matters once
//! sections are being emitted.
//!
//! Three properties beyond "does not panic":
//!
//! * **Both sinks always stay writable.** `vaco_probe::run` takes them by
//!   `&mut`, and a run that leaves output half-written is how a byte
//!   divergence hides.
//! * **The exit code is one of two values.** Not interesting on its own; it
//!   proves the run reached its end rather than unwinding somewhere.
//! * **Determinism.** The same argv twice must produce the same bytes. Output
//!   that depends on anything but the input is a D6 failure by definition, and
//!   iteration order over a map is the classic way to acquire one.
//!
//! Input is decoded as NUL-separated argv rather than through `arbitrary`, so
//! that a corpus entry is a readable command line and a crash reproducer can be
//! run by hand.
//! fuzz-crate: vaco-probe

#![no_main]

use libfuzzer_sys::fuzz_target;

/// Cap the argument count so the fuzzer spends its time on option *values*
/// rather than on rediscovering that ten thousand flags is slow.
const MAX_ARGS: usize = 24;

/// Cap each argument, for the same reason.
const MAX_ARG_LEN: usize = 512;

fuzz_target!(|data: &[u8]| {
    let mut argv: Vec<std::ffi::OsString> = Vec::new();
    for field in data.split(|b| *b == 0).take(MAX_ARGS) {
        let field = field.get(..MAX_ARG_LEN.min(field.len())).unwrap_or_default();
        // Lossy rather than skipping non-UTF-8: the option lexer has a
        // dedicated non-UTF-8 path (`CliError::NonUtf8OptionName`) and skipping
        // would make it unreachable. Real argv on Unix is bytes, so this is the
        // honest shape.
        argv.push(std::ffi::OsString::from(
            String::from_utf8_lossy(field).into_owned(),
        ));
    }

    let (mut out, mut err) = (Vec::new(), Vec::new());
    let first = vaco_probe::run(&argv, &mut out, &mut err);
    assert!(matches!(
        first,
        vaco_probe::Exit::Ok | vaco_probe::Exit::Failure
    ));

    let (mut out2, mut err2) = (Vec::new(), Vec::new());
    let second = vaco_probe::run(&argv, &mut out2, &mut err2);
    assert_eq!(first, second, "exit code is not deterministic");
    assert_eq!(out, out2, "stdout is not deterministic");
    assert_eq!(err, err2, "stderr is not deterministic");
});
