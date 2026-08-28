#![forbid(unsafe_code)]
//! `vaco-corpus` — the `just corpus-fetch` / `just corpus-verify` CLI.
//!
//! Deliberately thin: `mutate`/`minimise` are exposed as a library API
//! ([`vaco_corpus::mutate`]) for now, consumed programmatically by fuzz
//! tooling rather than from argv — a CLI surface for them is a named cut,
//! not an oversight, since there is no established fuzz-triage workflow
//! calling into this crate yet for it to serve.

use std::process::ExitCode;

use vaco_corpus::fetch::{self, NetworkPolicy};
use vaco_corpus::{Store, embedded_catalogue};

fn usage() -> &'static str {
    "usage:\n  \
     vaco-corpus list\n  \
     vaco-corpus fetch <name> [--cache-dir <dir>]\n  \
     vaco-corpus verify <name> [--cache-dir <dir>]\n\n\
     Network access is opt-in: set VACO_CORPUS_NETWORK=1 to allow a fetch on \
     a cache miss. Without it, `fetch`/`verify` only succeed against an \
     already-cached object."
}

fn store_from_args(args: &[String]) -> Store {
    let dir = args
        .iter()
        .position(|a| a == "--cache-dir")
        .and_then(|i| args.get(i + 1))
        .cloned();
    match dir {
        Some(d) => Store::at(d),
        None => Store::open_default(),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        eprintln!("{}", usage());
        return ExitCode::FAILURE;
    };

    match cmd.as_str() {
        "list" => {
            let lock = embedded_catalogue();
            for entry in &lock.entries {
                let status = if entry.is_fetchable() { "fetchable" } else { "gap" };
                println!("{:<32} {:<20} {status}", entry.name, entry.suite);
            }
            ExitCode::SUCCESS
        }
        "fetch" | "verify" => {
            let Some(name) = args.get(1) else {
                eprintln!("{}", usage());
                return ExitCode::FAILURE;
            };
            let lock = embedded_catalogue();
            let Some(entry) = lock.find(name) else {
                eprintln!("vaco-corpus: no entry named {name:?}");
                return ExitCode::FAILURE;
            };
            let store = store_from_args(&args);
            let policy = if cmd == "verify" {
                NetworkPolicy::CacheOnly
            } else {
                NetworkPolicy::from_env()
            };
            match fetch::fetch(entry, &store, policy) {
                Ok(bytes) => {
                    println!("{name}: {} bytes, verified", bytes.len());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{name}: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!("{}", usage());
            ExitCode::FAILURE
        }
    }
}
