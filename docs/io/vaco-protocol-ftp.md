# `vaco-protocol-ftp`

Layer 2. `ftp:` — RFC 959 (control connection, login, passive mode) plus
RFC 2428 (`EPSV`).

## What it is

`ftp://[user[:pass]@]host[:port]/path` logs in (anonymous by default),
probes seekability and size, opens a passive data connection, and transfers
`path` with `RETR` (read, `Protocol::open`) or `STOR` (write,
`Protocol::create`).

## How it works

### Measured command sequence

Captured against a fake FTP server built for this purpose (`tests/
fake_server.rs`, and an earlier standalone Python version used while
measuring), since no real FTP server is reachable from this environment.
Both directions send the identical setup sequence before the first byte of
data:

```text
(connect)                 -> 220 <greeting>
USER <user>                -> 331
PASS <password>            -> 230
TYPE I                      -> 200
FEAT                         -> 211  (response read, not otherwise used)
PWD                           -> 257  (response read, not otherwise used)
REST 0                        -> 350
SIZE <path>                    -> 213 <n>
EPSV                            -> 229 (|||port|)   [falls back to PASV on refusal]
RETR <path>  /  STOR <path>      -> 150, then the data connection, then 226
```

Two things worth calling out:

1. **`<path>` is always the full path from the URL, unmodified.** Measured:
   requesting `ftp://host/pub/file.bin` sends `SIZE /pub/file.bin` and `RETR
   /pub/file.bin` directly — no `CWD` is ever issued, even though the path
   has a directory component. This crate therefore does not implement
   `CWD`-relative navigation at all; it is a scoping decision, not an
   oversight, since nothing measured needed it.
2. **`EPSV` is tried before `PASV`, with a genuine fallback.** A server that
   answers `500` to `EPSV` gets a `PASV` retry, and the address `PASV`
   returns is the one actually used — confirmed with a fake server that
   fails `EPSV` deliberately.

### Login defaults

`user` resolves from the URL's userinfo, then `-ftp-user`, then
`anonymous`. `password` resolves from the URL's userinfo, then
`-ftp-password`, then — only when the resolved user is `anonymous` —
`-ftp-anonymous-password`, then the reference's own measured default: the
literal string `nopassword` (not an email address, despite `-h
protocol=ftp`'s own help text for `-ftp-anonymous-password` suggesting one).

### `-h protocol=ftp` (ffmpeg 8.1)

```text
ftp AVOptions:
  -timeout           <int>        ED......... set timeout of socket I/O operations (from -1 to INT_MAX) (default -1)
  -ftp-write-seekable <boolean>    E.......... control seekability of connection during encoding (default false)
  -ftp-anonymous-password <string>     ED......... password for anonymous login. E-mail address should be used.
  -ftp-user          <string>     ED......... user for FTP login. Overridden by whatever is in the URL.
  -ftp-password      <string>     ED......... password for FTP login. Overridden by whatever is in the URL.
```

`-protocols` lists `ftp` under both `Input:` and `Output:`.

### Security: two nested `tcp:` opens per session, checked once

Both the control connection and every data connection dial their own duplex
`std::net::TcpStream` directly rather than through
`vaco_protocol_core::ProtocolRegistry` — the control connection needs a
duplex round trip per command (the same reasoning as
`vaco-protocol-tls`/`vaco-protocol-httpproxy`: `Protocol::open`/`create` each
return only one direction). `default_whitelist` is measured empty:

```text
$ ffmpeg -protocol_whitelist ftp -i ftp://127.0.0.1:PORT/pub/file.bin -f null -
[tcp @ ...] Protocol 'tcp' not on whitelist 'ftp'!
```

**One consequence worth being explicit about**: `env.check_scheme("tcp")` is
called exactly once, when `FtpSource`/`FtpSink` is constructed — not again
when [`MediaSource::seek`]/[`MediaSink::seek`] reopens a fresh data
connection later. `vaco_io::MediaSource`/`MediaSink`'s `seek` has no `env`
parameter (the trait predates any nested-open need on this path), so there
is no later whitelist to re-check against. The scheme a session's data
connections use never changes (`"tcp"`, always), so this reuses one already-
granted decision for the life of the session rather than fabricating a new
environment — see `src/source.rs`/`src/sink.rs`'s module docs for the full
reasoning.

## How to change it

- `src/control.rs` — the `Session` type: login, `TYPE`/`FEAT`/`PWD`,
  `REST`/`SIZE`, `EPSV`/`PASV` parsing (`parse_pasv`/`parse_epsv`, pure and
  fuzzed directly).
- `src/source.rs`/`src/sink.rs` — `RETR`/`STOR` over the negotiated data
  connection, and seek (abort + `REST` + reopen).
- `src/protocol.rs` — URL parsing (`parse_url`, pure and fuzzed directly),
  credential resolution, the `Protocol` impl, and the registry descriptor.

## Configuration

`-timeout`, `-ftp-write-seekable`, `-ftp-anonymous-password`, `-ftp-user`,
`-ftp-password` — see "`-h protocol=ftp`" above.

## Dependencies

`vaco-protocol-socket`, for `HostPort` and `addr::connect` (both connections
this crate opens use it directly — see "Security" above).

## Testing — what is measured, and what is not

- `control::tests::*` — `PASV`/`EPSV` address parsing, including
  `oversized_pasv_fields_do_not_overflow` (see "Fuzzing" below).
- `protocol::tests::*` — URL parsing and the credential-precedence rules,
  each with a dedicated test per precedence tier.
- `tests/fake_server.rs` — end to end through the real `Protocol::open`/
  `create` path against an in-process fake control+data server: a full
  `RETR` via `EPSV`, the `EPSV`-refused-so-`PASV` fallback, a full `STOR`,
  and the whitelist boundary.

**Untested, and why:** there is no real FTP server reachable from this
environment, so nothing here has been checked against a specific real
server's quirks — non-conforming greeting text, servers that require `CWD`
even for absolute paths, `REST` support gaps, active (`PORT`) mode (not
implemented at all: only passive), or `MDTM`/other extended commands this
crate never sends. `MediaSource::seek`'s abort-and-reopen sequence sends a
fresh `REST`/`EPSV or PASV`/`RETR` without an explicit `ABOR`; the fake
server accepts this because it does not track transfer state across
commands, so the client-side sequence is exercised but a strict real server
requiring `ABOR` before a new transfer is not. `FtpSink::seek` (mid-upload
resume) is similarly exercised only against the same permissive fake
server — real server-side `STOR`-after-`REST` semantics generally require
the server to already hold bytes up to that offset, which this crate cannot
verify without a real server.

## Fuzzing

`fuzz/fuzz_targets/protocol_ftp_parse.rs` feeds arbitrary bytes to
`protocol::parse_url`, `control::parse_pasv`, and `control::parse_epsv` — all
pure, I/O-free. **Found a real bug on the first run**: `parse_pasv` parsed
each comma-separated field as `u16`, so a malicious `227` response supplying
a field above 255 (e.g. `77777`) parsed cleanly and then overflowed
computing `p1 * 256` under the fuzz profile's overflow checks. Fixed by
parsing each field as `u8` (every field of a genuine response is one byte),
which makes the invalid class unrepresentable rather than merely checked
after the fact. Regression: `control::tests::oversized_pasv_fields_do_not_overflow`,
and the crash input is kept at `fuzz/seeds/protocol_ftp_parse/`. Re-run after
the fix: 6,962,160 execs in 30s, exit 0, `fuzz/artifacts/protocol_ftp_parse/`
empty.
