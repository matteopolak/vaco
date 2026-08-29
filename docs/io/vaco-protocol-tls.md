# `vaco-protocol-tls`

Layer 2. `tls:` — and the one crate in this workspace that declares
`rustls`/`ring`/`webpki-roots` (D11).

## What it is

A raw TLS-over-TCP transport: the scheme a demuxer opens when it needs a
TLS-secured byte stream that is not HTTP(S) (RTMPS is the usual case). It is
also, independently of `tls:` itself, the crate that owns this workspace's
`rustls` dependency — `vaco-protocol-http` depends on this crate instead of
declaring `rustls`/`ring` itself. See "Who owns `rustls`" below.

**2026-08-28 update**: the crypto provider is `ring`, not `rustls-rustcrypto`.
The owner's Gate 1 amendment (`planning/00-decisions.md`) permits FFI for TLS
specifically; `rustls-rustcrypto` (D14.2's original pure-Rust choice) was
pinned at `0.0.2-alpha` with no release since 2024-04-24 and seven RUSTSEC
advisories that could not clear without one, failing Gate 3 outright. See
`docs/dependencies.md`'s `ring` entry for the full assessment.

## The whitelist rule — read this before wiring in HLS/DASH

**`tls:` grants nothing by default, and needs `tcp` granted to it
explicitly.** Measured directly against `ffmpeg 8.1` (D17):

```
$ ffmpeg -protocol_whitelist tls -listen_timeout 1000 -i tls://127.0.0.1:<port> -f null -
[tcp @ ...] Protocol 'tcp' not on whitelist 'tls'!

$ ffmpeg -protocol_whitelist tls,tcp -listen_timeout 1000 -i tls://127.0.0.1:<port> -f null -
[tls @ ...] IO error: Connection reset by peer   # past the gate; the peer wasn't speaking TLS
```

`ffmpeg -v debug`'s own log confirms this is a genuine absence, not an
unmeasured default: `[tls @ ...] No default whitelist set`. So
`TLS_PROTOCOL.default_whitelist` is `&[]` — the same answer every protocol in
this project that opens a nested URL of its own gives (`cache:`, `concat:`,
`subfile:` in `vaco-protocol-wrap`; `tcp:`/`udp:`/`udplite:`/`unix:` in
`vaco-protocol-socket`, trivially, since they open nothing). **`hls:`'s
curated default grant is the exception in this project, not the rule** — see
`vaco-protocol-wrap`'s crate docs for the general argument.

The same measurement, one level up: `https:` opens a nested `tls:`, and it
does not grant that by default either (`-protocol_whitelist https` alone
refuses the nested `tls` open with the same message shape). A caller that
wants `https:` to work fully unrestricted-adjacent still needs to name every
scheme the chain actually uses.

**A discrepancy worth flagging for HLS/DASH, not something this crate can
fix**: `vaco-protocol-core::env::ProtocolEnv::check_scheme` unions a parent's
`default_whitelist` into an *explicit* caller whitelist at every nesting
depth (`W2/W3` in that module's docs). The reference does not do this: an
explicit `-protocol_whitelist` **replaces** any protocol's own default grant
entirely, at every depth — a protocol's own default list is only ever
consulted when the *whole call* had no explicit whitelist at all. Measured
directly: `-protocol_whitelist https` (naming only `https`) refuses a nested
`tls` open even though `vaco-protocol-http`'s own `DEFAULT_WHITELIST` constant
lists `tls` among what `http`/`https` grant — under `vaco-protocol-core`'s
current model, that default would be unioned in and the same nested open
would *succeed* where the reference refuses it. This makes the current
project model **more permissive** than the reference in the
explicit-whitelist case, which is the wrong direction for a security control.
It is `vaco-protocol-core`'s model to change, not this crate's — reported
here because whoever tightens it will need exactly this measurement.

## Who owns `rustls`

**This crate.** `cargo xtask owner-gate` fails the build the instant two Vaco
crates both declare a `MEDIA`-listed external dependency in their own
`[dependencies]` table (`xtask/src/owner_gate.rs`; `rustls` is on that list —
"a transport swap changes what bytes arrive"; the crypto provider it activates
is not its own row any more, because `ring` arrives through a Cargo feature
rather than a manifest declaration — see that file's comment). `vaco-protocol-http`
needed a crypto provider for `ureq` before this crate existed, so it declared
`rustls`/`rustls-rustcrypto` directly and wrote up the full D14.2 gate-by-gate
record in its own crate docs. That record is **still there, unduplicated** —
`docs/io/vaco-protocol-http.md`'s "Dependencies" section — because it predates
this crate and moving the *declaration* should not orphan the analysis. What
changed: `vaco-protocol-http`'s `Cargo.toml` no longer lists `rustls` itself;
its `transport.rs` calls [`crypto::shared_provider`](../../crates/io/vaco-protocol-tls/src/crypto.rs)
here instead, so both crates' TLS configuration is built from the exact same
`Arc<rustls::crypto::CryptoProvider>` in a process that uses both.

Note that `cargo xtask dep-gate` (D10 Gate 1, not D11 `owner-gate`) sees `ring`
show up in the resolved build graph under **both** `vaco-protocol-tls` and
`vaco-protocol-http` — that is Cargo feature unification on the one shared
`rustls` package, not a second declaration; `dep-gate`'s own comment on the
`ring` row (`xtask/src/deps.rs`) explains why both are correctly permitted.

`webpki-roots` is declared here only — `vaco-protocol-http` never touches it
directly (it reaches Mozilla's root bundle through `ureq`'s own
`rustls-webpki-roots` feature, as before).

## How it works

| Module | Job |
|---|---|
| `options` | `TlsOptions` — `verify`/`tls_verify`, `ca_file`/`cafile`, `verifyhost`. |
| `crypto` | `shared_provider()` — the one `ring`-backed provider this crate and `vaco-protocol-http` both use. |
| `pem` | A small, whitespace-tolerant PEM block extractor + base64 decoder, written here (D10: no PEM-parsing crate is pre-declared). |
| `roots` | The default root store (`webpki-roots`), optionally extended with `-ca_file`'s certificates (appended, never substituted). |
| `verify` | `PermissiveVerifier` (the `verify=false` default — see below) and `client_config()`, which builds the full `rustls::ClientConfig` either way. |
| `connect` | Resolves and connects the raw `TcpStream` directly (not through `vaco-protocol-socket`'s registered `tcp:`), applies the whitelist check by hand, and drives the handshake to completion before returning. |
| `protocol` | `TlsProtocol`: the `Protocol::open`/`create` entry points. |

### Why `tls:` does not nest through `vaco-protocol-socket`

`vaco_protocol_core::Protocol::open` returns a read-only `Box<dyn
MediaSource>`. A TLS handshake needs both directions on one connection before
there is anything to hand back to a caller at all, so there is no way to get
a usable transport out of one `Protocol::open` call as the trait is shaped
today (D5: the trait was designed for a demux-only v0.1, per its own module
docs). `crate::connect`'s module docs have the full argument; in short, this
crate connects its own `std::net::TcpStream` (reusing
`vaco_protocol_socket::addr::connect` and `vaco_protocol_socket::url::parse`
rather than duplicating that logic) and calls
`vaco_protocol_core::ProtocolEnv::check_scheme("tcp")` by hand, exactly where
`ProtocolRegistry::resolve` would have called it for a real nested open. This
is a reported gap in `vaco-protocol-core`'s trait, not a workaround: a
`Protocol` genuinely cannot express "give me a duplex transport" today.

### `-verify`/`-tls_verify` defaults to `false` — the reference's own default

Measured (`ffmpeg -h protocol=tls`): `-tls_verify <boolean> ED... Verify the
peer certificate (default false)`. This crate matches that default
deliberately. What it still checks even when `verify` is left off: the
handshake's cryptographic signature, via `rustls::crypto::verify_tls12_signature`/
`verify_tls13_signature` against the peer's offered public key — a
passive attacker cannot forge the handshake without also possessing (or
successfully forging, which this check catches) a matching private key. What
it does *not* check: the certificate's trust chain, or its hostname. See
`src/verify.rs`'s module docs for the full reasoning. **A caller that wants
real certificate validation must pass `-verify 1` (or `-tls_verify 1`)
explicitly, exactly as with the reference.**

`-verifyhost`, when set, replaces the hostname used for **both** SNI and
verification — a deliberate simplification (the reference may present one SNI
name while verifying a different one; this crate does not distinguish them).

### Security: `ca_file` cannot come from an attacker-controlled URL

`tls:`'s inline `?key=value` query options (parsed by
`vaco_protocol_socket::url::parse`, shared with `tcp:`/`udp:`) are
**discarded** — `crate::connect::host_port` only keeps the `host:port` pair
from that parse. `ca_file` is read only from the trusted `-opt`/`Dict`
surface (the CLI, or an embedder's own code), never from a URL's own query
string. This is what stops a hostile playlist entry shaped like
`tls://evil.example:443?ca_file=/etc/passwd` from ever reaching the
filesystem read in `protocol::read_ca_file` — the query portion of a `tls:`
URL is simply never consulted for options at all.

## How to change it

* **Adding a `-h protocol=tls` option**: probe first (D6/D7/D17); do not
  guess a default. `options.rs` records the exact probe this crate's fields
  came from.
* **Client certificate authentication and `-listen` (TLS server mode)** are
  the two biggest deferred pieces — both need a private-key parser
  (PKCS#8/RSA DER) this crate does not have. See the crate's `lib.rs` module
  docs for the full deferred list.
* **Gotcha**: do not add `rustls` to any other crate's `Cargo.toml` —
  `cargo xtask owner-gate` will fail the build. Route through
  `crate::crypto::shared_provider` instead, the way `vaco-protocol-http` does.
* **This crate does not depend on `vaco-protocol-dial`, and should not.**
  `vaco-protocol-dial`'s `dial_tls` depends on this crate for `connect_tcp`/
  `handshake`, so the reverse dependency would be
  `vaco-protocol-tls -> vaco-protocol-dial -> vaco-protocol-tls`, a cycle
  `cargo xtask layer-check` would refuse. `connect.rs` is the base the shared
  dial helper calls into, not a caller of it — this crate predates
  `vaco-protocol-dial` and is not a fifth copy of the pattern it factored
  out.

## Configuration

`TlsOptions` (`src/options.rs`): `verify`/`tls_verify`, `ca_file`/`cafile`,
`verifyhost`. Deferred (accepted for interface parity in a future revision,
not yet in this struct at all — see the crate's `lib.rs` docs): `cert_pem`,
`key_pem`, `cert_file`/`cert`, `key_file`/`key`, `listen`, `http_proxy`,
`external_sock`, `use_srtp`, `mtu`.

## Dependencies

`rustls` 0.23 (`default-features = false`, `features = ["std", "tls12",
"ring"]` — the `ring` feature is what makes `rustls::crypto::ring` available;
see `docs/dependencies.md`'s `ring` entry for why it replaced
`rustls-rustcrypto` and why it was chosen over `aws-lc-rs`), `webpki-roots`
1.x. `vaco-protocol-socket` for `addr::connect`/`url::parse` (see "How it
works" above). `vaco-protocol-core` for the trait and the whitelist gate (not
this crate's to change).

## wasm

Exempted from `cargo xtask wasm-check` and `cargo xtask time-gate`
(`xtask/src/wasm.rs` and `xtask/src/time_gate.rs`'s `NATIVE_ONLY` lists), both
for the same underlying reason `vaco-protocol-http` is exempt from each:
`ring` (reached via `rustls`'s own `ring` feature) pulls `getrandom` without
wasm's `js` feature enabled, which fails to compile for
`wasm32-unknown-unknown` with `getrandom`'s own hard `compile_error!` —
re-measured directly against a throwaway crate depending on `ring` alone
after the `rustls-rustcrypto`-to-`ring` swap, same wall as before; and
`rustls` reaches the wall clock internally for certificate-expiry checks,
which is its own implementation, not this crate's to route through
`vaco-time`.

The registry fragment (`vaco-component.toml`) sets `default = false` for the
same wasm-regression reason `vaco-protocol-http`'s and
`vaco-protocol-socket`'s fragments do.
