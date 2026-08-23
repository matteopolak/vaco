# `vaco-protocol-wrap`

Layer 2. `cache:`, `subfile:`, `concat:`, `concatf:`, `tee:`, `async:`.

## What it is

Six protocols that share a crate because they share a shape: each one's whole
job is to change how *another* URL behaves, rather than to reach a transport
of its own.

| Protocol | Wraps | Changes |
|---|---|---|
| `subfile:` | one URL | exposes only `[start, end)` of it |
| `concat:` / `concatf:` | several URLs | reads them back to back as one stream |
| `cache:` | one URL | buffers it, so a forward-only source becomes seekable |
| `tee:` | several URLs | writes the same bytes to all of them |
| `async:` | one URL | reads it ahead of the caller, on a background thread |

## The inner-URL security rule

**Every inner URL is opened through the exact `ProtocolEnv` this crate's
protocol was itself given — never a fresh, unrestricted one.** There is no
additional confinement layered on top of that here, because `ProtocolEnv`'s
whitelist/blacklist/root/depth gate (owned by `vaco-protocol-core`, not this
crate) already *is* the confinement; reconstructing it would be exactly the
"reset privilege check" that crate's own module docs warn against.

The concrete, **measured** consequence (`ffmpeg 8.1`, not assumed) is the
fact the rest of the project needs, since HLS/DASH and anything else that
opens a URL out of a document another party wrote will need to reason about
the same gate:

> **None of these six protocols grants any default whitelist to what they
> open.** `ffmpeg -protocol_whitelist cache -i "cache:a.mkv"` is refused with
> `Protocol 'file' not on whitelist 'cache'!` — the caller must name `file`
> explicitly too, even though `cache:` cannot do anything without opening its
> inner URL. The same was measured for `concat:` (`-protocol_whitelist
> concat` alone still refuses the inner `file` open) and `subfile:`. This is
> the *opposite* of a playlist protocol's own preset: `hls`'s default grant
> (documented in `vaco-protocol-file`'s crate docs) is `http`, `https`,
> `tls`, `tcp`, `crypto` — deliberately excluding `file` (rule W3) — because
> `hls` *knows* what kind of URL a segment reference is. A generic wrapper
> like `cache:` or `concat:` does not know that, and the reference's answer
> is to grant nothing rather than guess.

Every `ProtocolDesc` in this crate sets `default_whitelist: &[]` for exactly
that reason. `nested_scheme: true` is still set on all of them — that flag
only says whether a `-protocol_whitelist`-style preset should look at this
protocol's own grants at all, and an empty grant is still a real, checkable
one, distinct from having none to check.

## How it works

### `subfile:` (`src/subfile.rs`)

Grammar: `subfile,,start,N,end,M,,:inner-url`. The comma-delimited prefix
lives in `Url::args` (rule S3 of `vaco-protocol-core`'s grammar) and is
genuinely odd — probed rather than guessed:

* Both doubled commas are mandatory. `subfile,start,0,end,10:x` (single
  commas) is refused by the reference with "Error parsing options string".
* `start`/`end` may appear in either order.
* `end` is an **exclusive** offset, not a length — `start,100,end,300` reads
  exactly the 200 bytes `[100, 300)`, verified byte-for-byte against a
  128-byte pattern file through `ffmpeg -f rawvideo`.
* `end` omitted, or its `AVOption` default of `0`, means "through EOF".
* `end < start` (once genuinely set) is refused: "end before start".
* `start` at or beyond the inner size yields an immediately empty read, not
  an error.

`parse_args` in `src/subfile.rs` implements this grammar directly;
`SubfileSource` then translates every position by `start` and clamps reads at
`end`.

### `concat:` / `concatf:` (`src/concat.rs`)

`concat:a.ts|b.ts|c.ts` splits on a **literal `|` with no escaping** — a file
named `x\|y.ext` was created on disk and `concat:x\|y.ext` still opened it as
two failing entries, `x\` and `y.ext`, proving the backslash is not an
escape.

`concatf:list.txt` reads the same list from a file, **one entry per line**
(not `|`-separated — a line containing `a|b` is one failing entry, not two),
each line **trimmed** of leading/trailing whitespace, with **no blank-line or
`#`-comment skipping** (an empty line is a real attempt to open an empty
path, and fails exactly the way opening `""` normally fails). A trailing
newline does not manufacture a spurious empty final entry — `read_list_file`
uses `str::lines`, which already has that property, rather than a manual
`split('\n')`.

`ConcatSource` reads its entries back to back, opening entry `k+1` only after
entry `k` reports EOF — but see "Deferred" below: every entry is opened
**eagerly**, at construction, not lazily as that sentence would otherwise
imply for how far ahead the *opens* happen.

### `cache:` (`src/cache.rs`)

`cache:inner-url`. The reference's one option, `read_ahead_limit` (bytes of
history retained; `-1` unlimited, default 65536, measured via `ffmpeg -h
protocol=cache`), is implemented here as a cap on the crate's own history
buffer rather than a look-ahead-specific window — see `CacheOptions`'s doc
comment for exactly what a fuller implementation would change.

`CacheSource` retains every byte read from `inner` (bounded by the option, via
`vaco_limits::Budget`, since an inner network source is untrusted-length
input). A backward seek into that history is free; a forward seek past it
reads and discards through the gap — the only way to reach a new position on
a transport that cannot itself seek. `seekability()` always reports `Cheap`,
which is the whole point of the protocol: from the caller's side, `Cheap` is
exactly the contract "any position is reachable, cheaply or by one linear
scan forward" describes.

