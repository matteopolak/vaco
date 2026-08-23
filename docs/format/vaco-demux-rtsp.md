# `vaco-demux-rtsp`

Layer 4. The RTSP 1.0/2.0 session layer (RFC 2326 / RFC 7826) and the
`rtsp`/`rtp`/`sdp` container demuxers.

## The transport security rule — read this first

RTSP negotiates a transport **with a remote server**, and the server names
the port/address that transport then uses (`Transport:` response headers).
A hostile or compromised server is exactly a remote attacker choosing those
values. This crate's rule:

* **Local** UDP ports (what this crate binds to receive on) are always
  chosen by this crate itself, from `-min_port`/`-max_port`
  (`RtspOptions`, default `5000`/`65000` — measured from `ffmpeg -h
  demuxer=rtsp` 8.1). A server cannot make this crate bind a port of the
  server's choosing.
* **Remote** addresses (where receiver reports are sent, unicast; the
  multicast group to join) *are* server-chosen — that is RFC 2326's
  negotiation working as specified, not a bypass.
* Every socket open goes through `vaco-protocol-core::ProtocolEnv::check_scheme`
  — `crate::transport::udp` for UDP, `crate::connection::connect_tcp` for
  the control connection (which, like `vaco-protocol-tls`, cannot go
  through the registry at all since it needs a duplex transport
  `Protocol::open` cannot express — it calls `check_scheme("tcp")` by hand,
  exactly where `ProtocolRegistry::resolve` would). Nothing here opens a
  socket by constructing a transport directly.
* **`-protocol_whitelist`'s default for `rtsp`, measured**: `[rtsp @ ...]
  No default whitelist set` — same as every protocol in this project that
  opens a nested URL of its own. An embedder must name `udp`/`tcp`
  explicitly; there is no curated default grant the way `hls`'s is.

## What is in here

| Module | Job |
|---|---|
| `message` | RTSP request/response text grammar |
| `transport` | `Transport:` header, the four modes, `transport::udp` (the security-critical socket-opening module) |
| `auth` | Basic/Digest (RFC 2617), MD5 via `vaco-hash` (this crate does not declare `md-5` itself — D11) |
| `base64` | A small RFC 4648 codec — no `base64` crate is declared workspace-wide (D10) |
| `connection` | The duplex control connection, `$`-interleaved frame demuxing |
| `http_tunnel` | RTSP-over-HTTP (Apple's tunnelling scheme): two raw HTTP legs |
| `session` | `OPTIONS`/`DESCRIBE`/`SETUP`/`PLAY`/`PAUSE`/`TEARDOWN`/`GET_PARAMETER` keepalive, session ids, Digest re-auth on `401` |
| `demux` | `RtspDemuxer`, `RtpDemuxer`, `SdpDemuxer`, and the registered `DemuxerDesc`s |
| `options` | `RtspOptions` — names/defaults from `ffmpeg -h demuxer=rtsp` 8.1 |

## How it works

`RtspDemuxer::open` is the real entry point (not the registered
`RTSP_DEMUXER`, which always returns `Unsupported` — see below): connect,
`DESCRIBE`, `SETUP` every allowed track, `PLAY`, then `read_packet` drains
either the interleaved control connection or each track's UDP socket
(round-robin, short per-socket timeout) and depacketises via
`vaco_format_rtp::for_encoding`.

## A gap this crate reports rather than works around

`vaco_format_core::Demuxer` has no `play`/`pause` methods (an earlier
planning draft sketched them for RTSP specifically, but the frozen trait
does not have them) — `RtspDemuxer::play`/`pause` are inherent methods
instead, reachable by a caller holding the concrete type.

`vaco_format_core::DemuxerDesc::open` takes exactly one already-opened
`MediaSource` and no URL/registry — there is no sensible bytes-already-
fetched value for `rtsp://`, unlike an HLS playlist. `RTSP_DEMUXER`'s
registered `open` always returns `Unsupported`, mirroring the reference's
own behaviour (measured: `ffmpeg -v debug -i rtsp://...` never logs a
generic `[tcp @ ...]` open the way `-i http://...` does — RTSP dispatches by
URL scheme before any generic protocol resolution). A caller recognising
`rtsp://` must call `RtspDemuxer::open` directly. `SDP_DEMUXER`'s registered
path is less broken (SDP bytes genuinely can be handed to it) but still
cannot open the UDP transports SDP names without a `ProtocolRegistry` —
`SdpDemuxer::open`'s `registry: Option<...>` parameter is the real path,
mirroring `HlsDemuxer::open`'s `access: Option<RemoteAccess>`.

## How to change it

* **Adding an RTSP option**: probe `ffmpeg -h demuxer=rtsp` first (D6/D7/D17);
  `options.rs`'s module doc records the full transcript this crate's
  defaults came from.
* **`rtsps://` (TLS)**: not implemented — this crate connects `tcp:` only.
  `connection::connect_tcp` is written so that swapping in
  `vaco_protocol_tls::connect::handshake` is a small, local change; see
  `options.rs`'s docs for why it was deferred this pass.
* **Multi-track UDP reads**: `read_udp`/`SdpDemuxer::read_packet` round-robin
  poll each track's socket with a short timeout rather than a real
  select()/epoll — `vaco_io::MediaSource` has no non-blocking-read primitive
  to build one on top of. Fine for a handful of tracks; a caller with many
  simultaneous tracks would want a real multiplexed I/O layer this crate
  does not have.

## Configuration

`RtspOptions` (`src/options.rs`): `initial_pause`, `rtsp_transport`,
`rtsp_flags`, `allowed_media_types`, `min_port`/`max_port`,
`listen_timeout`, `timeout`, `reorder_queue_size`, `buffer_size`,
`user_agent`.

## Dependencies

`vaco-hash` (MD5, D11-owned there), `vaco-protocol-core`,
`vaco-protocol-socket` (for the raw duplex TCP connect and the registered
`udp:` protocol — never `socket2` directly, D11), `vaco-format-core`,
`vaco-format-rtp`, `vaco-codec-core`, `vaco-packet`, `vaco-time` (NTP-style
timestamps and the HTTP-tunnel session cookie — never `std::time`, D18).

## wasm

**Not portable** (`xtask/src/wasm.rs`'s `NATIVE_ONLY`). RTSP's control
connection is inherently duplex, so — exactly like `vaco-protocol-tls` —
this crate connects its own `std::net::TcpStream` directly rather than
going through `Protocol::open`'s read-only shape, and its UDP transports go
through `vaco-protocol-socket`'s registered `udp:` protocol. Both pull in
`vaco-protocol-socket`, which is itself `NATIVE_ONLY` for depending on
`socket2` (measured: `E0583` building this crate for
`wasm32-unknown-unknown`, the same underlying wall one level removed). A
wasm build reaches RTSP through the host runtime's own duplex socket API, a
different transport behind the same seam, not a wasm build of this crate.
