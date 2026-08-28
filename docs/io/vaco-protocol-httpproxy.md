# `vaco-protocol-httpproxy`

Layer 2. `httpproxy:` — an HTTP `CONNECT` tunnel to a proxy.

## What it is

`httpproxy://[user:pass@]proxy-host:proxy-port/target-host:target-port`
connects to the proxy, issues an HTTP `CONNECT` request for the target, and
hands back the raw tunnelled TCP connection as a `MediaSource` (`open`) or
`MediaSink` (`create`). Everything after the handshake is an uninterpreted
byte pipe — a caller speaking HTTP through the tunnel (the usual case) does
so itself.

## How it works

### Measured request/response shape

No live proxy is reachable from this environment, so the request/response
shape was captured against a local loopback `TcpListener` standing in for a
proxy — a legitimate substitute here because the property under test is
*what bytes this crate sends and how it parses what comes back*, not
anything a real proxy's specific implementation would add. Captured with
`ffmpeg 8.1`:

```
$ ffmpeg -v debug -i "httpproxy://127.0.0.1:18080/example.com:80" -f null -
```

sent, over a fresh TCP connection to `127.0.0.1:18080`:

```
CONNECT example.com:80 HTTP/1.1\r\n
Host: 127.0.0.1:18080\r\n
Connection: close\r\n
\r\n
```

Two things worth flagging because they are easy to get "obviously" wrong:

1. **`Host:` names the proxy, not the tunnel target.** Naming the target in
   both `CONNECT` and `Host:` is the more common shape among HTTP proxy
   clients generally; the reference does not do that here.
2. **No `Proxy-Authorization` on the first attempt**, even with `user:pass@`
   in the URL. It appears only after a `407` whose `Proxy-Authenticate`
   names `Basic`, and — measured directly, two separate `Starting connection
   attempt` lines in `-v debug` — on a **fresh TCP connection**, not a second
   request on the first one:

```
C: CONNECT example.com:80 HTTP/1.1 / Host: ... / Connection: close
S: 407 Proxy Authentication Required / Proxy-Authenticate: Basic realm="x"
    (connection closed)
C: (new connection) CONNECT example.com:80 HTTP/1.1 / Host: ... /
   Connection: close / Proxy-Authorization: Basic dXNlcjpwYXNz
S: 200 Connection established
```

`dXNlcjpwYXNz` is `base64("user:pass")`, standard RFC 4648 §4 alphabet.

### Direction and whitelist

`ffmpeg -hide_banner -protocols` lists `httpproxy` under both `Input:` and
`Output:`. `ffmpeg -v debug` prints `[httpproxy @ ...] No default whitelist
set`, and an explicit `-protocol_whitelist httpproxy` alone still refuses the
nested `tcp` connection — `default_whitelist: &[]`, the same shape as every
other nested-opening protocol measured in this workspace.

`-h protocol=httpproxy` reports "Unknown protocol": no private `AVOption`s at
all (the same shape as `data:`/`md5:`), so `options: None`.

### Why the nested `tcp:` open bypasses the registry

Same reasoning as `vaco-protocol-tls` (see that crate's docs for the fuller
argument): the `CONNECT` handshake needs to write a request and read a
response on the *same* connection before there is anything to hand back to a
caller, and `vaco_protocol_core::Protocol::open`/`create` each return only
one direction. `connect::dial` calls `env.check_scheme("tcp")` by hand,
exactly where `ProtocolRegistry::resolve` would have, so the whitelist
property still holds.

### Response parsing is deliberately not `BufReader`

`read_header_block` reads one byte at a time rather than through a
`BufReader`, because the same `TcpStream` this function reads from is handed
back as the tunnel on success. A `BufReader` that read ahead past the header
block would silently strand any tunnel bytes a fast peer pipelined right
behind `200 ...\r\n\r\n` in the same TCP segment — a real correctness gap,
not a hypothetical one, since nothing downstream would ever see those bytes.

## How to change it

- `src/connect.rs` — URL parsing (`parse`), the request line (`request_line`),
  response parsing (`parse_response`, pure and fuzzed directly), the byte-at-
  a-time header reader (`read_header_block`), and the connect-with-retry
  state machine (`dial`).
- `src/protocol.rs` — the `Protocol` impl and `HTTPPROXY_PROTOCOL` descriptor.

## Configuration

No options. The proxy address, target address, and (optional) `user:pass`
all come from the URL.

## Dependencies

`vaco-protocol-socket`, for `HostPort` and `addr::connect` — the same
address-resolution and TCP-dial logic `tcp:`/`tls:` use, not duplicated here.

## Testing — what is measured, and what is not

- `request_line_names_the_proxy_in_host_not_the_target`,
  `request_line_with_auth_matches_the_measured_retry`,
  `base64_matches_the_measured_worked_example` — exact byte-for-byte
  transcripts from the capture above.
- `tests/loopback.rs`'s `dial_completes_against_a_local_listener_that_answers_200`,
  `dial_retries_with_auth_after_a_407_basic_challenge` — the full state
  machine against a real loopback `TcpListener`, asserting both the bytes
  sent and the two-separate-connections shape of the retry. Kept as an
  integration test rather than an inline `src/connect.rs` module because
  their `std::thread::spawn` (the listener's accepting side) would otherwise
  fall inside `cargo xtask time-gate`'s scan of shipped `src/` files.
- `dial_is_denied_without_tcp_on_the_whitelist` — the whitelist boundary.

**Untested, and why:** there is no real HTTP proxy reachable from this
environment, so nothing here has been checked against a *specific* real
implementation's quirks (e.g. non-conforming status lines, chunked or
otherwise unusual `407` bodies, Digest rather than Basic challenges — this
crate does not implement Digest at all, since it was never observed). The
loopback-listener tests substitute for "does this crate speak the protocol
correctly" but cannot substitute for "does a specific real proxy accept it".

## Fuzzing

`fuzz/fuzz_targets/protocol_httpproxy_parse.rs` feeds arbitrary bytes to
`connect::parse` (URL parsing) and `connect::parse_response` (response
header parsing) — both pure, I/O-free functions, so no network or process
spawn is involved. 12,860,157 execs in 30s, exit 0, `fuzz/artifacts/
protocol_httpproxy_parse/` empty.
