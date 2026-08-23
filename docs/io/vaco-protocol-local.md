# `vaco-protocol-local`

Layer 2. The `data:` and `md5:` protocols.

## What it is

Two small, unrelated local protocols, plus the base64 codec `data:` needs.
Neither wraps another URL as its primary job (`md5:` opens one nested URL for
its destination, but that is incidental to what it does), so neither belongs
in `vaco-protocol-wrap`.

* `data:` — RFC 2397 data URLs. Read-only.
* `md5:` — discards every byte written to it and emits an MD5 digest of the
  whole stream when writing finishes. Write-only.

**`fd:` is not here.** Plan 18 §2.4 originally scoped it into this PR; **D16**
(`planning/00-decisions.md`) later found that estimate assumed an `unsafe`
escape hatch D2 does not grant — turning an integer into an owned file
descriptor needs `FromRawFd::from_raw_fd`, and nothing proves the integer
names a descriptor this process actually owns. D16's decision is that `fd:`
is not implemented at all, project-wide, not even restricted the way `pipe:`
is restricted to descriptors 0/1/2 in `vaco-protocol-file`.

## How it works

### `data:` (`src/data.rs`, `src/base64.rs`)

Grammar: `data:[<mediatype>][;base64],<payload>`. Three things measured
against `ffmpeg 8.1` are **not** what RFC 2397 says, and this crate matches
the measurement rather than the RFC (D6/D7 — the reference's behaviour is the
fact, its name for a thing is not):

| RFC 2397 says | Measured (`ffmpeg -i "data:..."`) |
|---|---|
| Non-base64 payload is URL-encoded | **No percent-decoding at all.** `data:text/plain,hello%20world` yields the literal bytes `hello%20world`. |
| `;base64` is a flag | It is a **literal, case-sensitive** token. `;BASE64` is not recognised; the payload is then read as literal bytes. |
| Media type defaults to `text/plain;charset=US-ASCII` when omitted | The reference does default an *omitted* type (`data:,hello` works), but a **present, malformed** one is refused: `data:x,hello` -> `Invalid content-type 'x'`. |

The content-type rule that reproduces every probed case: **if the text before
the first comma (`header`) is non-empty, the part of it before the first `;`
must contain `/`.** An entirely empty header — the bare `data:,payload` form
— needs no type at all. `parse` in `src/data.rs` implements exactly this one
rule; see its doc comment for the full probe log (`data:;base64,...`,
`data:a;base64,...`, `data:foo/bar;base64,...`, etc.).

Base64 decoding (`src/base64.rs`) is hand-written — twelve lines of alphabet,
D10 makes a new dependency for it a reviewed decision — and **strict**,
matching the reference: no embedded whitespace, no URL-safe alphabet, no
unpadded input. `ffmpeg -i "data:audio/wav;base64,aGVsbG8"` (unpadded) and
`data:audio/wav;base64,aGVs bG8=` (embedded space) are both refused by the
reference, and [`Base64Error::BadLength`]/[`Base64Error::BadChar`] refuse the
same inputs here.

### `md5:` (`src/md5.rs`)

**Not the same thing as `-f md5`.** `vaco-mux-hash`'s `WholeHashMuxer::md5` is
a *muxer* and prints `MD5=<hex>\n`. `md5:` is a *protocol* — any muxer can
write through it — and measured output is different: a bare lower-case hex
digest with **no** `MD5=` label.

```text
$ ffmpeg -f lavfi -i testsrc=size=32x32:rate=1:duration=1 -f rawvideo md5:
8fbd8482c70a0669a30408f2219104ba
```

`md5:` with an empty `rest` writes the digest to standard output (measured
above); `md5:some/path` writes it to that path instead (measured with `-f nut
"md5:myoutput.md5"`).

`Md5Sink::write` only feeds the running hash — `vaco_hash::HashAlgo::Md5`,
the single owner of `md-5` under D11 — and never touches the real
destination. `flush` (called explicitly, or as a best-effort `Drop`
backstop, mirroring `IoWriter`'s own rationale) computes the digest exactly
once — `hasher: Option<RunningHash>`, `take()`n so a second `flush` is a
no-op rather than a re-emit or a panic, the same shape
`vaco-mux-hash::WholeHashMuxer::write_trailer` uses. `Md5Sink::is_seekable`
is `false`: a hash is order-sensitive, so silently accepting a seek and
continuing to feed bytes in write order would compute a digest that does not
match what a seeking muxer thinks it wrote.

## How to change it

* **Adding a `data:` divergence from the RFC**: probe it first (`ffmpeg -i
  "data:..."` with `-f rawvideo -pix_fmt gray -video_size <n>x1` forces the
  bytes through unmodified, which is what most of this crate's own probing
  used — see the git history / crate docs for the transcripts). Do not trust
  RFC 2397 text alone; three of its rules are simply not what the reference
  does.
* **`md5:`'s destination is a nested open, not a raw `std::fs::File`.** It
  goes through `env.registry.create(&url.rest, ...)` with the *same*
  `ProtocolEnv` this protocol was given (see "Security" below) — do not
  replace it with a direct filesystem call, which would silently drop root
  confinement and the whitelist gate for wherever the digest lands.
* **Gotcha — the base64 decode table is a `const fn` with an `#[allow(
  clippy::indexing_slicing)]`.** `slice::get`/`get_mut` are not yet
  const-stable on this toolchain, so the usual indexing-free style is
  unavailable inside it; both indices are provably in bounds (64-entry
  alphabet, 256-entry table), and the reason is recorded on the `allow`.

## Configuration

Neither protocol has a `-h protocol=<name>` option set (measured:
`data`/`md5` are not even known to `ffmpeg -h protocol=...` — they take no
options at all).

## Dependencies

`vaco-hash` for `md5:`'s digest (D11 — this crate must not declare `md-5`
itself; `cargo xtask owner-gate` enforces it), `vaco-io` for the
`MediaSource`/`MediaSink` traits, `vaco-protocol-core` for the trait, the URL
grammar and the whitelist gate. No new external dependency.

## Security

Neither protocol opens a further URL on its own initiative from untrusted
input. `data:`'s entire content is the URL string itself — there is nothing
nested to gate, which is why its `default_whitelist` is empty and
`nested_scheme` is `false`. `md5:`'s one nested open (its destination) is a
path the *caller* wrote as part of the same URL, not something read out of a
document `md5:` parsed — but it still routes through the `ProtocolEnv` it was
given rather than the OS directly, so a caller that *does* want to confine it
(rule U2) or gate it (the whitelist) can. See `vaco-protocol-wrap`'s crate
docs for the measured whitelist behaviour this project's wrapping protocols
share; `md5:`'s single nested open follows the same rule.
