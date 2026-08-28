# `vaco-protocol-ipfs`

Layer 2. `ipfs:` and `ipns:` — fetch content-addressed (`ipfs:`) or
name-addressed (`ipns:`) IPFS data through an HTTP gateway. Input-only. IPFS
has a specification of its own, but nothing this crate encodes was read from
it — every detail below was measured against the reference client's wire
behavior, clean-room per D6/D7/D17.

## What it is

`ipfs://<CID>/path` (or `ipns://<name>/path`) resolves a gateway and opens
`<gateway>/ipfs/<CID>/path` (or `/ipns/...`) through the ordinary
`http:`/`https:` protocol, one level deeper through the same
`ProtocolEnv` — the same "nested open through the registry" shape as
`vaco-protocol-local`'s `md5:`. There is no duplex handshake here (unlike
`httpproxy:`/`ftp:`/`gopher:`/`icecast:` in this workspace): a gateway fetch
is a plain single request/response.

## How it works

All of the following was measured against `ffmpeg 8.1`, using explicit env
vars, a local fake HTTP server, and a temp `$HOME`/`$IPFS_PATH`.

### Gateway precedence and a genuine reference quirk

`-gateway` wins outright; then `$IPFS_GATEWAY`; then a `gateway` file under
`$IPFS_PATH`; then one under `$HOME/.ipfs`. Confirmed both by the
reference's own numbered help text and by its debug log skipping straight
past an unset source with no attempt to use it (`$IPFS_GATEWAY is empty.`
then falls through to `$IPFS_PATH`, with no partial attempt in between).

The `$IPFS_PATH`-based lookup has a real bug this crate reproduces
faithfully rather than fixing: the reference concatenates `$IPFS_PATH` with
the literal string `gateway`, **inserting no path separator**:

```text
$ env IPFS_PATH=/tmp/fake_ipfs ffmpeg -v debug -i ipfs://QmCid/x -f null -
[ipfs @ ...] The IPFS gateway file (full uri: /tmp/fake_ipfsgateway)
doesn't exist. Is the gateway enabled?
```

Only a `$IPFS_PATH` that already ends in `/` finds its file. The
`$HOME/.ipfs` fallback does **not** have this bug — the reference builds
that particular path itself, with a separator, and it works with a bare
(no trailing slash) `$HOME`. See `gateway::ipfs_path_gateway_file` vs
`gateway::home_gateway_file`.

A trailing `/` on a resolved gateway (from any of the four sources) is
stripped before use; a gateway file's trailing whitespace/newline is
trimmed.

### A CID is required before gateway discovery even starts

`ipfs://` with an empty path fails immediately with `A CID must be
provided.` — measured with *no gateway configured at all*, and the
reference's debug log shows no `$IPFS_GATEWAY is empty.` line in that case,
meaning the CID check happens strictly before gateway discovery is even
attempted. `open_generic` checks in the same order, and
`protocol::tests::empty_rest_is_refused_before_gateway_discovery` is written
specifically to tell the two possible orderings apart (by which
`Malformed.detail` comes back), not just to check that *some* error occurs.

### Wire shape

A raw-byte capture against a local fake HTTP server, gateway
`http://127.0.0.1:PORT`, url `ipfs://QmCid/video.mp4`:

```text
GET /ipfs/QmCid/video.mp4 HTTP/1.1
User-Agent: Lavf/<version>
Accept: */*
Range: bytes=0-
Connection: close
Host: 127.0.0.1:PORT
Icy-MetaData: 1
```

— exactly `vaco-protocol-http`'s own default GET request, confirming this
protocol does nothing more than rewrite the URL and hand it to `http:`/
`https:`. `ipns:` produces the identical shape with `/ipns/` instead of
`/ipfs/`.

### Direction, options, and `default_whitelist`

