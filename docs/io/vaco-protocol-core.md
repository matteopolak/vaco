# `vaco-protocol-core`

Layer 2. The `Protocol` trait, the URL grammar, and the whitelist gate.

## What it is

Three things that only make sense together:

1. **`split_url`** — a URL splitter for a grammar that is deliberately *not*
   RFC 3986, because the reference tool's is not either.
2. **`Protocol` / `ProtocolDesc` / `ProtocolRegistry`** — stateless transports,
   reachable by scheme.
3. **`ProtocolEnv`** — the capability a nested open must carry, and the gate that
   decides whether it is allowed.

The third is why this is a crate rather than a module. A playlist chooses its own
URLs, so opening one is a **privilege decision**, not plumbing.

## How it works

### The URL grammar

FFmpeg's URL space is a superset of RFC 3986 with several format-specific
escapes. A parser that normalises them away silently changes which file gets
opened, so we split URLs ourselves and use RFC 3986 parsing only *inside* the
protocols that genuinely speak it (http, ftp, rtsp).

`split_url` produces:

```rust
pub struct Url {
    pub scheme: Option<String>,   // None means a bare path, which means `file`
    pub nested: Option<String>,   // the inner half of `outer+inner:`
    pub args: String,             // the `name,a,b,c:` private prefix, separator included
    pub rest: String,             // everything after the terminating `:`, uninterpreted
    pub inline_opts: Dict,        // empty unless `take_inline_opts` was called
}
```

#### Rules, in order

| # | Rule |
|---|---|
| **S1** | No `:` before the first `/` — a bare path. `scheme` is `None`. |
| **S2** | Scheme name is `[A-Za-z][A-Za-z0-9+.-]*`, terminated by `:` or by `,`. |
| **S3** | Terminated by `,`: everything up to the next `:` becomes `args`, the protocol's own private prefix. |
| **S4** | A one-letter name followed by `:/` or `:\` is a Windows drive letter, not a scheme. Handled here so no nested protocol has to re-handle it. |
| **S5** | A `+` inside the name splits outer from inner. First `+` only. |
| **S6** | Everything after the terminating `:` is `rest`. Parsed by the protocol, never here. |
| **U1** | The default scheme is `file` and **only** `file`. No configuration makes a bare path reach the network. |
| **U2** | A `file` open never follows a symlink out of an explicitly restricted root. Enforced in `vaco-protocol-file`; the root travels in `ProtocolEnv::root`. |

The parse is **total**: every string is a valid URL, because a string matching
nothing is a relative path, and refusing to open a file because its name is
strange is worse than opening it.

#### Reference table

| Input | scheme | nested | args | rest |
|---|---|---|---|---|
| `clip.mkv` | *(none → `file`)* | — | | `clip.mkv` |
| `/abs/clip.mkv` | *(none → `file`)* | — | | `/abs/clip.mkv` |
| `dir/weird:name.mkv` | *(none → `file`)* | — | | `dir/weird:name.mkv` |
| `C:\videos\clip.mkv` | *(none → `file`)* | — | | `C:\videos\clip.mkv` |
| `file:clip.mkv` | `file` | — | | `clip.mkv` |
| `pipe:1` | `pipe` | — | | `1` |
| `http://host/path?a=b` | `http` | — | | `//host/path?a=b` |
| `data:audio/wav;base64,UklGR…` | `data` | — | | `audio/wav;base64,UklGR…` |
| `concat:a.ts\|b.ts\|c.ts` | `concat` | — | | `a.ts\|b.ts\|c.ts` |
| `tee:out1.mkv\|[f=mpegts]out2.ts` | `tee` | — | | `out1.mkv\|[f=mpegts]out2.ts` |
| `crypto+file:secret.bin` | `crypto` | `file` | | `secret.bin` |
| `a+b+c:x` | `a` | `b+c` | | `x` |
| `subfile,,start,1024,end,4096,,:archive.bin` | `subfile` | — | `,,start,1024,end,4096,,` | `archive.bin` |
| `async:http://host/path` | `async` | — | | `http://host/path` |
| `cache:async:https://host/p` | `cache` | — | | `async:https://host/p` |
| `rtmp://host/app/s live=1` | `rtmp` | — | | `//host/app/s live=1` † |

† `inline_opts` is empty until `Url::take_inline_opts()` is called, which the
RTMP family does and nobody else does. A path may legitimately contain spaces
and `=`; splitting unconditionally would rename files. A token is only taken if
whitespace precedes it, so `file:my=name.mkv` is untouched.

