//! The `tls:` protocol: a raw TLS connection over TCP, and the crate that
//! owns `rustls`/`ring`/`webpki-roots` on behalf of this workspace (D11 — see
//! "Dependencies" below for why `vaco-protocol-http` does not declare them
//! itself).
//!
//! # What it is
//!
//! A [`vaco_protocol_core::Protocol`] implementation of `tls:` — the scheme a
//! demuxer opens when it needs a TLS-secured byte stream that is *not*
//! HTTP(S) (RTMPS, or any other protocol that layers its own framing directly
//! over TLS). `https:` (`vaco-protocol-http`) does **not** route through this
//! crate's registration — see [`connect`]'s module docs for why a registered
//! `Protocol` cannot serve that purpose at all — but it does share this
//! crate's crypto-provider construction, which is the actual `rustls`
//! wiring D14.2 made a decision about.
//!
//! # How it works
//!
//! | Module | Job |
//! |---|---|
//! | [`options`] | `TlsOptions` — `-h protocol=tls`'s surface, the parts this crate implements. |
//! | [`crypto`] | [`crypto::shared_provider`] — the one `ring`-backed `Arc<CryptoProvider>` this crate and `vaco-protocol-http` both build TLS configuration from. |
//! | [`pem`] | A small, permissive (whitespace-tolerant) PEM block extractor and base64 decoder, written here rather than adopted — see the module docs. |
//! | [`roots`] | The default root store (`webpki-roots`), optionally extended with a caller-supplied `ca_file`/`ca_pem`. |
//! | [`verify`] | Certificate verification policy: standard `WebPkiServerVerifier` when `-verify`/`-tls_verify` is set, and — matching the reference's own **measured default** — a permissive verifier otherwise that still cryptographically checks the handshake signature but does not check the certificate's trust chain or hostname. |
//! | [`connect`] | Resolves and connects the underlying TCP socket directly (not through `vaco-protocol-socket`), applies the whitelist check by hand, and drives the `rustls` handshake. |
//! | [`protocol`] | [`protocol::TlsProtocol`]: the `Protocol::open`/`create` entry points. |
//!
//! # Security: the whitelist, and a default that will surprise you
//!
//! **`tls:` grants nothing by default, and needs `tcp` granted to it
//! explicitly** — measured directly against the reference (`ffmpeg 8.1`,
//! D17), not assumed:
//!
//! ```text
//! $ ffmpeg -protocol_whitelist tls -listen_timeout 1000 -i tls://127.0.0.1:<port> ...
//! [tcp @ ...] Protocol 'tcp' not on whitelist 'tls'!
//!
//! $ ffmpeg -protocol_whitelist tls,tcp -listen_timeout 1000 -i tls://127.0.0.1:<port> ...
//! [tls @ ...] IO error: Connection reset by peer   # got past the gate; the peer wasn't speaking TLS
//! ```
//!
//! `ffmpeg -v debug`'s own log confirms this is not merely "unmeasured
//! default" but a genuine absence: `[tls @ ...] No default whitelist set`.
//! So `TLS_PROTOCOL.default_whitelist` is `&[]`, matching every other
//! protocol in this project that opens a nested URL of its own (`cache:`,
//! `concat:`, `subfile:` — see `vaco-protocol-wrap`'s crate docs for the
//! general shape of this rule: **the curated default grant `hls:` has is the
//! exception, not the rule**, because a generic transport does not know what
//! kind of thing it is about to open the way a playlist parser does).
//!
//! **`-tls_verify`/`-verify` default to `false` in the reference** — measured
//! via `ffmpeg -h protocol=tls`, not memory: `-tls_verify <boolean> ED... Verify
//! the peer certificate (default false)`. This crate matches that default
//! deliberately (see [`verify`]'s module docs for the exact, still
//! cryptographically-bound behaviour that gives you, and why "match the
//! measured default" is the right call here rather than silently hardening
//! it) — **a caller that wants real certificate validation must pass
//! `-verify 1` or `-tls_verify 1` explicitly**, exactly as it must with the
//! reference.
//!
//! # Why `tls:` does not nest through `vaco-protocol-socket`'s `tcp:`
//!
//! See [`connect`]'s module docs for the full argument. In one sentence:
//! [`vaco_protocol_core::Protocol::open`] returns a read-only
//! `Box<dyn MediaSource>`, a TLS handshake needs both directions on one
//! connection, so this crate connects its own [`std::net::TcpStream`] and
//! calls [`vaco_protocol_core::ProtocolEnv::check_scheme`] with `"tcp"` by
//! hand — preserving the measured whitelist property above without being
//! able to express the nested open through the registry.
//!
//! # What is deliberately not implemented
//!
//! Client certificate authentication (`-cert_pem`/`-key_pem`/`-cert_file`/
//! `-cert`/`-key_file`/`-key`) — a private key parser (PKCS#8/RSA DER) is
//! substantial enough to deserve its own review, and nothing in this
//! project's v0.1 scope (D5: zero muxers, no server-mode demuxer) calls for
//! one yet. `-listen` (TLS server mode) — same blocker, plus a server needs a
//! certificate to present, which is the same missing piece. `-http_proxy`
//! (matching `vaco-protocol-http`'s own scoping), `-external_sock` (there is
//! no already-connected-fd handoff in this project's `IoFlags`/`ProtocolEnv`
//! model to receive one through), and `-use_srtp`/`-mtu` (DTLS-specific; this
//! crate is TLS-over-TCP only — no `dtls:` registration exists here).
//!
//! # Configuration
//!
//! [`options::TlsOptions`] — `verify`/`tls_verify` (alias), `ca_file`/`cafile`
//! (alias), `verifyhost`. See the crate docs above and [`verify`]'s module
//! docs for what `verify=false`'s default actually does.
//!
//! # Dependencies (D11 — who owns `rustls`)
//!
//! This crate declares `rustls` and `webpki-roots` directly (`ring` arrives
//! transitively, via `rustls`'s own `ring` Cargo feature — see [`crypto`]'s
//! module docs for why that is still exactly one owner); **`vaco-protocol-http`
//! does not** (its own `Cargo.toml` depends on this crate instead, for
//! [`crypto::shared_provider`]). `cargo xtask owner-gate` fails the build the
//! moment two Vaco crates both declare a `MEDIA`-listed dependency, and
//! `rustls` is on that list (`xtask/src/owner_gate.rs`) — "a transport swap
//! changes what bytes arrive" is exactly true of a TLS stack. `cargo xtask
//! dep-gate` (D10 Gate 1) separately checks that `ring` — and the `cc` build
//! machinery it needs — is reachable **only** through this crate; the
//! 2026-08-28 owner amendment to Gate 1 is what permits that reachability at
//! all (`planning/00-decisions.md`, "Gate 1 amendment": TLS carries no media
//! semantics, unlike every codec/container/filter crate Gate 1 still binds
//! absolutely). `vaco-protocol-http` already had the full D14.2 gate-by-gate
//! record for this trio in its own crate docs before this crate existed
//! (`ureq`'s `rustls-no-provider` + `rustls-webpki-roots` features needing
//! `rustls` present with matching feature flags for Cargo's feature
//! unification, and `unversioned_rustls_crypto_provider` needing an actual
//! `Arc<CryptoProvider>` to hand `ureq`); moving the *declaration* here
//! without repeating that record would just relocate the analysis, so
//! `docs/io/vaco-protocol-http.md` still carries it and this crate's own docs
//! point there rather than duplicating it. See `docs/dependencies.md` for the
//! `ring`-vs-`rustls-rustcrypto`-vs-`aws-lc-rs` assessment, and [`crypto`]'s
//! module docs for the one function that makes the shared ownership work.
//!
//! # wasm
//!
//! Exempted from `cargo xtask wasm-check` (`xtask/src/wasm.rs`'s
//! `NATIVE_ONLY`) — re-measured after the `rustls-rustcrypto`-to-`ring` swap:
//! a throwaway crate depending on `ring` alone still fails to build for
//! `wasm32-unknown-unknown`, on the same wall as before (`getrandom`'s own
//! hard `compile_error!` without wasm's `js` feature enabled — `ring` never
//! gets far enough to reach its own C/assembly on this target). Also
//! exempted from `cargo xtask time-gate`'s `NATIVE_ONLY` for the same reason
//! `vaco-protocol-http` is: `rustls` reaches the wall clock internally
//! (certificate expiry checks) as part of its own, not-ours-to-change,
//! implementation.

#![forbid(unsafe_code)]

pub mod connect;
pub mod crypto;
pub mod options;
pub mod pem;
pub mod protocol;
pub mod roots;
pub mod verify;

pub use options::TlsOptions;
pub use protocol::{TLS_PROTOCOL, TlsProtocol, TlsSink, TlsSource};

use vaco_protocol_core::ProtocolRegistry;

/// Register `tls:`.
pub fn register(registry: &mut ProtocolRegistry) {
    registry.register(&TLS_PROTOCOL);
}
