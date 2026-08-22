//! The whole command line, against an arbitrary argument vector.
//!
//! This is the mandatory target for the crate (D6): `argv` is untrusted input,
//! and the workspace denies `unwrap`, `expect`, `panic` and `indexing_slicing`
//! precisely so that a hostile command line cannot reach a panic.
//!
//! The input bytes are split on NUL into argv entries, so the fuzzer controls
//! the entry count as well as the contents — and on Unix the entries carry
//! **arbitrary bytes**, which is the case real filenames actually exercise and
//! that a `Vec<String>` generator would never reach.
//!
//! Beyond totality, the structural invariants of the scope model are asserted:
//! a hoisted option really is global, a grouped one really is not, group indices
//! are dense per kind, and nothing is invented.
//! fuzz-crate: vaco-cli-core

#![no_main]
use std::ffi::OsString;

use libfuzzer_sys::fuzz_target;
use vaco_cli_core::{GroupKind, OptFlags, OptTable, ffmpeg, ffprobe, split};

fn to_argv(data: &[u8]) -> Vec<OsString> {
    data.split(|b| *b == 0).map(to_os).collect()
}

#[cfg(unix)]
fn to_os(bytes: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStringExt;
    // `from_vec` is a safe function: on Unix an `OsString` *is* a byte string.
    OsString::from_vec(bytes.to_vec())
}

#[cfg(not(unix))]
fn to_os(bytes: &[u8]) -> OsString {
    OsString::from(String::from_utf8_lossy(bytes).into_owned())
}

fn check(table: &OptTable, argv: &[OsString]) {
    let Ok(cl) = split(table, argv) else {
        return;
    };

    for o in &cl.global {
        let d = o
            .desc
            .unwrap_or_else(|| panic!("a deferred option was hoisted as global: {:?}", o.name));
        assert!(
            d.flags.contains(OptFlags::GLOBAL),
            "{:?} was hoisted but is not global",
            o.name
        );
    }

    for o in cl.groups.iter().flat_map(|g| &g.opts).chain(&cl.orphaned) {
        if let Some(d) = o.desc {
            assert!(
                !d.flags.contains(OptFlags::GLOBAL),
                "{:?} is global but was bound to a file",
                o.name
            );
        }
    }

    for kind in [GroupKind::Input, GroupKind::Output] {
        for (want, g) in cl.of_kind(kind).enumerate() {
            assert_eq!(
                usize::try_from(g.index).unwrap_or(usize::MAX),
                want,
                "group indices are not dense within {kind:?}"
            );
        }
    }

    // Every option and every URL came from a distinct argv entry, and every
    // valued option consumed one more. Nothing may be invented.
    let opts = cl.global.len()
        + cl.orphaned.len()
        + cl.groups.iter().map(|g| g.opts.len()).sum::<usize>();
    assert!(
        opts + cl.groups.len() <= argv.len(),
        "split produced more items than there were arguments"
    );

    // Validation and specifier resolution must be total too: they run on
    // whatever survived splitting, which is still attacker-shaped.
    let _ = cl.validate();
    for o in cl
        .global
        .iter()
        .chain(cl.groups.iter().flat_map(|g| &g.opts))
        .chain(&cl.orphaned)
    {
        let _ = o.stream_spec();
        let _ = o.metadata_spec();
        let _ = o.resolved();
        let _ = o.value_str("test");
    }
}

fuzz_target!(|data: &[u8]| {
    // Bound the work: libFuzzer inputs are already small, but an argv of a
    // hundred thousand empty entries teaches us nothing and slows the corpus.
    if data.len() > 4096 {
        return;
    }
    let argv = to_argv(data);
    check(&ffmpeg(), &argv);
    check(&ffprobe(), &argv);
});
