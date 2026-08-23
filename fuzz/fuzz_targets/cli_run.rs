//! A whole `vaco` invocation, plus stream selection over synthetic inputs.
//!
//! `cli_argv` already fuzzes the *splitter* — the lexer, the scope model and the
//! grouping pass, all in `vaco-cli-core`. This target deliberately does not
//! repeat that. It covers the layer above it, which is where `vaco-cli`'s own
//! decisions live:
//!
//! * **binding** — turning a split command line into inputs and outputs,
//!   including the non-UTF-8 and missing-value paths;
//! * **selection** — `-map`, the negative and optional forms, the automatic
//!   rules, and the `dropped` flag, over stream sets the fuzzer invents;
//! * **spec construction** — muxer resolution and the codec check, which is as
//!   far as an invocation gets before it needs a real file.
//!
//! Two halves, because each reaches something the other cannot. The full run
//! covers the plumbing but never sees a stream, since every URL it is given
//! fails to open; the direct call to [`select::resolve`] sees arbitrary stream
//! sets but no argv.
//!
//! Invariants asserted, beyond "does not panic":
//!
//! * selection is a **function**: the same inputs twice give the same answer;
//! * every pick names a stream that exists;
//! * `dropped` implies nothing was picked;
//! * a failure carries a non-empty message and a non-zero exit status, and a
//!   success carries a zero one — a diagnosis that exits 0 would be invisible.
//! fuzz-crate: vaco-cli

#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;
use vaco_cli::select::{self, Suppressed, InputStreams, MapEntry};

/// Option tokens worth spending fuzzer bytes on: the ones this crate binds, the
/// ones that open a file, and a few that are only interesting because they are
/// on the wrong side or take no value.
const TOKENS: &[&str] = &[
    "-i", "-f", "-map", "-c", "-c:v", "-c:a", "-codec", "-vn", "-an", "-sn", "-dn", "-y", "-n",
    "-hide_banner", "-version", "-formats", "-muxers", "-h", "-loglevel", "-protocol_whitelist",
    "-probesize", "-nostats", "--", "-", "copy", "null", "0", "0:v", "0:a", "-0:v", "0:v:0?",
    "[x]", "matroska", "nosuchformat", "out.mkv", "out.zzz", "/nonexistent/vaco-fuzz-input",
    "pipe:0", "", ":", "?",
    // CL-04: `-h`'s topic grammar takes an arbitrary string, including one
    // that looks like another option (`-h -i` swallows `-i` rather than
    // re-lexing it) — see `vaco_cli_core::help::parse_topic` and
    // `vaco_cli::help::render`. "long"/"full" exercise the two depths,
    // "decoder=", "protocol=", "demuxer=", "muxer=", "filter=", "bsf=" (with
    // no name after the `=`, which is a distinct case from no `=` at all)
    // exercise all seven kinds and the found/not-found paths.
    "long", "full", "decoder=h264", "encoder=x", "demuxer=matroska", "demuxer=",
    "muxer=matroska", "protocol=file", "protocol=", "filter=scale", "bsf=x",
    "-buildconf", "-codecs", "-protocols", "-bsfs", "-dispositions",
];

fn argv_from(u: &mut Unstructured<'_>) -> Vec<String> {
    let n = u.int_in_range(0..=12).unwrap_or(0);
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        // Mostly vocabulary, sometimes raw bytes: the vocabulary reaches deep
        // paths quickly, the raw bytes reach the ones a grammar would never
        // guess.
        let raw = u.ratio(1, 5).unwrap_or(false);
        if raw {
            let s: String = String::arbitrary(u).unwrap_or_default();
            out.push(s);
        } else {
            let i = u.choose_index(TOKENS.len()).unwrap_or(0);
            out.push(TOKENS.get(i).copied().unwrap_or("-").to_owned());
        }
    }
    out
}

fn streams_from(u: &mut Unstructured<'_>) -> Vec<InputStreams> {
    let files = u.int_in_range(0..=3).unwrap_or(0);
    let mut out = Vec::with_capacity(files);
    for _ in 0..files {
        let mut f = InputStreams::default();
        let n = u.int_in_range(0..=6).unwrap_or(0);
        for _ in 0..n {
            f.push_described(
                u.int_in_range(0..=5).unwrap_or(0),
                u.int_in_range(0..=8192).unwrap_or(0),
                u.int_in_range(0..=8192).unwrap_or(0),
                u.int_in_range(0..=64).unwrap_or(0),
                u32::arbitrary(u).unwrap_or(0),
            );
        }
        out.push(f);
    }
    out
}

fn maps_from(u: &mut Unstructured<'_>) -> Vec<MapEntry> {
    let n = u.int_in_range(0..=5).unwrap_or(0);
    let mut out = Vec::new();
    for _ in 0..n {
        let text = if u.ratio(2, 3).unwrap_or(true) {
            let i = u.choose_index(TOKENS.len()).unwrap_or(0);
            TOKENS.get(i).copied().unwrap_or("0").to_owned()
        } else {
            String::arbitrary(u).unwrap_or_default()
        };
        // A value that does not parse is a legitimate outcome, not a case to
        // skip: it is the path `-map` failures take.
        if let Ok(m) = MapEntry::parse(&text) {
            out.push(m);
        }
    }
    out
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);

    // ---------------------------------------------------------- the whole run
    let argv = argv_from(&mut u);
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = vaco_cli::run(&argv, &mut out, &mut err);
    if !code.is_ok() {
        assert!(
            !out.is_empty() || !err.is_empty(),
            "a failing run said nothing: {argv:?}"
        );
    }

    // Running it again must give the same status: nothing here may depend on
    // hidden state, and a listing that printed once and not twice would be a
    // real defect.
    let mut out2 = Vec::new();
    let mut err2 = Vec::new();
    let code2 = vaco_cli::run(&argv, &mut out2, &mut err2);
    assert_eq!(code, code2, "{argv:?}");
    assert_eq!(out, out2, "{argv:?}");

    // ------------------------------------------------------------- selection
    let files = streams_from(&mut u);
    let maps = maps_from(&mut u);
    let blocked = Suppressed {
        video: u.ratio(1, 4).unwrap_or(false),
        audio: u.ratio(1, 4).unwrap_or(false),
        subtitle: u.ratio(1, 4).unwrap_or(false),
        data: u.ratio(1, 4).unwrap_or(false),
    };
    let all = |_| true;
    let first = select::resolve(&files, &maps, blocked, &all);
    let again = select::resolve(&files, &maps, blocked, &all);

    match (first, again) {
        (Ok(a), Ok(b)) => {
            assert_eq!(a.picks, b.picks, "selection is not a function");
            assert_eq!(a.dropped, b.dropped);
            if a.dropped {
                assert!(a.picks.is_empty(), "a dropped output selected streams");
            }
            for p in &a.picks {
                let file = files
                    .get(p.file as usize)
                    .unwrap_or_else(|| panic!("pick names file {}", p.file));
                assert!(
                    file.streams.iter().any(|s| s.index == p.stream),
                    "pick names stream {} that does not exist",
                    p.stream
                );
            }
        }
        (Err(a), Err(b)) => {
            assert_eq!(a.exit, b.exit);
            assert!(!a.lines.is_empty(), "an error with no message");
            assert!(!a.exit.is_ok(), "a failure that exits 0 is invisible");
        }
        (a, b) => panic!("selection disagreed with itself: {a:?} vs {b:?}"),
    }
});
