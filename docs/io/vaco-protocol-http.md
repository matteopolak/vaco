# `vaco-protocol-http`

Layer 2. The `http:` and `https:` protocols.

## What it is

A `vaco_protocol_core::Protocol` implementation over a pure-Rust HTTP client
(`ureq`, with `rustls` and a pure-Rust crypto provider): ranged reads so
seeking a remote file does not download it from the start, redirects gated by
the same whitelist every nested protocol open goes through, custom headers
and user agent, cookies, ICY metadata request, HTTP Basic auth from URL
userinfo, and the `-reconnect*` family. It is the one crate on the wasm
`NATIVE_ONLY` list (see "Portability" below).

## How it works

Three layers, in increasing order of how OS-specific they are:

| Module | What it does | Portable? |
|---|---|---|
| `options` | `HttpOptions`, a plain data struct. | Yes |
| `url` | Building the outbound request target; resolving a `Location` header (absolute, protocol-relative, absolute-path, relative) against the request that produced it, per RFC 3986 §5. | Yes |
| `headers` | Assembling the default header set plus `-headers` overrides, from options and a byte range. | Yes |
| `parse` | Parsing bytes a *server* controls: `Content-Range`, `Retry-After`, the `-reconnect_on_http_error` list. This is the fuzz target's surface. | Yes |
| `reconnect` | Whether to retry a failure and how long to wait first. Takes/returns plain values, never sleeps itself. | Yes |
| `transport` | The `ureq::Agent`, TLS configuration, and turning `ureq::Error` into `ProtocolError`. | **No** — sockets and TLS. |
| `source` | `HttpSource`: ties the above into a `vaco_io::RawSource`, including the reconnect loop and the "server ignored my `Range`" safety net. | Mostly not (drives `ureq::Body`), but its only call into `transport` is one function. |
| `protocol` | `HttpProtocol`: the `Protocol::open` entry point, and the redirect-through-the-whitelist loop. | No. |

### Ranged reads

`HttpSource::adopt` classifies every response by status before handing a
caller a single byte:

* **`206` with a parseable `Content-Range`** — genuinely ranged. Position is
  taken from the header's own `start`, not assumed from what was requested.
* **`200` for a request at byte `0`** — the common "this server does not
  support `Range`" case. Adopted at position `0`, `seekability()` becomes
  `None` from then on. Measured directly: a local `http.server` with no Range
  support answers `ffprobe`'s probing `Range: bytes=0-` with a plain `200`,
  and `ffprobe` reads the whole file forward in one pass (`0 seeks`) rather
  than failing — see "Where the values came from" below.
* **`200` for a request at a non-zero offset** — the server ignored `Range`
  and is about to hand back bytes from the wrong position. Refused as an I/O
  error rather than silently consumed; see
  `tests/ranged_reads.rs`'s `seek_when_range_is_ignored_by_a_later_request_errors_instead_of_corrupting`.
* **A redirect or `4xx`/`5xx` reaching a seek/reconnect** — refused outright.
  There is no `ProtocolEnv` available at this layer to gate a new URL
  through (see "Security" below), so a mid-stream redirect is not followed.

`-short_seek_size` (default `0`, matching the reference) lets a small forward
seek read-and-discard on the existing connection instead of opening a new
one; the threshold is caller-configured, never attacker-controlled, so the
discard loop is bounded by a value the attacker cannot choose.

### Reconnection

`reconnect::decide` classifies a failure (`NetworkError`, `StreamDropped`,
`UnexpectedEof { total_known }`, `HttpStatus(code)`), checks it against the
matching `-reconnect*` flag, enforces `-reconnect_max_retries` and
`-reconnect_delay_total_max` (measured against **wall-clock elapsed since the
first failure**, via `vaco_time::Instant` — not a running sum of intended
waits, which would under-count time actually spent blocked or in a slow
connect), and returns a plain doubling backoff (`1s, 2s, 4s, …`, capped at
`-reconnect_delay_max`) honouring a server's `Retry-After` when
`-respect_retry_after` is set. `HttpSource::try_reconnect` is an explicit
loop, not recursion — `-reconnect_max_retries` accepts values up to
`i32::MAX`, and a persistently failing server must not turn that into one
stack frame per attempt.

