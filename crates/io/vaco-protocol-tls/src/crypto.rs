//! The one `rustls` crypto provider this workspace builds, shared by this
//! crate and `vaco-protocol-http`.
//!
//! # Why a shared function rather than two independent constructions
//!
//! D11 requires exactly one Vaco crate to declare `rustls` in its
//! `Cargo.toml`; that crate is this one (see the crate docs' "Who owns
//! `rustls`" section). `vaco-protocol-http` still needs a crypto provider —
//! `ureq`'s `TlsConfig::builder().unversioned_rustls_crypto_provider(...)`
//! wants an `Arc<rustls::crypto::CryptoProvider>` — so it gets one from here
//! rather than declaring the dependency itself.
//!
//! Building the provider once per process (behind a [`std::sync::OnceLock`],
//! same as `vaco-protocol-http`'s own `transport::agent()` did before this
//! crate existed) also means this crate's own `connect::handshake` and
//! `vaco-protocol-http`'s `Agent` end up backed by the *same* provider
//! instance in a process that uses both, rather than two independently
//! initialised ones with no behavioural difference but a wasted setup cost.

use std::sync::{Arc, OnceLock};

use rustls::crypto::CryptoProvider;

/// The process-wide `rustls` crypto provider.
///
/// # Why `ring`, not `rustls-rustcrypto`
///
/// D14.2 originally chose `rustls-rustcrypto` because it was the only
/// zero-FFI `rustls` provider (`ring` and `aws-lc-rs`, rustls's two
/// production providers, both vendor and compile C/assembly and failed D10
/// Gate 1 at the time). That provider turned out to be pinned at
/// `0.0.2-alpha` (published 2024-04-24, no release since) and to hard-require
/// dependency versions carrying seven RUSTSEC advisories that could not be
/// patched without a new release of it — failing Gate 3's "alive" and
/// "sound" criteria outright.
///
/// TLS carries no media semantics, so this crate may use FFI where codec and
/// container crates may not. `ring` is the replacement — see
/// `docs/dependencies.md` for the full Gate 2/3 record and why it was chosen
/// over `aws-lc-rs` (both were
/// viable; `ring`'s only extra build machinery is `cc`, where `aws-lc-rs`
/// needs `cc`+`cmake`+`pkg-config`+optionally `bindgen` for the identical
/// job).
///
/// This function goes through `rustls::crypto::ring::default_provider()`
/// rather than depending on the `ring` crate directly: `ring` is pulled in as
/// `rustls`'s own optional dependency via its `ring` Cargo feature (enabled
/// on the workspace `rustls` dependency), so this crate never needs to `use
/// ring::...` itself. `cargo xtask dep-gate` still sees `ring` in the
/// resolved build graph either way (it walks the graph, not manifests) and
/// checks it is reachable only through this crate.
#[must_use]
pub fn shared_provider() -> Arc<CryptoProvider> {
    static PROVIDER: OnceLock<Arc<CryptoProvider>> = OnceLock::new();
    PROVIDER
        .get_or_init(|| Arc::new(rustls::crypto::ring::default_provider()))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_stable_across_calls() {
        // Same `Arc` allocation, not merely an equal one: two independent
        // providers would each carry their own RNG/algorithm-table state,
        // which is harmless but wasteful to build twice per process.
        assert!(Arc::ptr_eq(&shared_provider(), &shared_provider()));
    }
}
