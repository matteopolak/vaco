# `vaco-mux-image2`

Layer 4. The `image2` muxer: filename patterns, `-update`, `-strftime`,
`-frame_pts`, `-atomic_writing`. FM-35b, issue #593. Companion crate:
`vaco-demux-image2` (the read side, FM-35a, issue #592), on which this crate
depends for its `%d`/`%0Nd` sequence-pattern grammar (`pattern.rs`) — a
format-crate-to-format-crate dependency, not a D14.1 layering violation,
which is specifically about depending on a codec/parser crate.

---

## What it is

* `Image2MuxWriter` (`writer.rs`) — the real thing: a filename pattern plus
  `Image2MuxOptions`, one file written per frame (or one file total under
  `-update`).
* `MUXER_IMAGE2` (`pipe_mux.rs`) — the registry-reachable degenerate case.

## How it works

### The registry seam does not fit this format either

`MuxerDesc::open` is `fn(Box<dyn MediaSink>) -> Result<Box<dyn Muxer>>` —
one already-open sink, no filename, so it cannot express "open a new file
per frame" any more than the demux side's `DemuxerDesc::open` can express
"open many files by pattern." `MUXER_IMAGE2` gets `image2pipe`'s shape
instead: every frame's payload written back to back into the one sink it is
given (`pipe_mux::Image2SinkMuxer`, essentially `vaco-mux-raw::RawMuxer` with
one stream limit). This is not an approximation of the real muxer — it is
the literal correct behaviour for "one sink, no pattern," and it is the
write-side mirror of what `vaco-demux-image2`'s pipe splitters already expect
to be fed (`ffmpeg -f image2pipe ... | ffmpeg -f png_pipe -i -` is a real,
supported reference pipeline).

Use `Image2MuxWriter::create` directly for the real thing.

### `-update`, `-strftime`, `-frame_pts`, numbering — resolution order

`Image2MuxWriter::filename_for` (one call per frame):

1. **`-update 1`**: the pattern *is* the filename, used verbatim, every
   frame. Measured — `ffmpeg -update 1 -f image2 upd.png` writes exactly
   `upd.png`, never `upd1.png` or similar.
2. **`-strftime 1`**: expand the pattern through `crate::strftime` against
   the wall clock at the moment of the call.
3. **`-frame_pts 1`**: substitute the packet's own PTS (as passed to
   `write_frame`) into the pattern's `%d`/`%0Nd` placeholder, instead of a
   counter.
4. Otherwise: a monotonic counter starting at `-start_number` (default `1` —
   **not** the demux side's `0`; both defaults were read from their own
   `-h` page, not assumed to match).

Combining `-update`/`-strftime`/`-frame_pts` was not measured (no
combination produced an obviously wrong file in casual testing, but none was
checked against the reference either) — `Image2MuxWriter` picks `update` >
`strftime` > `frame_pts` > counter, and that priority is a design choice of
this crate's, not a measured fact.

### `-strftime`'s wall clock never risks a wasm panic

`strftime::expand_now` calls `vaco_time::unix_nanos()`, never
`std::time::SystemTime::now()` directly — the latter panics on
`wasm32-unknown-unknown`. On a target with no wall clock (wasm without the
`web` feature) it returns `Error::Unsupported` rather than inventing a date.
The calendar conversion itself (`civil_from_unix_seconds`/`days_from_civil`)
is Howard Hinnant's public-domain closed-form Gregorian algorithm — no
lookup table, no loop, so it cannot degrade on an adversarial timestamp.

Directive coverage (deliberately a subset — an unrecognised directive passes
through literally rather than being guessed): `%Y %y %m %d %H %M %S %j %F %T
%%`.

### `-atomic_writing 1`

Writes to `<final-name>.tmp`, then `fs::rename`s it into place. **The
reference's own temporary-name scheme could not be observed** — there is no
filesystem tracer available in this sandbox (no `sudo`, so no `fs_usage`/
`dtruss`), and `-atomic_writing`'s effect is invisible from the muxer's
external behaviour alone (`ls` after the fact just shows the final name
either way). What is preserved is the property that actually matters — a
concurrent reader never observes a partially-written file — even though the
literal temporary filename this crate uses very likely differs from the
reference's.

## Not implemented

`-protocol_opts` (per-file protocol options; this crate always writes with
plain `std::fs`, so there is no protocol layer for it to configure).

## Configuration

`Image2MuxOptions` (plain `Default` struct, no `vaco-opts` derive — see the
demux crate's doc for why that matches every other format crate in this
codebase today):

| Field | Default | `-h muxer=image2` name |
|---|---|---|
| `update` | `false` | `-update` |
| `start_number` | `1` | `-start_number` |
| `strftime` | `false` | `-strftime` |
| `frame_pts` | `false` | `-frame_pts` |
| `atomic_writing` | `false` | `-atomic_writing` |

## Dependencies

`vaco-core`, `vaco-io`, `vaco-time` (the wasm-safe wall clock), `vaco-limits`,
`vaco-packet`, `vaco-format-core`, `vaco-codec-core`, and `vaco-demux-image2`
(for `pattern::SequencePattern` and `fsutil::split_dir_and_name`). `std::fs`
for the actual writes — compiles on `wasm32-unknown-unknown`, fails every
call at runtime there.

## Gotchas

* `Image2MuxWriter::create` rejects a pattern with no `%d` placeholder
  *unless* `-update` or `-strftime` is set (neither needs one). A caller
  hitting `Error::InvalidData` here almost always forgot one of those two
  flags rather than the placeholder itself.
* `strftime::expand` is pure string substitution and cannot fail; only
  `expand_now` (which reads the clock) returns a `Result`.
