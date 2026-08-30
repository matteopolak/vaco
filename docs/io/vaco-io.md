# `vaco-io`

Layer 2. Byte sources and sinks, buffering, seekability — the `AVIO` equivalent.

## What it is

The bottom of the I/O stack: what a demuxer reads from and a muxer writes to.
Deliberately **not** `std::io::Read + Seek`, because a media source has to answer
three questions `std` cannot express, and demuxers make genuinely different
decisions from the answers:

| Question | Answered by | Why a demuxer cares |
|---|---|---|
| Is seeking cheap, expensive, or impossible? | `Seekability` | Whether to build an index, do a two-pass read, or work in one pass |
| How big is this, really? | `MediaSource::size` | `Seek::End` costs a round trip on HTTP and is meaningless on a pipe |
| What are the next *n* bytes, without consuming them? | `MediaSource::peek` | Format probing, **including on a pipe** |

`Seekability::Expensive` is not decoration. A source that reports it makes
`IoContext` substitute read-and-discard for any forward seek under
`short_seek_max`, which is the difference between one HTTP connection and one
connection per box header while walking an MP4 `moov`.

## How it works

### Three layers, each with one job

```
RawSource            one call per syscall: read, seek, size, seekability
   ↓ PeekSource      adds the unread-bytes window that `peek` needs
MediaSource          the frozen, object-safe interface every protocol produces
   ↓ IoContext       the 32 KiB buffer, typed readers, short seeks, checksums
demuxer
```

A protocol crate implements `RawSource` and wraps it in `PeekSource`. It never
writes a buffer of its own. The real read buffer lives once, in `IoContext`, so
in the steady state no byte is copied twice: `PeekSource`'s window is empty once
probing is over, and reads pass straight through it.

The alternative — every protocol implementing `peek` itself — was rejected
because `peek` is subtle (compaction, growth, EOF, budget) and would have been
re-implemented once per protocol with a different bug each time.

### `IoContext`'s buffer

```
buf:  [........xxxxxxxxxxxx........]
       0       head        tail    len
base = logical offset of buf[0]        pos() = base + head
```

Every method preserves `head <= tail <= buf.len()`. `compact()` slides
`[head, tail)` to the front and moves `base`. Consumption goes through one
`advance(n)`, which is also where an open checksum is fed — so there is exactly
one place a byte can be counted twice or skipped.

### `IoContext::seek`, in order

1. **Inside the buffer**, including backwards, down to `base`. Free, and
   constant: every box and element parser overshoots and rewinds.
2. **A forward hop of at most the short-seek threshold** becomes
   read-and-discard. The threshold is derived from seekability, not configured
   per source: `Cheap → 0` (a local seek always wins), `Expensive →
   short_seek_max`, `None → u64::MAX` (read-and-discard is the only
   implementation).
3. **A real `MediaSource::seek`**, which resets the buffer.

On a forward-only source a backward seek outside the buffer is
`Error::NotSeekable` — never a silent wrong answer.

**Past-EOF seeks are legal and unclamped**, the same as `lseek` on a real
file: `MediaSource::seek` reports the position it actually reached, which can
be past the source's own length, and only a subsequent read discovers there
is nothing there. Every `MediaSource` this crate ships agrees on this,
`MemorySource` included — it used to clamp to `data.len()`, which silently
made it disagree with `FileSource` (a bare `File::seek(SeekFrom::Start)`,
which the OS never clamps either) about where a demuxer ended up after an
identical seek. A source is still free to clamp *its own* bound for reasons
that are not "hit real EOF" — `vaco-protocol-wrap`'s `SubfileSource` clamps to
its window's end, which is that source's own notion of EOF — `seek`'s
contract only requires that the returned position be honest about where the
source actually landed.

**`IoContext::skip(n)` is exactly `n` or an error**, never a silent partial
move. It calls `seek` internally and compares the position that comes back
against the target; a mismatch — whether from real EOF, a short read-and-
discard, or a source's own bound — is `Error::UnexpectedEof`, not a rounded-
down success. Every `io.skip(n)?` call site in the format/demux layer assumes
its position advanced by precisely `n` afterward; the alternative (returning
how far it actually moved) would have silently broken all of them, since none
inspect the return value today. A caller that genuinely wants "as far as
possible" should call `IoContext::seek` directly and read the position it
returns, the way `vaco-demux-mpegts`'s resync logic already does.

### `peek`

`peek(n)` compacts, grows the buffer to `n` if needed (charging the budget), then
fills until it has `n` bytes or hits EOF. It returns a borrow of the buffer, so
it cannot consume by construction. Fewer than `n` bytes come back only at EOF.

Because the bytes live in `IoContext`'s own buffer, **this works on a pipe**.
`tests/peek.rs` proves it over a real `std::io::pipe()`, not a simulation.

