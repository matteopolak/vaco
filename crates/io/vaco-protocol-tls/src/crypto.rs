//! The one `rustls-rustcrypto` provider this workspace builds, shared by this
//! crate and `vaco-protocol-http`.
//!
//! # Why a shared function rather than two independent constructions
//!
//! D11 requires exactly one Vaco crate to declare `rustls`/`rustls-rustcrypto`
//! in its `Cargo.toml`; that crate is this one (see the crate docs' "Who owns
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

/// The process-wide `rustls-rustcrypto` provider.
///
/// D14.2's provider decision, in one function: `rustls-rustcrypto` is the
/// only pure-Rust `rustls` crypto provider available with zero FFI (`ring`
/// and `aws-lc-rs`, rustls's two production providers, both vendor and
/// compile C/assembly and fail Gate 1 of D10 outright). See this crate's own
/// docs, and `docs/io/vaco-protocol-http.md`'s "Dependencies" table, for the
/// full gate-by-gate record — kept there rather than duplicated here, since
/// it was written before this crate existed and moving the *declaration*
/// should not orphan the analysis that justified it.
#[must_use]
pub fn shared_provider() -> Arc<CryptoProvider> {
    static PROVIDER: OnceLock<Arc<CryptoProvider>> = OnceLock::new();
    PROVIDER
        .get_or_init(|| Arc::new(rustls_rustcrypto::provider()))
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
