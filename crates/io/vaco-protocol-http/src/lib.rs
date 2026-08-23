//! The `http:` and `https:` protocols: ranged reads, redirects gated by the
//! whitelist, reconnection, and the option surface the reference documents
//! under `-h protocol=http`.
//!
//! # What it is
//!
//! A [`vaco_protocol_core::Protocol`] implementation over a pure-Rust HTTP
//! client (`ureq`, with `rustls` and a pure-Rust crypto provider — see
//! "Dependencies" below). It supports `Range`-based seeking so that opening a
//! remote file and seeking to its end (an `moov` atom placed last, a
//! Matroska cue index, an ID3 footer) issues one small request instead of
//! downloading the file from the start; redirects, resolved and re-checked
//! against the same whitelist every other nested open goes through; custom
//! headers and user agent; cookies; ICY metadata request; HTTP Basic auth
//! from URL userinfo; and the `-reconnect*` family.
//!
//! # How it works
//!
//! Three layers, in increasing order of how OS-specific they are:
//!
//! | Module | What it does | Portable? |
//! |---|---|---|
//! | [`options`] | The option surface (`HttpOptions`), a plain data struct. | Yes |
//! | [`url`] | Building the outbound request target; resolving a `Location` header (absolute, protocol-relative, absolute-path, relative) against the request that produced it, per RFC 3986 §5. | Yes |
//! | [`headers`] | Assembling the default header set plus `-headers` overrides, from options and a byte range. | Yes |
//! | [`parse`] | Parsing bytes a *server* controls: `Content-Range`, `Retry-After`, the `-reconnect_on_http_error` list. This is the fuzz target's surface — see `fuzz/fuzz_targets/protocol_http_response.rs`. | Yes |
//! | [`reconnect`] | Whether to retry a failure and how long to wait first. Takes/returns plain values (`vaco_time`), never sleeps itself. | Yes |
//! | [`transport`] | The `ureq::Agent`, TLS configuration, and turning `ureq::Error` into [`vaco_protocol_core::ProtocolError`]. | **No** — sockets and TLS. |
//! | [`source`] | [`source::HttpSource`]: ties the above together into a [`vaco_io::RawSource`], including the reconnect loop and the "server ignored my `Range`" safety net. | Mostly not (drives `ureq::Body`), but its only call into `transport` is one function. |
//! | [`protocol`] | [`protocol::HttpProtocol`]: the `Protocol::open` entry point, and the redirect-follows-through-the-whitelist loop. | No (constructs `ureq` requests directly for the initial connect). |
//!
//! `wasm-check` exempts this crate by name (see `xtask/src/wasm.rs`'s
//! `NATIVE_ONLY` list) — `wasm32-unknown-unknown` has no sockets, and a
//! browser port would go through `fetch` behind the same
//! [`vaco_protocol_core::Protocol`] trait rather than being a wasm build of
//! *this* crate. `options`, `url`, `headers`, `parse` and `reconnect` are
//! written so that a future `vaco-protocol-fetch` needs only to replace
//! `transport`/`source`/`protocol`'s socket-facing parts, reusing everything
//! above unchanged.
//!
//! # Security: redirects go through the whitelist too
//!
//! See [`protocol`]'s module docs for the full argument. In one sentence: a
//! `Location` header is a URL chosen by whoever answered the socket, so it is
//! checked by [`vaco_protocol_core::ProtocolEnv::check_scheme`] — the same
//! gate every nested protocol open goes through — before this crate ever
//! connects to it. `tests/redirect_whitelist.rs` proves a redirect to `file:`
//! is refused, without a real network.
//!
//! # ICY/SHOUTcast metadata
//!
//! `Icy-MetaData: 1` is sent whenever `-icy` is on (the default); when a
//! server answers with `icy-metaint`, [`source::HttpSource`] strips the
//! interleaved metadata blocks out of the byte stream it hands to a demuxer
//! (there is no RFC for this wire format — probed against a loopback server,
//! `tests/icy_metadata.rs`) and keeps the most recently seen block's text
//! reachable through [`source::HttpSource::icy_metadata`]. That accessor is
//! not yet reachable from a `Box<dyn` [`vaco_io::MediaSource`]`>` — the trait
//! has no metadata side channel — so nothing in the project routes it to a
//! demuxer's own metadata table yet; see that method's doc comment.
//!
//! # Chunked POST
//!
//! [`Protocol::create`](vaco_protocol_core::Protocol::create) now does
//! something: [`post::HttpSink`] buffers a caller's
//! [`vaco_io::MediaSink::write`] calls and sends them as one
//! `Transfer-Encoding: chunked` `POST` on the first
//! [`vaco_io::MediaSink::flush`] call (`MediaSink` has no dedicated
//! finish/close hook, so `flush` is the only "no more bytes are coming"
//! signal available — see [`post`]'s module docs for the full reasoning,
//! including why the body is buffered rather than streamed onto the socket
//! as each `write` happens). `-chunked_post 0` (a fixed-`Content-Length`
//! POST) is accepted as an option but not implemented.
//!
//! # What is deliberately not implemented
//!
//! See [`options`]'s module docs for the full list (proxy, server/`-listen`
//! mode, `-post_data`, `-send_expect_100`, chunked-readahead sizing) and why.
//! A correct proxy/server implementation is substantial enough to deserve
//! its own review rather than riding along here. Also not implemented: the
//! `Retry-After` HTTP-date form (only the delay-seconds form is parsed — see
//! [`parse::parse_retry_after_secs`]).
//!
//! # Dependencies (D10)
//!
//! | Crate | Gate 1 (pure Rust) | Gate 2 (licence) | Gate 3 (trusted) |
//! |---|---|---|---|
//! | `ureq` 3.x, with `rustls-no-provider` + `rustls-webpki-roots` (**not** the `rustls` feature — that pulls `ring`) | Pass: no `-sys`, no build script compiling native code. | MIT OR Apache-2.0. | Widely used pure-Rust HTTP client; active. |
//! | `rustls` 0.23, `default-features = false`, `features = ["std", "tls12"]` | Pass, with no crypto provider of its own — that is the entire point of the feature choice above. | Apache-2.0 OR ISC OR MIT (dual/triple; ISC and Apache-2.0/MIT are all on the D3 allow-list). | The de facto standard pure-Rust TLS library. |
//! | `rustls-rustcrypto` 0.0.2-alpha | Pass: every dependency it pulls (`aes-gcm`, `chacha20poly1305`, `p256`, `p384`, `rsa`, `ed25519-dalek`, `x25519-dalek`, `sha2`, `hmac`, `der`, `pkcs8`, `sec1`, `signature`, `rand_core`) is a `RustCrypto` pure-Rust crate; `cargo tree` on this crate contains nothing that links or compiles a foreign library. | MIT OR Apache-2.0. | **The honest caveat, per D10/D14.2**: `0.0.2-alpha` is a pre-1.0, low-download crate — it does *not* clear D10's "adopted" bar on reputation alone. It is taken up anyway because D14.2 already made this call at the workspace level (`ring` and `aws-lc-rs`, rustls's two production providers, both vendor and compile C/assembly and fail Gate 1 outright — see `planning/00-decisions.md` D14.2), and it is the only *other* rustls crypto provider available with zero FFI. `Cargo.lock`/`cargo tree` confirm nothing in this crate's dependency graph is `ring` or `aws-lc-rs`/`aws-lc-sys`; `cargo xtask dep-gate` checks this in CI, denying exactly those three names. Re-check this provider's maturity at every release, per D10's "re-checked each release" — this is the one dependency in this crate's graph that has not yet earned trust by track record, only by being the sole pure-Rust alternative. |
//! | `webpki-roots` | Not a direct dependency of this crate — reached transitively through `ureq`'s `rustls-webpki-roots` feature, which builds the default `RootCertStore` from it. Removed from this crate's own `Cargo.toml` after the initial stub, since nothing here touches the crate directly. | MIT OR Apache-2.0. | Mozilla's own root bundle, repackaged; itself widely used (`rustls` depends on it in most deployments). |
//!
//! **Update (D11, `vaco-protocol-tls` landed later): this crate's own
//! `Cargo.toml` no longer declares `rustls`/`rustls-rustcrypto` directly.**
//! `cargo xtask owner-gate` fails the build the moment two Vaco crates both
//! declare either — both are on its `MEDIA` list ("a transport swap changes
//! what bytes arrive") — and `vaco-protocol-tls` is now the crate that owns
//! them, for the `tls:` protocol it registers. This crate depends on
//! `vaco-protocol-tls` instead and calls
//! [`vaco_protocol_tls::crypto::shared_provider`] from `crate::transport`'s
//! `agent()` in place of building its own `Arc<rustls::crypto::CryptoProvider>`.
//! The gate-by-gate record above is left exactly as it was written — it is
//! still an accurate account of *why* this trio was chosen, just no longer of
//! which `Cargo.toml` declares it. See `docs/io/vaco-protocol-tls.md`'s "Who
//! owns rustls" for the other half of this arrangement.
//!
//! The `ureq`/`rustls`/`rustls-rustcrypto` trio was already the exact D14.2
//! answer to "which TLS provider" before that move — this crate's original
//! contribution was wiring `ureq`'s `unversioned_rustls_crypto_provider` to it
//! rather than accepting `ureq`'s own `rustls` feature default, which is
//! `ring`.
//!
//! # Configuration
//!
//! [`options::HttpOptions`] — one field per `-h protocol=http` option this
//! crate implements, in the reference's own declaration order. Also honours
//! [`vaco_protocol_core::ProtocolEnv::rw_timeout`] as a per-request timeout.

#![forbid(unsafe_code)]

pub mod headers;
pub mod options;
pub mod parse;
pub mod post;
pub mod protocol;
pub mod reconnect;
pub mod source;
pub mod transport;
pub mod url;

pub use options::HttpOptions;
pub use post::HttpSink;
pub use protocol::{HTTP_PROTOCOL, HTTPS_PROTOCOL, HttpProtocol};
pub use source::HttpSource;

use vaco_protocol_core::ProtocolRegistry;

/// Register `http:` and `https:`.
///
/// `vaco-registry` calls this; so does every test that needs a real (or, per
/// this crate's own tests, a locally-bound) HTTP open.
pub fn register(registry: &mut ProtocolRegistry) {
    registry.register(&HTTP_PROTOCOL);
    registry.register(&HTTPS_PROTOCOL);
}