### Sticky error and EOF

Once a transport read fails, the error is stored and every later call replays it
rather than retrying. `vaco_core::Error` is not `Clone` (it wraps
`std::io::Error`), so `replay()` re-manufactures an equivalent value preserving
kind and message; the `source()` chain does not survive. `clear_error()` exists
for the caller that has done something to make progress possible.

`at_eof()` is only true once a read has actually hit the end — the same contract
`avio_feof` has. Reading the last byte does not set it.

### Reading and writing are separate types

`IoContext` reads; `IoWriter` writes. Plan 18 §2.2 specifies one type with both
halves, mirroring `AVIOContext`'s `write_flag`. That was not adopted: one type
makes "read from a write context" a runtime error on forty methods, and two
types make it a compile error and delete the check. This is a deliberate
divergence, recorded here because contributors will compare the two.

### `DynBuf` and `SharedDynBuf`

`DynBuf` is the `avio_open_dyn_buf` role: a growable, **seekable** in-memory sink
for the write-measure-patch pattern every ISO-BMFF and Matroska muxer needs.
Seekable is the point — the patch step is a seek.

`IoWriter` takes ownership of its sink, which is right for a file and wrong for
an element buffer the muxer has to read back. `SharedDynBuf` is the same buffer
behind an `Arc<Mutex<_>>`: one clone goes into the writer, one stays with the
muxer.

## How to change it

* **The `MediaSource`/`MediaSink` traits are frozen.** They live at the top of
  `src/lib.rs`, verbatim. Adding a method breaks every protocol; report it rather
  than doing it. Everything added since — `RawSource`, `PeekSource`, `IoContext`,
  `DataMarker` — was added *around* them for exactly that reason.
* **Adding a transport**: implement `RawSource` (usually one method) and wrap in
  `PeekSource`. Do not implement `MediaSource` by hand unless the transport has
  a genuinely better peek than "read ahead into a window" — an mmap would.
* **Adding a byte-order reader**: put it next to `rb32` in `ctx.rs` and route it
  through `fixed::<N>()`, which routes through `read_exact`, which routes through
  `read_partial`. Do not touch `head`/`tail` directly; `advance()` is the only
  writer, and the checksum depends on that.
* **Adding a checksum kind**: `checksum.rs`. The implementations are bit-serial,
  not table-driven, deliberately: regions are small (a TS section is at most
  1021 bytes), and a table would be the only indexing in the crate, where
  `clippy::indexing_slicing` is denied. If a whole-file checksum protocol
  (`crc:`, `md5:`) ever needs throughput, add a table there and keep this one.
* **Gotcha — checksums and seeks.** A region accumulates bytes *consumed
  sequentially*. A seek does not feed what it skipped. That is what the container
  formats using this want, and it is why a region is not defined by a byte range.
* **Gotcha — `IoWriter::flush` copies.** The buffer is copied into a temporary
  because the borrow checker cannot see that `sink` and `buf` are disjoint fields
  across a `Box<dyn _>` call. If this shows up in a profile, the fix is a
  split-borrow helper, not `unsafe`.

## Configuration

`IoOptions`, passed to `IoContext::new` and `IoWriter::new`:

| Field | Default | Meaning |
|---|---|---|
| `block_size` | 32 KiB (`DEFAULT_BLOCK_SIZE`) | Read/write buffer. Clamped to `[64, 16 MiB]`. The reference tool's default, which matters because buffer size determines how many range requests an HTTP walk makes and therefore shows up in differential traces. |
| `direct` | `false` | `-avioflags direct`: bypass the buffer for reads/writes at least as large as it, and flush after every write. |
| `short_seek_max` | 64 KiB | Forward seek distance that is cheaper as a read-and-discard, on an `Expensive` transport. Ignored for `Cheap` and `None`. |
| `limits` | `Limits::strict()` | Caps every buffer this context allocates. `limits.max_probe_bytes` is the cap on a single `peek`. |

Every allocation in the crate goes through a `vaco_limits::Budget` — the read
buffer (whose size can come from a URL option), the peek window (whose size comes
from `probesize`), `DynBuf`'s growth (which a muxer drives from packet payloads).
`clippy.toml` bans raw `Vec::with_capacity` to force this.

`CancelToken` is attached with `IoContext::set_cancel` and checked before every
transport read.

## Dependencies

* `vaco-core` — the `Error` taxonomy.
* `vaco-limits` — `Budget` and `Limits`.
* `proptest` (dev) — the transparency, seek-equivalence and peek properties.

No external crates, no FFI, `#![forbid(unsafe_code)]`. `std::io::pipe` is used in
tests only, and is `std`, which D14.3 permits everywhere.
