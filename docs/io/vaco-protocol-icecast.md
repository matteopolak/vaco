# `vaco-protocol-icecast`

Layer 2. `icecast:` — the Icecast/SHOUTcast source-client protocol. No RFC;
a documented de-facto convention (Icecast's own "Source Client" docs
describe the server side of the same handshake), but every detail below was
measured against the reference client's wire behavior, not read from that
documentation — clean-room, per D6/D7/D17.

## What it is

`icecast://[user[:pass]@]host[:port]/mount` connects and pushes an audio
stream to an Icecast (or SHOUTcast) mount point: `SOURCE` (legacy) or `PUT`
(modern) plus `Ice-*` headers and HTTP Basic auth, then the raw stream bytes.
Output-only — there is no `icecast:` input in the reference.

## How it works

All of the following was measured against `ffmpeg 8.1`, using local fake TCP
servers (no real Icecast server is reachable here) — see
`tests/fake_server.rs`.

### Two wire shapes, chosen by `-legacy_icecast`

Legacy (`-legacy_icecast 1`): `SOURCE <path> HTTP/1.1`, headers, then the
body immediately — no `100-continue` wait. The reference's own debug log
shows this mode dials plain `tcp:` directly, with no nested `http:` protocol
involved at all.

Modern (the default): `PUT <path> HTTP/1.1` with `Expect: 100-continue`, and
the client genuinely blocks: a fake server that accepts the connection,
reads the full header block, and answers nothing never receives a body
within the capture window. The reference's debug log shows modern mode
routed through its internal `http:`/`https:` protocol — a C implementation
detail of *how* it issues the request, not a grant this crate's own
`default_whitelist` needs to mirror (see below).

### Default port is `80` (`443` under `-tls 1`) — not the conventional
Icecast port `8000`

```text
$ ffmpeg -v debug -f lavfi -i sine -f mp3 icecast://127.0.0.1/mount
[tcp @ ...] Address 127.0.0.1 port 80

$ ffmpeg -v debug -f lavfi -i sine -f mp3 -tls 1 icecast://127.0.0.1/mount
[tcp @ ...] Address 127.0.0.1 port 443
```

Consistent with modern mode's internal routing through `http:`/`https:`: it
inherits *that* protocol's default port. Not something the scheme's name or
Icecast server conventions would have suggested — this crate's
`parse_url(rest, tls)` takes the resolved `-tls` flag specifically so it can
pick the right default.

### Exact header order and omission rule

Captured verbatim with every optional field set:

```text
PUT /mystream.mp3 HTTP/1.1
User-Agent: MyAgent/1.0
Accept: */*
Expect: 100-continue
Connection: close
Host: 127.0.0.1:19502
Content-Type: audio/mpeg
Icy-MetaData: 1
Ice-Name: MyStream
Ice-Description: A test stream
Ice-URL: http://example.com
Ice-Genre: Rock
Ice-Public: 1
Authorization: Basic c291cmNlOmhhY2ttZQ==
```

`Expect` is omitted in legacy mode; every other line and its position is
identical between the two modes. `Ice-Name`/`Ice-Description`/`Ice-URL`/
`Ice-Genre` are each omitted **entirely** (not sent empty) when their option
is unset — measured by unsetting one at a time and confirming only that one
line vanishes. `Ice-Public` and `Icy-MetaData` are always present regardless
of options.

### Auth

URL userinfo overrides `-password` outright — measured via the reference's
own debug line, `Overwriting -password <pass> with URI password!`, logged
when both are given. The username defaults to the literal `source` when the
URL has no userinfo — measured by base64-decoding the `Authorization` header
in that case. There is no `-user`-shaped option; only `-password`.

### Direction and options

`-protocols` lists `icecast` only under `Output:`. `-h protocol=icecast`
shows every option `E`-flagged (encoding/write) only, confirming the
direction independently. `Protocol::open` is stubbed `Unsupported`, the same
pattern as `vaco-protocol-local`'s `md5:`.