### `tee:` (`src/tee.rs`)

`tee:out1|out2`, same literal-`|`-no-escaping grammar as `concat:`.
**This is the `tee:` *protocol*, not the `tee` *muxer*.** The reference has
both: `-f tee "a|[f=mpegts]b"` selects the muxer, which can send each output
through a *different container format* via the bracketed `[key=value]`
per-output options `vaco-protocol-core`'s own module docs show as an example
URL shape. A plain muxer writing to `tee:a.nut|b.nut` (measured: two
byte-identical files) uses the protocol instead, which duplicates raw bytes
verbatim with no format re-interpretation. The muxer belongs in a
`vaco-mux-tee` format crate — `vaco_protocol_core::Protocol` has no way to
express "re-encode this packet stream per output" — so `TeeSink` does not
parse bracketed options at all; a segment starting with `[` is opened as a
literal URL and fails through the ordinary "cannot resolve this" path.

`TeeSink::write` aborts the whole write on the first output that fails
(the muxer's own documented per-output failure tracking has no equivalent at
this layer to hang a "keep going" policy on); `is_seekable` is true only when
every wrapped sink is, since a seek only some outputs can honour would
desynchronise them.

### `async:` (`src/asyncproto.rs`)

`async:inner-url`. One worker thread owns `inner` outright and streams it
over a bounded `std::sync::mpsc` channel in fixed chunks; a second, single
slot command channel carries seeks from the caller to the worker (discard
in-flight chunks, re-seek `inner`, resume). Bounded, not unbounded, because
the entire point of "read ahead" is bounded look-ahead, not "buffer the whole
stream first".

**On `wasm32` there is no thread**, so `AsyncSource` falls back to reading
`inner` directly — same public API, same correctness, just no longer ahead of
the caller. Same shape as `vaco-sched::Driver`: one `#[cfg]`-gated function
(here, `spawn_reader`) is the only thing that calls `std::thread::spawn`, and
the fallback branch is not itself `cfg`'d out.

## Deferred: `concat:`/`concatf:` open every entry eagerly

`MediaSource` trait objects carry no lifetime, so a `ConcatSource` cannot
hold onto the `&ProtocolEnv<'_>` a later entry's open would need — only
`Protocol::open` has that borrow, for the duration of one call. Opening every
entry up front, inside `open`, sidesteps the problem entirely, at the cost of
holding every entry's transport open for the whole concatenation's lifetime
rather than only the one currently being read. For file-and-pipe-shaped
inputs that is a handful of extra file descriptors; a list large enough for
it to matter would need a redesign carrying **owned** whitelist/root state
rather than a borrowed `ProtocolEnv` (an early draft of this attempted
exactly that — cloning `Option<&[&str]>` into owned `Vec<String>` and
rebuilding a borrowing `ProtocolEnv` per open — and the lifetime bookkeeping
it needed was judged not worth carrying for a first implementation). This is
real, scoped follow-up work, not an oversight to paper over.

## How to change it

* **Adding a `subfile:` grammar case**: probe it first — `ffmpeg -f rawvideo
  -pix_fmt gray -video_size <n>x1 -i "subfile,,...,,:pattern.bin" -f rawvideo
  out.raw` forces the exact byte range through unmodified for comparison
  against a Python-generated pattern file, which is how every case in
  `parse_args`'s tests was derived.
* **`cache:`'s retention is a `Vec<u8>`, not a ring buffer.** Fine for the
  `read_ahead_limit` scale this was measured against (64 KiB default); a
  caller that sets a very large limit and seeks around a lot will pay for
  `Vec` growth the way `DynBuf` (`vaco-io`) already does for the write side —
  see that type if this needs to become smarter.
* **`async:`'s chunk size (`CHUNK`) and queue depth (`QUEUE_DEPTH`) are
  constants, not options** — the reference exposes no equivalent `-h
  protocol=async` option (measured: empty), so there is nothing to bind a
  configured value to yet.
* **Gotcha — `TeeSink`/`ConcatSource` both split on literal `|` with no
  escape.** Do not add backslash-escaping "to be helpful"; it would diverge
  from the measured reference behaviour for a filename that contains `|`.

## Configuration

* `cache:`'s `read_ahead_limit` — see `CacheOptions`.
* Every other protocol in this crate has no options, matching `ffmpeg -h
  protocol=<name>` for `subfile` (whose `start`/`end` live in the URL's own
  comma-args, not as `-h`-visible options), `concat`/`concatf`, `tee` and
  `async`.

## Dependencies

`vaco-protocol-core` for the trait and the gate (not this crate's to
change), `vaco-io` for the source/sink traits, `vaco-limits` for `cache:`'s
budget-bounded history buffer, `vaco-opts` for `cache:`'s option schema.
`async:` uses `std::sync::mpsc` and, on every non-`wasm32` target,
`std::thread::spawn` directly inside one `#[cfg]`-gated function — **not**
routed through `vaco-time`, despite an earlier brief for this crate assuming
otherwise; see `asyncproto.rs`'s module docs for the correction and why
`xtask/src/time_gate.rs` is the authority here (it maps `std::thread::spawn`
to "a driver the caller supplies (D18)", and `vaco-time` itself has no spawn
wrapper at all).