#### The invariant that matters

```
split_url(s).to_string() == s        for every s
```

Splitting is **lossless**. If it were not, the string the whitelist checked and
the string a protocol opens could differ — which is the shape of a bypass. This
is a unit test, a proptest, and the primary assertion of the `io_url_split`
fuzz target.

### The whitelist gate

```
allowed(scheme) =
      scheme ∉ blacklist                                        (W1)
  AND (whitelist is None OR scheme ∈ whitelist
                         OR scheme ∈ parent.default_whitelist)  (W2, W3)
  AND depth < recursion_limit                                   (W4)
```

* **W1** — the blacklist always wins, checked first, so a scheme on both lists is
  refused.
* **W2** — a demuxer that opens nested URLs (`hls`, `dash`, `concat`, `sdp`,
  `tee`, `image2` with a pattern) **must** route through `ProtocolEnv`. CI
  enforces the other half: no `vaco-demux-*`/`vaco-mux-*` crate may depend on a
  concrete protocol crate, only on this one. A demuxer that could construct a
  `FileProtocol` directly could skip the gate.
* **W3** — the default grants of a remote playlist protocol exclude `file`. A
  hostile `.m3u8` served over HTTP cannot read `/etc/passwd`. This is the single
  most important security property in the I/O layer, and `tests/whitelist.rs`
  covers both the `file:` spelling and the bare-path spelling, because U1 makes
  them the same thing.
* **W4** — `depth` increments on every nested open including
  protocol-over-protocol, so `cache:async:https://…` is depth 3. Checked before
  dispatch, so a recursion bomb costs no opens.

`ProtocolEnv` is **threaded down and never reconstructed** — a reconstructed
environment is a reset privilege check. `ProtocolRegistry::resolve` is the single
function that applies the gate and produces the descended environment, so there
is exactly one place it could be bypassed.

A denied *unknown* scheme reports the denial, not the absence, so error messages
are not a registry oracle.

### Errors

`ProtocolError` is separate from `vaco_core::Error` because the interesting cases
carry the offending name and `vaco_core::Error` is a closed enum this crate
cannot extend. `Denied` is a distinct variant from `Unsupported`: "we refuse"
and "we cannot" are different facts, and a log line about a refused open should
read as one. Conversion into `vaco_core::Error` is lossy by design.

## How to change it

* **Adding a protocol**: implement `Protocol`, declare a `static ProtocolDesc`,
  and register it. `Protocol` is `Send + Sync` and stateless — `open` produces
  the state — which is what lets the descriptor be a constant.
* **Adding a nested-opening protocol**: set `flags.nested_scheme`, list what it
  grants in `default_whitelist`, and open through `env.registry.open(..., env)`
  using the env you were handed. Never build a fresh `ProtocolEnv`, and never
  reach for a concrete protocol type.
* **Changing the grammar** means changing the round-trip invariant. Add the case
  to `round_trip_is_exact` first; if it cannot be made to hold, the change is
  wrong.
* **Gotcha — `args` is not in plan 18.** The plan's `Url` has no field for the
  `subfile,,start,…:` prefix, which sits before the `:` and so cannot live in
  `rest`. Without it the round-trip invariant is unachievable. Added deliberately.
* **Gotcha — case.** Scheme lookup and every gate comparison are
  ASCII-case-insensitive. `HTTP://` gets pasted more often than anyone would
  like.

## Configuration

Not a configured crate: it carries policy rather than setting it.

| Knob | Where from | Default |
|---|---|---|
| `whitelist` | `-protocol_whitelist` | `None` — unrestricted, correct for a URL the *user* typed and wrong for one that came out of a file |
| `blacklist` | `-protocol_blacklist` | `None` |
| `recursion_limit` | `ProtocolEnv::with_recursion_limit` | `DEFAULT_RECURSION_LIMIT` = 8 |
| `root` | caller, per rule U2 | `None` |
| `rw_timeout` | `-rw_timeout` | `None` |

`ProtocolDesc::options` is `Option<fn() -> &'static Schema>` — a function pointer
rather than a reference, because `schema_of` is not `const` and the descriptor
must be a `static`.

## Dependencies

* `vaco-core` — `Error`, `Dict`.
* `vaco-io` — `MediaSource`, `MediaSink`, `CancelToken`.
* `vaco-opts` — `Dict` (re-exported from `vaco-core`) and `Schema` for
  `-h protocol=name`.
* `proptest` (dev) — the round-trip and idempotence properties.

`#![forbid(unsafe_code)]`, no external crates, no FFI.