`-protocols` lists `ipfs`/`ipns` under `Input:` only. `-h protocol=ipfs` and
`-h protocol=ipns` report an identical single option, `-gateway <string>`
(`.D.`, decoding only) — confirming the direction independently.
`default_whitelist` is measured empty for both (`[ipfs @ ...] No default
whitelist set`), the same shape as `crypto:`/`tls:`/`httpproxy:`/`ftp:` in
this workspace; the reference's *internal* nested `http:` open carries its
own separate grant, a C implementation detail of the fetch, not something
this crate's own descriptor needs to mirror (same reasoning as `icecast:`'s
docs).

## How to change it

- `src/gateway.rs` — pure, I/O-free: `resolve` (the four-source precedence),
  `build_target`, `ipfs_path_gateway_file`/`home_gateway_file` (the file-path
  construction, including the reproduced bug). This is what the fuzz target
  and most unit tests exercise directly.
- `src/protocol.rs` — `IpfsProtocol`/`IpnsProtocol`, `discover_gateway` (the
  actual `std::env::var`/`std::fs::read_to_string` calls, handed to
  `gateway::resolve`), `open_generic` (the CID check, then discovery, then
  the nested `http`/`https` open), and the two registry entries.

**Gotcha:** if `$IPFS_PATH`-based discovery ever seems to silently miss a
real gateway file, check for a missing trailing slash on `$IPFS_PATH`
first — that is the reference's own bug, reproduced deliberately, not a bug
in this crate.

## Configuration

`-h protocol=ipfs` / `-h protocol=ipns`: `gateway` (`-gateway`) — identical
for both schemes. Also consulted, in order, when `-gateway` is unset:
`$IPFS_GATEWAY`, a `gateway` file under `$IPFS_PATH`, a `gateway` file under
`$HOME/.ipfs`.

## Dependencies

None beyond this workspace's own crates (`vaco-core`, `vaco-io`,
`vaco-opts`, `vaco-protocol-core`) — the actual fetch is delegated to
whatever `http`/`https` protocol is registered in the caller's
`ProtocolRegistry` at runtime, not linked in directly. `vaco-protocol-http`
is a dev-dependency only, for the end-to-end integration test.

## Testing — what is measured, and what is not

- `gateway::tests::*` — the four-source precedence (including that `-gateway`
  beats everything and the file sources are in the right order), trailing-
  slash/whitespace trimming, the `$IPFS_PATH` no-separator bug versus
  `$HOME/.ipfs`'s correct separator, and `build_target` for both schemes and
  a bare-CID (no path) URL.
- `protocol::tests::*` — both descriptors' empty `default_whitelist`,
  `create()`'s `Unsupported` for both schemes, and the CID-before-discovery
  ordering (checked by which specific error message comes back, not just
  that an error occurs).
- `tests/fake_server.rs` — end to end through the real `Protocol::open` path
  *and* the real `vaco-protocol-http`: a full content fetch for both `ipfs:`
  and `ipns:`, a gateway with a trailing slash not producing a double slash
  on the wire, refusal with no gateway configured, and whitelist denial of
  the nested `http` open.

**Untested, and why:** there is no real IPFS gateway reachable from this
environment (the fetch always goes through a local fake HTTP server
instead). The `$IPFS_GATEWAY`/`$IPFS_PATH`/`$HOME`-based discovery paths are
covered only by `gateway::resolve` directly with synthetic candidate
strings, never through a real `std::env::set_var`/filesystem round trip in
an integration test: `std::env::set_var` is `unsafe` on this edition, and
this crate (like the rest of the workspace) forbids `unsafe_code` even in
its test targets. Only the `-gateway` **option** path is exercised end to
end.

## Fuzzing

`fuzz/fuzz_targets/protocol_ipfs_parse.rs` feeds arbitrary UTF-8 to
`gateway::resolve`, `gateway::build_target` (both schemes), and
`gateway::ipfs_path_gateway_file`/`home_gateway_file` — all pure, I/O-free.
15,965,920 execs in 31s, exit 0, `fuzz/artifacts/protocol_ipfs_parse/`
empty.