### `default_whitelist` is empty

`[icecast @ ...] No default whitelist set` — the same shape as
`crypto:`/`tls:`/`httpproxy:`/`ftp:` in this workspace. A caller still needs
an explicit `tcp`/`tls` grant on top of `icecast` itself.

## Security

The `SOURCE`/`PUT` handshake is inherently duplex (write headers, then — for
modern mode — read a `100 Continue` before the body), which
`Protocol::create`'s one-direction return type cannot express — so the
connection is dialled through `vaco_protocol_dial::{dial_tcp, dial_tls}`
rather than the registry; both check the whitelist by hand before
connecting.

## How to change it

- `src/options.rs` — `IcecastOptions`, matching `-h protocol=icecast`
  exactly.
- `src/request.rs` — pure, I/O-free: `method` (SOURCE/PUT + the
  expect-continue flag), `build_headers`, `basic_auth`, `parse_status_line`.
  This is the module the fuzz target and most unit tests exercise directly.
- `src/protocol.rs` — `IcecastProtocol`, URL parsing (`parse_url`),
  credential resolution (`credentials`), and the registry entry. Dialing and
  the header-block read come from `vaco-protocol-dial`; `handshake` is the
  local glue around them.

**Gotcha:** `handshake`'s wait for `100 Continue` only recognizes exactly
that status; what the reference does when the server answers something else
immediately (e.g. an eager `401`) has not been captured against a real
Icecast server — see Testing below.

## Configuration

`-h protocol=icecast`: `ice_genre`, `ice_name` (becomes the `Ice-Name`
header despite its help text saying "set stream description" — the same
text `-ice_description` has; the header name is what a server sees, not the
help text), `ice_description`, `ice_url`, `ice_public`, `user_agent`,
`password`, `content_type`, `legacy_icecast`, `tls`.

## Dependencies

`vaco-protocol-socket` (`HostPort`, `addr::connect`) and
`vaco-protocol-tls` (`TlsOptions`, `connect::{connect_tcp, handshake,
TlsStream}`) — TLS handling is reused, not duplicated, the same as
`vaco-protocol-gopher`'s `gophers:`.

## Testing — what is measured, and what is not

- `request::tests::*` — header order, the four `Ice-*` omission rules,
  method/expect-continue selection, `basic_auth`'s worked example, and
  `parse_status_line`.
- `protocol::tests::*` — URL parsing (default ports, userinfo),
  credential-precedence, the empty `default_whitelist`, `open()`'s
  `Unsupported`, and whitelist denial.
- `tests/fake_server.rs` — end to end through the real `Protocol::create`
  path: modern mode waiting for and receiving `100 Continue` before sending
  the body, modern mode never sending the body when `100` never arrives,
  legacy mode sending the body immediately with no wait, and whitelist
  denial.

**Untested, and why:** there is no real Icecast (or SHOUTcast) server
reachable from this environment. Specifically untested against a real
server: what happens after the body is fully sent (the reference's final
success/failure status is read after the stream ends, which this crate's
`MediaSink`-shaped return never observes); what a non-`100` immediate
response during the modern-mode wait actually means for the reference (this
crate treats it as `ProtocolError::Malformed` and gives up rather than
inspecting the code, since no real server was available to characterize what
the reference itself does there); and the TLS (`-tls 1`) path, which is
exercised only indirectly through `vaco-protocol-tls`'s own handshake tests,
not end to end through this crate (mirroring `gophers:`'s same gap).

## Fuzzing

`fuzz/fuzz_targets/protocol_icecast_parse.rs` feeds arbitrary bytes to
`request::parse_status_line` and arbitrary UTF-8 to `protocol::parse_url`
(both TLS and non-TLS) — all three pure, I/O-free. 3,069,555 execs in 31s,
exit 0, `fuzz/artifacts/protocol_icecast_parse/` empty.
