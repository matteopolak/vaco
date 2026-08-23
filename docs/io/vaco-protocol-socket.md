# `vaco-protocol-socket`

Layer 2. `tcp:`, `udp:`, `udplite:`, `unix:`.

## What it is

Four bare-socket transports, each a thin wrapper over a socket type the
standard library or `socket2` already provides: `tcp:` over
`std::net::TcpStream`/`TcpListener`, `udp:`/`udplite:` over a `socket2`-built,
`std::net::UdpSocket`-driven datagram socket, and `unix:` over
`std::os::unix::net` (with an always-registered fallback on targets with no
`AF_UNIX` — see "How it works" below). None of them opens another URL: they
are the leaves the rest of the protocol graph (`tls:`, `http:`/`https:`) sits
on top of.

## The whitelist rule — read this before wiring in a playlist protocol

**Every protocol in this crate grants nothing by default, and needs the
scheme itself explicitly granted.**

Measured directly against `ffmpeg 8.1` (D17), not assumed:

```
$ ffmpeg -v debug -i tcp://127.0.0.1:<port> ...
[tcp @ ...] No default whitelist set
```

`tcp:`, `udp:`, `udplite:` and `unix:` all print exactly this line — there is
no "`Setting default whitelist '...'`" line for any of them, which is what
`hls:`/`http:` print instead. That is expected: `default_whitelist` governs
what a protocol grants to URLs *it* opens, and none of these four protocols
opens one. So every `ProtocolDesc` here sets `default_whitelist: &[]`, and
`ProtocolFlags::nested_scheme` is `false` for all four.

The property this crate actually needs to hold — and which HLS/DASH-style
callers depend on — is the ordinary one: **a caller that wants to open
`tcp://…`/`udp://…`/`unix:…`, whether typed by a user or named by an
attacker-influenced playlist, must have the scheme itself on its whitelist**
(or be running fully unrestricted). Nothing here grants that implicitly.
`tests/whitelist.rs` and each protocol's own `*_needs_to_be_on_the_whitelist_itself`
test assert this directly.

See `docs/io/vaco-protocol-tls.md` for the one place this crate's own `tcp:`
registration is *not* how a security-relevant nested open happens: `tls:`
connects its own raw socket rather than routing through this crate's
`TcpProtocol`, because a duplex handshake cannot be expressed through
`vaco-protocol-core::Protocol::open`'s single-direction return type. That
crate still applies the same whitelist check by hand.

## How it works

| Module | Job |
|---|---|
| `url` | Parses `//host:port[?opt=val&...]` out of `Url::rest`. Total: never panics. |
| `addr` | Resolves `host:port` and connects with a bound timeout, trying every resolved address. |
| `options` | `TcpOptions`, `UdpOptions` (shared by `udp:`/`udplite:`), `UnixOptions` — names and defaults from `ffmpeg -h protocol=<name>`. |
| `tcp` | `TcpProtocol`: connect or (`-listen`) bind-and-accept-one, `TcpSource`/`TcpSink`. |
| `udp` | `UdpProtocol` (parameterised by `lite: bool`): builds the socket with `socket2` (for buffer sizes, multicast, TTL, TOS, `SO_REUSEADDR`), then does all I/O through `std::net::UdpSocket`'s safe, plain-`&mut [u8]` API — see the module docs for why (`socket2`'s own `recv`/`recv_from` need `&mut [MaybeUninit<u8>]`, and turning a `&mut [u8]` into that safely needs the exact unsafe cast `socket2` performs internally, which this crate cannot do itself under `#![forbid(unsafe_code)]`). |
| `unix` | `UnixProtocol` — real on `#[cfg(unix)]` (`native` submodule), a `#[cfg(not(unix))]` fallback that always reports `Unsupported` elsewhere, so `UNIX_PROTOCOL`'s ctor path resolves and the scheme is a known-but-unsupported name rather than an absent one on every platform. |

`unix:`'s grammar is a bare path (`unix:/tmp/sock`), not `//host:port` — a
socket path may contain `?`/`:` and is never split on them.

