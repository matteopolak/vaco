# `vaco-protocol-gopher`

Layer 2. `gopher:` (RFC 1436) and `gophers:` (the same protocol over TLS —
no RFC of its own; it is the reference's own name, not this project's
invention).

## What it is

`gopher://host[:port]/<T><selector>` connects, sends `<selector>\r\n` (`<T>`,
the item-type character, is consumed by the client and never sent), and
reads (`Protocol::open`) or writes (`Protocol::create`) the raw bytes that
follow — one request, one reply, no further framing. `gophers:` is
identical except the connection is TLS.

## How it works

All of the following was measured against a local fake gopher server (no
real one is reachable here) — see `tests/fake_server.rs`.

### Only three item types are accepted, and the check happens after connecting

`gopher://host/1/menu` (item type `1`, a directory listing) fails with
`Gopher protocol type '1' not supported yet!` — but only *after* the TCP
connection succeeds, before anything is sent. Trying every RFC 1436 type
character found exactly three accepted: `5` (DOS binary archive), `9`
(binary file), `s` (sound file). Every other type this project's tool would
plausibly touch — including `0` (plain text) and the image types
(`g`/`h`/`I`) — is refused.

### The type is one character, not one path segment

`gopher://host/some/selector` (no explicit type; `some` up to the first `/`
is four characters) sends `/selector\r\n`, **not** `ome/selector\r\n`. The
rule: take the *first character* of the first path segment as the type,
discard the rest of that segment entirely, and the selector is everything
from the *next* `/` onward (inclusive), or empty if there is none. See
`selector::parse`'s doc comment for the worked examples this was measured
against.

### `default_whitelist` is genuinely non-empty

The first protocol found so far in this workspace where that is true —
`crypto`, `tls`, `httpproxy` and `ftp` are all empty by default.

```text
$ ffmpeg -v debug -i "gopher://127.0.0.1:PORT/9/x" -f null -
[gopher @ ...] Setting default whitelist 'gopher,tcp'

$ ffmpeg -v debug -i "gophers://127.0.0.1:PORT/9/x" -f null -
[gophers @ ...] Setting default whitelist 'gopher,gophers,tcp,tls'
```

Makes sense structurally: a gopher menu's entries point at further
gopher (or plain) resources, so the reference pre-grants exactly what a
gopher session could legitimately need next. An *explicit*
`-protocol_whitelist gopher` (W3: an explicit whitelist replaces rather than
unions the default) still refuses the nested `tcp` open — the same shape as
every other protocol's default grant under an explicit whitelist.

### Output: selector first, then raw bytes with no framing

Confirmed with a raw-byte capture: `gopher://host/9/out`, muxed input
`hello output data`, produced exactly `/out\r\nhello output data` on the
wire — the selector line, then the write stream continues straight through.

### Direction and options

`-protocols` lists `gopher` under both `Input:` and `Output:`. `-h
protocol=gopher` / `-h protocol=gophers` both report "Unknown protocol": no
private `AVOption`s (the same shape as `data:`/`md5:`).

### Security

The selector round trip is inherently duplex (write, then treat the
connection as one direction for the rest of its life) —
`vaco_protocol_core::Protocol::open`/`create` each return only one
direction, so, like `tls:`/`httpproxy:`/`ftp:` in this workspace, the
connection is dialled directly rather than through the registry.
`env.check_scheme` is called by hand for every scheme actually used: `"tcp"`
for `gopher:`; `"tls"` then `"tcp"` for `gophers:`, reusing
`vaco_protocol_tls::connect::{connect_tcp, handshake}` rather than
duplicating TLS handling — `connect_tcp` does its own `"tcp"` check
internally, so `gophers:`'s dial checks both schemes without this crate
repeating either.

## How to change it

- `src/selector.rs` — `parse` (type + selector splitting, pure, fuzzed
  directly), `split_authority`, `check_type`.
- `src/protocol.rs` — `GopherProtocol`/`GophersProtocol`, the shared
  `send_selector`/`open_generic`/`create_generic` helpers (generic over any
  `Read + Write + Send` transport, so the TCP and TLS paths share one
  implementation), and the two registry descriptors.

## Configuration

No options for either scheme.

## Dependencies

`vaco-protocol-socket` (`HostPort`, `addr::connect`) and `vaco-protocol-tls`
(`TlsOptions`, `connect::{connect_tcp, handshake, TlsStream}`) — `gophers:`
reuses TLS handling rather than duplicating it.

## Testing — what is measured, and what is not

- `selector::tests::*` — the type/selector split algorithm against the
  exact transcripts above, and the type whitelist.
- `protocol::tests::*` — both descriptors' `default_whitelist` values and
  the port default.
- `tests/fake_server.rs` — end to end through the real `Protocol::open`/
  `create` path: a full read, an unsupported-type refusal (connection
  accepted, nothing sent), and a full write.

**Untested, and why:** there is no real gopher server reachable from this
environment, and `gophers:` specifically has no TLS-capable fake server in
this crate's own test suite (`tests/fake_server.rs` only stands up plain
TCP) — its TLS path is exercised only indirectly, through
`vaco-protocol-tls`'s own handshake tests, not end to end through this
crate. Whether a real gopher server ever sends anything before the client's
selector (this crate assumes not, matching RFC 1436's model) is also
unverified against a real implementation.

## Fuzzing

`fuzz/fuzz_targets/protocol_gopher_parse.rs` feeds arbitrary bytes to
`selector::split_authority` and `selector::parse` — both pure, I/O-free.
3,182,504 execs in 30s, exit 0, `fuzz/artifacts/protocol_gopher_parse/`
empty.
