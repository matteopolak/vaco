# `vaco-protocol-dtls`

Layer 2. `dtls:` — and the one crate in this workspace that declares
`openssl` (D11).

## What it is

A DTLS-over-UDP transport (RFC 6347): the scheme a WHIP/WebRTC-shaped caller
opens for a datagram stream that needs a DTLS handshake before any
application data flows. This closes #562 (`PR-12b`), the last piece of #64
blocked from project start on "no native Rust DTLS stack exists" — the
2026-08-28 owner amendment to D10's Gate 1 (`planning/00-decisions.md`, "Gate
1 amendment") permits FFI for transport security specifically, and `openssl`
is the only credible DTLS implementation once FFI is on the table (see
`docs/dependencies.md`'s `openssl` entry for the full assessment against
`boring`/`wolfssl`).

## The whitelist rule

Same shape as `vaco-protocol-tls`'s `tls:`, applied to `udp` instead of
`tcp`: `dtls:` grants nothing by default (`default_whitelist` is `&[]`) and
needs `udp` granted to it explicitly. See that crate's docs for the general
argument (every protocol that opens a nested connection of its own follows
this rule; `hls:`'s curated grant is the one deliberate exception).

## Who owns `openssl`

**This crate, alone.** `cargo xtask owner-gate` fails the build the instant
two Vaco crates both declare a `MEDIA`-listed dependency (`openssl` is on
that list, `xtask/src/owner_gate.rs` — "a transport swap changes what bytes
arrive"). `cargo xtask dep-gate` (D10 Gate 1) separately checks that `openssl`
and the build machinery it needs (`cc`, `pkg-config`, `vcpkg`,
`openssl-sys`, `openssl-src`) are reachable *only* through this crate — the
2026-08-28 Gate 1 amendment is what permits that reachability at all.
`vaco-protocol-tls`'s own `ring` dependency and this crate's `openssl`
dependency are independent: nothing shares a crypto provider between TLS and
DTLS in this workspace, because nothing needs to.

## How it works

| Module | Job |
|---|---|
| `options` | `DtlsOptions` — `-h protocol=dtls`'s surface, the parts this crate implements. |
| `cert` | Loading a configured PEM certificate/key, or generating an ephemeral self-signed one when none is configured — see its module docs for why that default (not an error) is the right call for DTLS specifically. |
| `context` | Building the `openssl::ssl::SslContext` `-verify`/`-use_srtp`/`-mtu` describe, shared by both the client and server paths. |
| `transport` | `UdpTransport` — a connected `UdpSocket` wearing the `Read`/`Write` coat `openssl::ssl::SslStream` needs. See its module docs for the one thing it deliberately does not do (DTLS retransmission timers). |
| `connect` | The client path (`Protocol::open`/`create` with `-listen` unset): connects its own UDP socket, applies the whitelist check by hand, and drives the handshake. Also `export_srtp_keying_material`, for `-use_srtp`'s payoff. |
| `listen` | The server path (`-listen 1`): binds, waits for the first peer datagram, connects the socket to that peer, and drives the handshake. See its module docs for the scoping decision (no stateless cookie exchange). |
| `protocol` | `DtlsProtocol`: the `Protocol::open`/`create` entry points, dispatching to `connect` or `listen` per `-listen`/`IoFlags::listen`. |

### Why `dtls:` does not nest through `vaco-protocol-socket`'s `udp:`

Same argument as `vaco-protocol-tls`'s `tls:`/`tcp:` relationship (see that
crate's `connect` module docs for the full case): `Protocol::open` returns a
read-only `Box<dyn MediaSource>`, and a DTLS handshake needs both directions
on one connection before there is anything to hand back at all.

### `-verify`/`-tls_verify` defaults to `false` — and here it means something different than for TLS

Measured (`ffmpeg -h protocol=dtls`): `-tls_verify <boolean> ED... (default
false)`, same default as `tls:`. But DTLS's actual callers (WebRTC/WHIP)
routinely present a self-signed certificate whose *fingerprint* was already
exchanged over signalling (SDP `a=fingerprint`) — there is often no CA chain
to check even in principle, so this crate generates one on the fly when none
is configured (`cert` module) rather than refusing to open `dtls:` at all.
`-verify 1` with `-ca_file` still turns on real chain verification, and a
self-signed certificate is still correctly refused in that mode — see
`tests/handshake_success.rs`'s `verify_true_without_the_private_ca_is_refused`.

## How to change it

* **Adding a `-h protocol=dtls` option**: probe first (D6/D7/D17);
  `options.rs` records the exact probe this crate's fields came from.
* **DTLS retransmission timers, and `-listen`'s stateless cookie exchange**
  are the two biggest scoped-out pieces — see `transport`'s and `listen`'s
  module docs for exactly what is missing and why it does not show up in
  this crate's own (loopback) test suite.
* **Gotcha**: do not add `openssl` to any other crate's `Cargo.toml` —
  `cargo xtask owner-gate` will fail the build.

## Configuration

`DtlsOptions` (`src/options.rs`): `listen`, `use_srtp`, `mtu`, `cert_pem`,
`key_pem`, `cert_file`/`cert`, `key_file`/`key`, `verify`/`tls_verify`,
`ca_file`/`cafile`, `verifyhost`. Not implemented: `-http_proxy`,
`-external_sock` — same blockers `vaco-protocol-tls` records for the same
options (no proxy tunnelling here, and no already-connected-fd handoff exists
in this project's `IoFlags`/`ProtocolEnv` model).

## Dependencies

`openssl` 0.10, `features = ["vendored"]` (so no system OpenSSL install is
required at build time — `openssl-src` compiles OpenSSL 3.6.3 from source).
See `docs/dependencies.md` for the full Gate 1/2/3 record, including why
`openssl` was chosen over `boring`/`wolfssl`. `vaco-protocol-socket` for
`addr::resolve` (name resolution only — DTLS connects its own `UdpSocket`
rather than reusing a connect helper, since UDP has no analogous "connect and
retry every address" primitive worth sharing). `vaco-time` for the
`-listen` accept loop's polling (D18: the clock, behind one door).
`vaco-protocol-core` for the trait and the whitelist gate.

## Testing

`tests/handshake_success.rs` runs a full handshake between two real
`openssl` endpoints over loopback UDP through the whole `Protocol::open`/
`create` path, including the case that actually tests verification (a
self-signed certificate rejected under `verify = true` with no matching CA).
Beyond this crate's own test suite: a real handshake was also run against the
actual `ffmpeg 8.1` reference binary (built `--enable-openssl`) acting as the
`-listen 1` DTLS peer, confirming interoperability with the reference
implementation directly, not only with this crate's own client/server pair —
see `docs/dependencies.md`'s `openssl` entry for the exact transcript.

No fuzz target: unlike `vaco-protocol-tls`'s `pem.rs` (a hand-rolled parser,
fuzzed as `tls_pem_parse`), this crate has no hand-written parser of its own
over untrusted network bytes — PEM parsing goes through `openssl`'s own
`X509::from_pem`/`PKey::private_key_from_pem`, and the DTLS record/handshake
parsing untrusted peer bytes actually go through is entirely inside
`openssl`'s own C code, which is not this project's fuzzing surface to own.

## wasm

Native-only (`xtask/src/wasm.rs`'s `NATIVE_ONLY`, `xtask/src/time_gate.rs`'s
`NATIVE_ONLY`): `openssl-sys`'s vendored build compiles C via `cc`, which does
not target `wasm32-unknown-unknown`, and DTLS needs a real UDP socket, which
that target has none of either. The registry fragment
(`vaco-component.toml`) sets `default = false` for the same wasm-regression
reason `vaco-protocol-tls`'s and `vaco-protocol-socket`'s fragments do.
