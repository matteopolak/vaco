# `vaco-protocol-file`

Layer 2. The `file:` and `pipe:` protocols.

## What it is

The two protocols everything else assumes exist, and the two ends of the
seekability model:

| | `file:` | `pipe:` |
|---|---|---|
| Seekability | `Cheap` | `None` |
| Size | known, re-stat'd every call | unknown |
| Reference case for | index building, two-pass reads | one-pass demuxing, `peek` on a stream |

They are built together because a change to the seekability model has to be
checked against both, and only one of them can be checked with a temp file.

## How it works

Neither type implements `MediaSource` directly. Both implement the thin
`vaco_io::RawSource` — one call per syscall — and are wrapped in
`vaco_io::PeekSource`, which supplies the peek window. Buffering happens once
more, higher up, in `IoContext`. Three layers, each with one job, and no byte
copied twice in the steady state.

### `file:`

`FileSource` wraps `std::fs::File`. `size()` re-stats on every call rather than
caching: a file being written grows, and a cached length would make `follow` stop
early.

`FileSink` is the write half. It is seekable, which is what the muxer
write-measure-patch pattern needs.

**URL spellings**, handled in `path::url_to_path`:

| URL | Path |
|---|---|
| `clip.mkv` | `clip.mkv` |
| `file:clip.mkv` | `clip.mkv` |
| `file:/tmp/clip.mkv` | `/tmp/clip.mkv` |
| `file:///tmp/clip.mkv` | `/tmp/clip.mkv` |
| `file://localhost/tmp/clip.mkv` | `/tmp/clip.mkv` |
| `file://evil.example/share/x` | **refused** |
| `C:\clip.mkv` | `C:\clip.mkv` (rule S4 kept it a bare path) |

A non-empty, non-`localhost` authority is refused rather than guessed at: a UNC
share is a network open wearing a local scheme, which is the exact confusion the
whitelist exists to prevent.

Percent-decoding is **not** performed. The reference tool does not decode `file:`
paths either, and a decoder here would make `%2e%2e` a traversal primitive.

### Rule U2 — root confinement

When a caller opens a URL it did not write — a `concat` list file, a local HLS
playlist — it names a root in `ProtocolEnv::root`, and the open is refused if the
target falls outside it.

Confinement is by **canonical** path, so a symlink pointing out of the root is
refused rather than followed. A relative name is anchored to the root before
resolution, so `../etc/passwd` is rejected instead of being resolved against the
process working directory. When the target does not exist yet (the create case),
the deepest existing ancestor is canonicalised and the remaining components are
checked by hand for `..`, `/` and a drive prefix — a component that does not
exist cannot be a symlink, but it can still climb out once created.

`root` is `None` for a path the user typed. Confining that would be a bug, not a
feature.

### `pipe:`

`pipe:` and `pipe:0` are stdin; `pipe:1` is stdout, `pipe:2` is stderr.

**`pipe:<n>` for any other descriptor is not supported, and cannot be.** Turning
an integer into an owned descriptor needs `FromRawFd::from_raw_fd`, which is
`unsafe` — justifiably, since nothing proves the integer names a descriptor this
process owns, and a wrong value closes somebody else's socket on drop. D2 forbids
`unsafe` outside `vaco-hw-*`, so those spellings return `Unsupported` with that
reason rather than opening something wrong. The same argument applies to plan 18
§2.4's separate `fd:` protocol.

## How to change it

* **Adding an option**: add a field to `FileOptions` with an `#[opt(...)]`
  attribute; parsing, help and serialisation follow from `vaco-opts`. Option
  *names* are interface facts (D9) and must match the reference tool.
* **Adding a protocol here**: `file` and `pipe` are the local, no-dependency
  pair. `data:`, `concat:`, `subfile:` and `cache:` are separate crates, not
  additions to this one — a demuxer must not be able to reach a concrete
  protocol type, and one crate per protocol is what keeps that checkable.
* **Gotcha — `confine` and the empty tail.** `Path::join("")` appends a trailing
  separator, which turns a regular file into a path the OS refuses to open. The
  no-tail case returns early for that reason.
* **Gotcha — `follow` polls.** A zero read with `follow` set means "not written
  yet", not "finished", so it sleeps `FOLLOW_POLL` and retries until
  `rw_timeout` (default 5 s). Polling rather than blocking is what keeps
  cancellation bounded by 10 ms instead of by the writer.
* **Gotcha — `list_dir` uses `symlink_metadata`.** A listing reports a link as a
  link rather than describing whatever it points at. Entries are sorted by name,
  because output order is a differential-test surface.

## Registration

Until this crate's `vaco-component.toml` was added, `file:` and `pipe:` were
**not reachable through `vaco-registry`** — the fragment simply did not
exist, so neither name appeared in `generated.rs`'s `PROTOCOLS` table despite
the implementation being complete. Both are registered unconditionally (no
`feature =`/`default = false`): a build with no `file:` protocol cannot open
a bare path, which is rule U1's default scheme for every path the user types
without a scheme at all.

## Configuration

`FileOptions`, parsed from the option dictionary handed to `open`/`create`:

| Option | Type | Default | Meaning |
|---|---|---|---|
| `truncate` | bool | `true` | Truncate an existing output file. Ignored when `IoFlags::append` is set. |
| `blocksize` | i32 | `0` | Suggested buffer size; `0` means the `IoContext` default. |
| `follow` | bool | `false` | Keep reading a file that is still being written. |

`pipe:` has no options.

`ProtocolEnv` supplies the rest: `root` (rule U2), `cancel` (checked by `follow`),
and `rw_timeout` (the `follow` deadline).

The peek window of the returned source is bounded by `Limits::strict()`. That is
not the probe policy — the real probe path goes through `IoContext`, whose limits
the caller sets — it is a floor so a direct `MediaSource::peek` on a raw source
cannot be unbounded.

## Dependencies

* `vaco-core` — `Error`.
* `vaco-io` — `RawSource`, `PeekSource`, `MediaSink`, `WriterSink`,
  `ReaderSource`, `CancelToken`.
* `vaco-protocol-core` — `Protocol`, `ProtocolDesc`, `ProtocolEnv`, `Url`.
* `vaco-opts` — the `FileOptions` schema.
* `tempfile`, `proptest` (dev).

`std::fs`, `std::io` and `std::os::unix::fs::symlink` (tests only) are the only
OS surface; D14.3 permits `std` everywhere. `#![forbid(unsafe_code)]`, no
external crates, no FFI.
