# vaco-protocol-dial

## What it is

Shared dial helpers for protocols whose duplex round trip must complete
before `vaco_protocol_core::Protocol::open`/`create` has anything to hand
back. Five crates (`vaco-protocol-ftp`, `-httpproxy`, `-gopher`, `-icecast`,
and formerly a copy each) independently reproduced the same `check_scheme`
+ `connect` pair, a TLS variant of it, and a byte-at-a-time header reader.
This crate holds each of those once.

## How it works

- `dial_tcp(hp, timeout, env)` — checks `"tcp"` against `env`'s whitelist,
  then connects via `vaco_protocol_socket::addr::connect`.
- `dial_tls(hp, timeout, env, opts)` — checks `"tls"`, then reuses
  `vaco_protocol_tls::connect::{connect_tcp, handshake}` for the TCP leg
  (which checks `"tcp"` itself) and the handshake.
- `read_header_block(stream, scheme, eof_detail)` — reads a header block a
  byte at a time up to `MAX_HEADER_BYTES`, so callers that hand `stream` back
  as a live connection afterward never strand read-ahead bytes in a
  `BufReader`. `scheme`/`eof_detail` let each caller phrase its own error.

Every one of these checks the whitelist itself, exactly where
`vaco_protocol_core::ProtocolRegistry::resolve` would have for a normal
nested open — the whitelist property holds even though the bytes never pass
through a registered `tcp:`/`tls:` `Protocol`.

## How to change it

`src/lib.rs` is the whole crate. `vaco-protocol-tls`'s own `connect.rs`
predates this crate and is not built on it: `dial_tls` depends on
`vaco-protocol-tls`, so the reverse dependency would be a cycle
(`cargo xtask layer-check` catches this if attempted). Its `connect_tcp` is
the base `dial_tcp` calls into for the TLS path's TCP leg, not a caller of
it.

Adding a sixth protocol with this shape: add it as a dependent, call
`dial_tcp`/`dial_tls` from its `Protocol::open`/`create`, and if it needs a
header-block read, call `read_header_block` with its own scheme name and
EOF wording.

## Configuration

None — this crate exposes no `Protocol` and no options schema.

## Dependencies

`vaco-protocol-core` (types), `vaco-protocol-socket` (`addr::connect`,
`HostPort`), `vaco-protocol-tls` (`connect_tcp`, `handshake`, `TlsOptions`).
Native-only: see `xtask/src/wasm.rs`'s `NATIVE_ONLY` entry — `dial_tls`
pulls in `vaco-protocol-tls`'s `getrandom`/`ring` wall (`ring` replaced
`rustls-rustcrypto` 2026-08-28; same wasm wall either way — see
`docs/dependencies.md`'s `ring` entry).

## Testing

Unit tests cover the whitelist-denied/-granted paths for `dial_tcp` against
a loopback listener, and `read_header_block`'s terminator/EOF/size-limit
behaviour against an in-memory `Cursor`. `fuzz/fuzz_targets/
protocol_dial_read_header_block.rs` fuzzes `read_header_block` directly
(30 s breadth run: exit 0, ~17.4M execs, no artifacts). Not verified here:
`dial_tls` against a real (non-loopback, non-fake) TLS server — each
dependent crate's own docs already say the same about their own callers.
