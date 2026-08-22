//! Placeholder so the declared `[[bench]]` target resolves.
//!
//! A `[[bench]]` entry with no corresponding file fails MANIFEST PARSING, which
//! breaks every `cargo` command for the ENTIRE workspace — not just this crate.
//! With many agents building concurrently in one tree that halts everyone, so
//! this file exists to keep the tree buildable until the real benchmark lands.
//!
//! `harness = false`, so this needs its own `main`.

fn main() {
    println!("vaco-tx benchmarks not yet implemented");
}