### Redirects, and why they go through the whitelist

`ureq` is configured with `max_redirects(0)` (`transport::agent`) — it never
follows a redirect itself. On a `3xx` response, `HttpProtocol::open`:

1. Resolves the `Location` value into a full URL string (`url::resolve_location`,
   pure string logic, no trust decision).
2. Checks that string's scheme against `env` — the *same* `ProtocolEnv` this
   open was itself granted — via `ProtocolEnv::check_scheme`.
3. If the target is still `http`/`https`, continues this function's own
   loop, bounded by `-max_redirects`. Otherwise hands off to
   `ProtocolRegistry::open_parsed`, the one function every top-level open in
   the project goes through.

This is measured against the reference directly, not assumed: redirecting a
local test server's response to `file:///etc/passwd` and pointing `ffprobe`
at it produces, verbatim:

```
[http @ ...] Protocol 'file' not on whitelist 'http,https,tls,rtp,tcp,udp,crypto,httpproxy,data'!
http://127.0.0.1:PORT/x: Invalid argument
```

`crate::protocol::DEFAULT_WHITELIST` reproduces that exact list (D9:
interface facts are free — an unregistered scheme in the list is inert,
since `ProtocolRegistry::find` still reports `Unknown` for it).
`tests/redirect_whitelist.rs` reproduces the refusal against this crate,
entirely over loopback, including the "still works when permitted" and
"`-max_redirects 0` refuses the very first redirect" cases.

**Limit, stated plainly**: a redirect encountered mid-stream (a seek or
reconnect issuing a fresh request to the *same* already-resolved URL) is
never followed, because `HttpSource` has no `ProtocolEnv` to re-check it
against (a `ProtocolEnv<'a>` borrows the registry/cancel token with a
lifetime tied to the original `open` call, and `HttpSource` must outlive
that call). Redirects are resolved once, at open time; a resource whose
location changes between the initial connect and a later seek is treated as
a connection failure, not as an implicit trust extension.

### Where the values came from

Not from a plan and not from memory: from `ffprobe -h protocol=http` on the
pinned reference (8.1), and from `ffprobe -v debug http://…` against a local
`python -m http.server`-style script whose request log is readable — read as
black-box observed behaviour of a shipped binary, which is exactly what
D6/D7 permit. Specifically measured:

* The default request line and headers (`User-Agent: Lavf/…`, `Accept: */*`,
  `Range: bytes=0-`, `Connection: close`, `Icy-MetaData: 1`) — `Host` is not
  one of them; every HTTP/1.1 client derives it from the URI authority.
* `-offset 100 -end_offset 200` → `Range: bytes=100-199` (end is exclusive on
  the option, inclusive on the wire — see `headers::range_header_value`).
* `-cookies 'sessionid=abc123; path=/\nfoo=bar; path=/'` → a single
  `Cookie: sessionid=abc123; foo=bar` header (the pair before each line's
  first `;`, joined with `; `).
* `-headers`/`-user_agent`/`-multiple_requests`/`-icy` each flip exactly the
  header they claim to.
* A 404 → `Invalid data found when processing input`, exit `1`. A refused
  connection → `Connection refused`, exit `1`. Both mapped here to
  `ProtocolError::Io`, preserving the underlying `io::ErrorKind` where `ureq`
  itself preserves it.
* `-max_redirects 0` against a server that always redirects → exactly one
  request, then an error — a redirect response is itself a failure at that
  setting, not merely "no further hops attempted".

## How to change it

* **Adding an option**: add a field to `HttpOptions` with an `#[opt(...)]`
  attribute, matching `ffprobe -h protocol=http`'s declaration order and
  wording where one exists. Names are interface facts (D9).
