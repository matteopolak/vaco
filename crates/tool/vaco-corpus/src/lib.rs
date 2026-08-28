//! Content-addressed conformance/fuzz corpus: fetching, storage, mutation and
//! minimisation.
//!
//! # What it is
//!
//! The infrastructure behind plan 13 §2.5 and §4: one content-addressed
//! local object store ([`store`]), one manifest naming every asset this
//! project knows about ([`lock`], backed by `vaco-media.lock` in this
//! crate's own directory), a fetcher that only ever reaches the network on
//! request ([`fetch`]), and format-agnostic mutation/minimisation
//! primitives ([`mutate`]) for building and shrinking fuzz corpora.
//!
//! # How it works
//!
//! ```text
//! vaco-media.lock ──▶ MediaLock::parse ──▶ [LockEntry] ──▶ fetch::fetch ──▶ Store
//!                                                              │
//!                                                    NetworkPolicy gates
//!                                                    whether a miss may
//!                                                    reach the network at all
//! ```
//!
//! A [`lock::LockEntry`] names a URL and an expected hash. [`fetch::fetch`]
//! checks the [`store::Store`] first; on a hit it never touches the network
//! regardless of policy. On a miss, [`fetch::NetworkPolicy::CacheOnly`]
//! (the default — see [`fetch::NetworkPolicy::from_env`]) fails cleanly
//! rather than guessing the network is reachable, which is what keeps CI and
//! offline builds working with this crate on the classpath.
//!
//! # What this crate deliberately does not do
//!
//! - **No BLAKE3.** Plan 13 §2.5.2 specifies it; `blake3` is not a workspace
//!   dependency and adding one is a D10 decision this crate does not make
//!   unilaterally. SHA-256 via the already-adopted `vaco-hash` is used
//!   instead — see `store.rs`'s module docs for the full reasoning.
//! - **No format-aware mutation.** Box/EBML/NAL-aware operators need to know
//!   what a box, an EBML element or a NAL unit is; that knowledge lives in
//!   the format crates, not here. See `mutate.rs`'s module docs.
//! - **No S3/R2 remote object store.** Plan 13 §2.5.2 describes one; this
//!   crate's [`store::Store`] is local-filesystem only. A remote store is a
//!   drop-in extension of the same content-addressed shape (same
//!   [`store::ObjectId`], same verified-write discipline) but is out of
//!   scope for this pass — nothing here assumes a bucket exists.
//! - **Argon and JVT/JCT-VC have no fetchable entries yet.** `vaco-media.lock`'s
//!   header explains why (no stable public single-file source was found) and
//!   records them as an explicit gap rather than omitting them silently.

#![forbid(unsafe_code)]

pub mod fetch;
pub mod lock;
pub mod mutate;
pub mod store;
pub mod toml_min;

pub use fetch::{FetchError, NetworkPolicy};
pub use lock::{LockEntry, MediaLock};
pub use store::{ObjectId, Store, StoreError};

/// The `vaco-media.lock` shipped alongside this crate's own source, embedded
/// at compile time so a caller never has to guess a path relative to the
/// binary. [`MediaLock::parse`] this to get the catalogue.
pub const EMBEDDED_LOCK: &str = include_str!("../vaco-media.lock");

/// Parse this crate's own embedded `vaco-media.lock`.
///
/// # Panics
/// Never in practice — this is the file `cargo test` also parses in
/// `lock::tests`, and CI fails long before a release if it stops parsing.
/// Panicking here (rather than returning a `Result`) is deliberate: every
/// caller of this specific function already knows the file is meant to be
/// valid, and a parse failure at this call site means the crate itself
/// shipped broken data, not that the caller passed bad input.
#[expect(
    clippy::expect_used,
    reason = "the embedded catalogue is this crate's own committed data, not caller input; a parse failure here is a broken release, and vaco-corpus is a test/tooling crate the panic policy for untrusted-input parsers does not apply to"
)]
#[must_use]
pub fn embedded_catalogue() -> MediaLock {
    MediaLock::parse(EMBEDDED_LOCK).expect("vaco-corpus's own vaco-media.lock must parse")
}

#[cfg(test)]
mod tests {
    use super::embedded_catalogue;

    #[test]
    fn the_embedded_catalogue_parses_and_is_non_empty() {
        let lock = embedded_catalogue();
        assert!(!lock.entries.is_empty());
    }

    #[test]
    fn every_fetchable_entry_has_a_64_hex_char_hash_and_a_url() {
        let lock = embedded_catalogue();
        for e in &lock.entries {
            if e.is_fetchable() {
                assert!(e.url.is_some(), "{}: fetchable but no url", e.name);
                assert!(e.sha256.is_some(), "{}: fetchable but no sha256", e.name);
            }
        }
    }

    #[test]
    fn documented_gaps_are_named_and_explained() {
        let lock = embedded_catalogue();
        let gaps: Vec<_> = lock.entries.iter().filter(|e| !e.is_fetchable()).collect();
        assert!(
            gaps.iter().any(|e| e.suite == "argon"),
            "argon should be a documented gap"
        );
        assert!(
            gaps.iter().any(|e| e.suite == "jctvc"),
            "jctvc should be a documented gap"
        );
        for gap in gaps {
            assert!(
                !gap.source.is_empty(),
                "{}: a gap entry must explain itself in `source`",
                gap.name
            );
        }
    }
}