Framing: one `RawSource::read` call is one `recv`/`read` syscall. UDP has no
reassembly — a datagram larger than the caller's buffer is truncated by the
kernel exactly as a raw `recvfrom` would truncate it.

## How to change it

* **Adding a `tcp:`/`udp:` option**: probe `ffmpeg -h protocol=<name>` first
  (D6/D7/D17) — do not guess a default or a range. `options.rs`'s doc comment
  on each field records where its value came from.
* **`socket2` vs `std::net`**: build and configure with `socket2` (anything
  `std::net` has no accessor for); do I/O with `std::net`'s safe API once the
  socket is configured, via `Into<std::net::{TcpStream,UdpSocket}>`. Do not
  call `socket2::Socket::recv`/`recv_from` directly — see `udp.rs`'s module
  docs for why that would need `unsafe` this crate cannot use.
* **`udplite:`'s protocol number** is the IANA-assigned `136`, applied via
  `socket2::Protocol::from(136)` rather than `socket2::Protocol::UDPLITE` —
  the named constant only exists on Linux/Android/FreeBSD/Fuchsia (measured:
  `E0599` on this crate's own macOS development target). Do not "fix" this
  back to the named constant without re-checking every target this crate
  builds on.
* **Gotcha — `unix.rs`'s fallback must keep the exact same public names**
  (`UnixOptions`, `UnixProtocol`, `UNIX_PROTOCOL`) as the real implementation,
  because `lib.rs` re-exports them unconditionally and `vaco-component.toml`
  names `UNIX_PROTOCOL` with no platform condition (the fragment schema has
  none to give it).

## Configuration

`TcpOptions`, `UdpOptions`, `UnixOptions` (`src/options.rs`, `src/unix.rs`) —
one field per option this crate implements, in the reference's own
declaration order where practical.

**Deliberately not wired to a real syscall** (accepted for `-h`/interface
parity, each documented in its own field's doc comment and in each module's
"What is deliberately not implemented" section):

* `tcp_mss` — no cross-platform `socket2` accessor for `TCP_MAXSEG`.
* `udplite_coverage` — `UDPLITE_SEND_CSCOV`/`RECV_CSCOV` have no `socket2`
  accessor; a raw `setsockopt` would need `unsafe`.
* `bitrate`/`burst_bits` (UDP send pacing) — write-side, and D5 has zero
  muxers today.
* `fifo_size`/`overrun_nonfatal` — no background-thread circular buffer; every
  read is a synchronous `recv`.
* `sources`/`block` (source-specific multicast filtering) — group
  join/leave is implemented (`socket2::Socket::join_multicast_v4/v6`),
  per-source filtering is not.
* `unix:`'s `seqpacket` type — `std::os::unix::net` has no stable
  `UnixSeqpacket`; requesting it returns `ProtocolError::Unsupported`.
* `-listen 2` (accept, then keep listening) is treated identically to
  `-listen 1` (accept exactly one connection) — see `tcp.rs`'s module docs.

## Dependencies

`socket2` (D14.3, permitted) for options `std::net` has no portable accessor
for. `vaco-protocol-core` for the trait and the whitelist gate (not this
crate's to change). `vaco-time` for the listen-timeout poll loop (`Instant`,
`sleep` — never `std::time`/`std::thread::sleep` directly, per D18). Nothing
else: no TLS, no HTTP, no PEM parsing — those live in `vaco-protocol-tls` and
`vaco-protocol-http`.

## wasm

Exempted from `cargo xtask wasm-check` (`xtask/src/wasm.rs`'s `NATIVE_ONLY`).
Measured, not assumed: a throwaway crate depending on `socket2` alone fails to
build for `wasm32-unknown-unknown` with nine `E0308`/`E0061`/`E0583` errors
inside `socket2` itself. `std::net` alone *does* compile there (a stub that
returns `io::Error`, not a `compile_error!`), but `socket2` does not, so this
crate cannot either.

The registry fragment (`vaco-component.toml`) sets `default = false` for the
same reason `vaco-protocol-http`'s own fragment does: registering
unconditionally would make `protocol-socket` a *default* optional dependency
of `vaco-registry`, regressing `vaco-registry`'s own `wasm-check`.