* **The fuzz target's surface is `parse.rs` and `url.rs`, not `source.rs`.**
  `fuzz/fuzz_targets/protocol_http_response.rs` drives `parse_content_range`,
  `parse_retry_after_secs`, `parse_reconnect_codes`, `cookie_header`,
  `parse_header_block`, `resolve_location`, `remove_dot_segments`,
  `request_target` and `split_userinfo` — the parts that take server bytes
  (per the crate's own brief), never the socket. A new function that parses
  a header value belongs in `parse.rs` and should be added to the fuzz
  target's `Input` struct in the same change.
* **Gotcha — `remove_dot_segments`.** Implemented as RFC 3986 §5.2.4's own
  two-buffer algorithm rather than a segment stack; re-derive from the RFC
  text, not from memory, if it ever needs revisiting — the bare `.`/`..`
  cases and the "never pop below the root" case are exactly where a
  hand-rolled version goes wrong.
* **Gotcha — the crypto provider is chosen explicitly, not via `ureq`'s
  `rustls` feature.** That feature pulls `ring` (`rustls-no-provider` +
  `rustls-webpki-roots` + `unversioned_rustls_crypto_provider` avoids it —
  see "Dependencies"). Do not add the plain `rustls` feature to this crate's
  `ureq` dependency; `cargo xtask dep-gate` will catch it, but the fix is
  cheaper before that.
* **Gotcha — registering this crate's protocols as `default = true` in
  `vaco-component.toml` regresses `vaco-registry`'s own `wasm-check`.**
  `protocol-http` becoming a *default* optional dependency of
  `vaco-registry` pulls `ureq`/`rustls-rustcrypto` into `vaco-registry`'s
  wasm build, which is not on the `NATIVE_ONLY` list and fails on
  `getrandom`'s hard `wasm32-unknown-unknown` `compile_error!`. Measured:
  setting `default = false` in this crate's `vaco-component.toml` and
  re-running `cargo xtask gen-registry` fixes it, and is why that fragment
  is `default = false` rather than the generator's ordinary default.
* **Not implemented, deliberately**: `-http_proxy`, `-listen`/`-resource`/
  `-reply_code` (server mode), `-post_data`/`-content_type`/`-chunked_post`/
  `-send_expect_100` (POST bodies), `-request_size`/`-initial_request_size`
  (chunked readahead sizing), `-method` (this crate only ever issues `GET`),
  parsing the ICY metadata interleaved *in the body* (the `Icy-MetaData: 1`
  request header is still sent, for fidelity), and the `Retry-After`
  HTTP-date form (only the delay-seconds form is parsed). D5 scopes v0.1 to
  zero muxers, so nothing calls `Protocol::create` on this crate yet, and a
  correct proxy/server implementation is substantial enough to deserve its
  own review rather than riding along here. `Protocol::create` returns
  `Unsupported`.

## Configuration

`HttpOptions`, parsed from the option dictionary handed to `open`/`check`
(declaration order follows `ffprobe -h protocol=http`, for the entries this
crate implements):

| Option | Type | Default | Meaning |
|---|---|---|---|
| `seekable` | int (`-1`/`0`/`1`) | `-1` (auto) | Probe from the first response, or force. |
| `headers` | string | empty | `Key: Value\r\n`-separated block; can override a default header. |
| `user_agent` | string | `Vaco/<version>` | Overrides `User-Agent`. |
| `referer` | string | empty | Sets `Referer`. |
| `multiple_requests` | bool | `false` | `Connection: keep-alive` vs `close`. |
| `cookies` | string | empty | `Set-Cookie`-syntax lines → one `Cookie:` header. |
| `icy` | bool | `true` | Send `Icy-MetaData: 1`. |
| `auth_type` | int (`none`/`basic`) | `none` | HTTP Basic auth is sent whenever URL userinfo is present, regardless of this value — "none" is autodetect, not "never". |
| `offset` | i64 | `0` | Initial byte offset, folded into the first `Range`. |
| `end_offset` | i64 | `0` (unbounded) | Exclusive upper bound on the requested range. |
| `reconnect` | bool | `false` | Reconnect after a mid-stream drop. |
| `reconnect_at_eof` | bool | `false` | Reconnect at EOF short of a known total size. |
| `reconnect_on_network_error` | bool | `false` | Reconnect when the *connect* attempt fails. |
| `reconnect_on_http_error` | string | empty | Comma-separated status codes to reconnect on. |
| `reconnect_streamed` | bool | `false` | Reconnect a forward-only (unsized) stream at EOF. |
| `reconnect_delay_max` | int | `120` | Backoff cap, seconds. |
| `reconnect_max_retries` | int | `-1` (unlimited) | Attempt cap. |
| `reconnect_delay_total_max` | int | `256` | Cap on wall-clock time spent across every wait. |
| `respect_retry_after` | bool | `true` | Honour a numeric `Retry-After` on a reconnect-eligible response. |
| `short_seek_size` | int | `0` | Below this many bytes, a forward seek reads-and-discards. |
| `max_redirects` | int | `8` | Redirect hops permitted; `0` makes a redirect itself an error. |

`ProtocolEnv` supplies the rest: `rw_timeout` becomes a per-request global
timeout (connect + send + receive-headers, not the whole body read), and
every nested open — every redirect — is gated by `check_scheme`.

## Dependencies

Every adoption here is D14.2's decision, not a new one this crate makes; its
own contribution was wiring `ureq` to the crypto provider that decision
requires, rather than accepting `ureq`'s own `rustls` feature default
(`ring`).

| Crate | Gate 1 (pure Rust) | Gate 2 (licence) | Gate 3 (trusted) |
|---|---|---|---|
| `ureq` 3.x, features `rustls-no-provider` + `rustls-webpki-roots` (**not** `rustls` — that feature pulls `ring`) | Pass: no `-sys`, no build script compiling native code. | MIT OR Apache-2.0. | Widely used pure-Rust HTTP client; active. |
| `rustls` 0.23, `default-features = false`, `features = ["std", "tls12"]` | Pass, and carries no crypto provider of its own — the entire point of the feature choice above. | Apache-2.0 OR ISC OR MIT. | The de facto standard pure-Rust TLS library. |
| `rustls-rustcrypto` 0.0.2-alpha | Pass: every dependency it pulls (`aes-gcm`, `chacha20poly1305`, `p256`, `p384`, `rsa`, `ed25519-dalek`, `x25519-dalek`, `sha2`, `hmac`, `der`, `pkcs8`, `sec1`, `signature`, `rand_core`) is a RustCrypto pure-Rust crate. | MIT OR Apache-2.0. | **Honest caveat**: `0.0.2-alpha` does not clear D10's "adopted" bar on reputation alone. Taken up because D14.2 already decided this at the workspace level — `ring` and `aws-lc-rs`, rustls's two production providers, both vendor and compile C/assembly and fail Gate 1 — and it is the only *other* zero-FFI rustls provider. `cargo xtask dep-gate` denies `ring`/`aws-lc-rs`/`aws-lc-sys` by name in CI. Re-check this provider's maturity at every release. |
| `vaco-core`, `vaco-io`, `vaco-limits`, `vaco-opts`, `vaco-protocol-core`, `vaco-time` | — | — | Workspace crates. |

`webpki-roots` is **not** a direct dependency of this crate — it is reached
transitively through `ureq`'s `rustls-webpki-roots` feature, which builds
the default `RootCertStore`. `proptest` is a dev-dependency.

`std::net`/`std::io`/`std::thread::sleep` are the only OS surface beyond
`ureq` itself; D14.3 permits `std` everywhere. `#![forbid(unsafe_code)]`, no
`unsafe` anywhere in this crate.

## Portability (D18)

The one entry on `xtask/src/wasm.rs`'s `NATIVE_ONLY` list:
`wasm32-unknown-unknown` has no sockets, so `ureq`/`rustls` cannot compile
for it. A browser build reaches HTTP through `fetch` instead — a *different*
crate (`vaco-protocol-fetch`, not yet built) behind the same
`vaco_protocol_core::Protocol` trait, which is the D11 adapter rule working
as intended, not a hole in this crate's design.

`options`, `url`, `headers`, `parse` and `reconnect` are written with no
`ureq` types and no socket/TLS calls specifically so that sibling crate needs
only to replace `transport`/`source`/`protocol`'s socket-facing parts —
header assembly, redirect resolution, response parsing and reconnect policy
all carry over unchanged.

`vaco-component.toml` registers `http`/`https` with `default = false` (see
"Gotcha" above) rather than the generator's ordinary default — this is a
wasm-buildability concern for `vaco-registry`, not a patent one (contrast
D4's use of the same mechanism), and is worth revisiting if `vaco-registry`
itself ever grows its own native/wasm feature split.
