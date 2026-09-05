# `vaco-mux-stream`

Layer 4. Meta-muxers and one meta-demuxer: `concat`, `ffmetadata`, `segment`,
`stream_segment`, `tee`, `fifo` (FM-33, issue #590).

---

## What it is

Five muxers and one demuxer that either own other muxers/demuxers and route
packets into or out of them (`tee` fans one input to several muxers;
`segment`/`stream_segment` hand successive spans to successive muxer
instances; `fifo` buffers packets in front of one inner muxer; `concat`
drives several inner demuxers in sequence), or are a flat key/value text
format with no inner anything (`ffmetadata`, grouped here because issue #590
names it alongside the others, not because it shares their shape).

**`concat` is a demuxer, not a muxer.** Measured: `ffmpeg -muxers | grep
concat` prints nothing; `ffmpeg -demuxers | grep concat` prints `D   concat
Virtual concatenation script`. The issue that asked for this crate assumed a
muxer; probing said otherwise, so `concat` registers a
`vaco_format_core::DemuxerDesc`. The crate name is therefore slightly off
for this one registration — noted rather than worked around, since renaming
the crate mid-wave is not this brief's call to make.

## How it works

### The registry seam does not fit most of these

`vaco_format_core::MuxerDesc::open` is `fn(Box<dyn MediaSink>) ->
Result<Box<dyn Muxer>>` — one sink, no options, no way to name an inner
muxer or a list of output URLs. `tee`, `segment`/`stream_segment` and `fifo`
all *need* exactly that (a target format name, a URL list, per-output
options) to do anything beyond the degenerate case. Each carries two things,
mirroring the pattern `vaco-demux-image2` already established for the
identical gap (see its `multi.rs` module docs):

* a real, richly configurable constructor (`TeeMuxer::new`,
  `SegmentMuxer::new`, `FifoMuxer::new`) a caller with the missing
  information (an embedder, `vaco-cli` once it grows one, this crate's own
  tests) uses directly;
* the `MuxerDesc` registration, whose `open` reports the gap with
  `Error::Unsupported` rather than guessing at a default nobody asked for.

`concat` has the same shape of gap one level down: `DemuxerDesc::open` gets
one already-open `vaco_io::MediaSource` (the concat *script*) and a
`ParserProvider`, with no way to open the *other* files the script names.
`concat::ConcatSource` is this crate's own version of the seam
`vaco_format_core::BsfProvider`/`ParserProvider` use one layer up: a caller
with `vaco-registry` in scope implements it by probing and opening each
named file (this crate cannot depend on `vaco-registry` itself — that crate
depends on every format crate, including this one, so the edge would
cycle); this crate's own tests supply a fake.

`ffmetadata` had the mildest version of the same problem, one level further
in: `vaco_format_core::Muxer` gave a muxer no channel for file-level
metadata, per-stream metadata or chapters at all. **Closed**: `Muxer::set_metadata` now exists, and
`FfmetadataMuxer` overrides it — the override just stores the
`vaco_format_core::metadata::MuxMetadata` it is handed, and `write_header` is
what turns it into the actual document via `write`: file tags become global
lines, `stream_tags` become one `[STREAM]` block per `add_stream`-declared
track, `chapters` become `[CHAPTER]` blocks (`ChapterUID`/timestamps mapped
from `vaco_core::Chapter` via the module's own `chapter_meta` helper).
`MuxMetadata::attachments` has no representation in this format and is
silently ignored, matching the reference (`-f ffmetadata` has no attachment
section). A caller that never calls `MuxBuilder::with_metadata` — every
pre-existing call site — still gets exactly `;FFMETADATA1\nencoder=vaco\n`,
since `MuxMetadata::default()` is empty. The module's plain `write`/`parse`
functions remain the entry point for a caller that wants to build the
document itself without going through the `Muxer` trait at all.

### `ffmetadata`

The `;FFMETADATA1` key/value text format. Escaping: `=`, `;`, `#`, `\` and a
literal newline are backslash-escaped in a value on the way out
(`foo=a\=b\;c\#d\\e` for `a=b;c#d\e`, measured); an embedded newline is a
bare `\` immediately followed by a **real** newline byte, not the two
characters `\` `n`. Reading back, `\` followed by *any* character unescapes
to that character literally (`a\nb` reads as `anb`; a trailing lone `\` is
dropped) — a generic rule, not a five-entry table.

Grammar (measured by round-tripping through `ffmpeg -f ffmetadata`):

* A line starting with `;` or `#` is a comment, **including** the
  conventional `;FFMETADATA1` header itself — it is never validated on read,
  only always written.
* A line with no unescaped `=` is silently ignored, not an error and not a
  key with an empty value.
* `key=value` splits at the first unescaped `=`; neither side is trimmed.
* `[CHAPTER]`/`[STREAM]` open a section; every following `key=value` line
  belongs to it until the next `[...]` or EOF.
* Section order: global lines, then every `[STREAM]` block, then every
  `[CHAPTER]` block (measured with both present).
* The reference always appends its own `encoder=Lavf<version>` last,
  overwriting any user-supplied `-metadata encoder=...`. This crate mirrors
  the shape with its own identity (`encoder=vaco`, `ENCODER_TAG`) rather
  than impersonating a build of the reference — the same decision
  `vaco-mux-hash`'s `SOFTWARE_LINE` makes.

A fuzz-found corner: a key that, once escaped, still starts with a bare `[`
is indistinguishable on read from a `[SECTION]` header, because `parse`
checks for a section before it looks for `=`. `write_kv` escapes just that
leading character (`\[`) when it would otherwise collide — see
`fuzz/seeds/mux_stream_ffmetadata_reader/bracket_prefixed_key` for the
regression input.

### `concat`

Reads a script naming files to demux in sequence, on one continuous
timeline. `script.rs` is the pure grammar (no I/O), `mod.rs`'s
`ConcatDemuxer` is the layer that actually opens files and rewrites
timestamps.

Measured (`ffmpeg -f concat -safe 0 -i list.txt …`):

* `#` starts a comment; **`;` does not** (`; comment` fails with `Line 1:
  unknown keyword ';'`) — unlike `ffmetadata`'s dual `;`/`#` convention.
* Tokenising a directive line uses the same quote/backslash grammar as
  `vaco_core::escape`: `'...'` is a literal span with **no** backslash
  meaning inside it, `\` outside quotes strips itself and keeps the next
  character, and quoted/unquoted/escaped spans concatenate freely. Measured:
  `file 'weird dir/seg\'s.ts'` resolves to `weird dir/seg\s.ts` — exactly
  `vaco_core::escape::unescape`'s reading (the quote closes at the bare `'`
  right after the backslash, since backslash has no effect inside a quote;
  `s.ts` follows unquoted; the final `'` opens a quote that runs to EOL).
  An unterminated quote is tolerated, not an error.
* An unrecognised keyword is a hard error: `Line {n}: unknown keyword
  '{kw}'` (verbatim). `option <name> <value>` is rejected unless `-safe 0`
  (`Line {n}: option not allowed if safe`, verbatim).
* `duration`/`inpoint`/`outpoint` accept both `SS[.frac]` and
  `HH:MM:SS[.frac]`, via `vaco_core::parse::duration` (the CLI's own
  grammar for this shape). Decimal digits beyond six are retained as an exact
  ratio; formatting for display is the only microsecond rounding boundary.
* An absolute path (or one containing `..`) is rejected as `Unsafe file
  name '<path>'` unless `-safe 0`.

What is faithful versus approximate: `file`/`duration`/`inpoint`/`outpoint`
are honoured, including per-packet trimming to `[inpoint, outpoint)` and a
running timestamp offset per finished file (its `duration` directive if
given, else its own demuxer's reported duration, else zero — a file with
neither produces overlapping, not sequential, timestamps for whatever
follows, which is recorded rather than silently patched over). Every file is
assumed to expose the **same number of streams in the same order** — the
overwhelmingly common real use of `concat` — and `option`,
`file_packet_metadata`, `stream`/`exact_stream_id` parse into
`FileEntry`/`Directive` but are not semantically wired up (their exact
effect on the reference was not pinned down in the probing budget available;
see `script.rs`'s module docs). `-auto_convert` (bitstream reformatting
between differently-framed segments) is not implemented — it needs a
`BsfProvider` `open_script` does not take today.

### `tee`

One input, several inner muxers, via the
`[opt=val:opt2=val2]path|[opt=val]path2` grammar (`tee/grammar.rs`). Reuses
`vaco_core::escape` wholesale for all three separator levels (`|`, `:`,
`=`) rather than a second hand-rolled scanner — probing showed the identical
quote/backslash rules at every level.

Measured: `|` separates outputs (`\|` escapes a literal pipe in a path);
`[...]` at the start of a segment holds `:`-separated `key=value` options,
everything after the closing `]` is the path; `select=v`/`select=a` filter
an output by media type, `select='a:0'` (quoted, to protect the `:` from the
option-list separator) by type-and-index; `f=<name>` overrides the target
format; `onfail=ignore` lets the whole `tee` open succeed when one output's
`write_header` fails (`"Slave muxer #0 failed: ..., continuing with 1/2
slaves."`, exit 0) versus aborting the whole thing without it (`"Slave
muxer #0 failed, aborting."`, exit 254).

`StreamSelector` supports the two forms probing exercised: a bare media
letter and a `type:index` pair. An unparseable `select=` value is treated as
"select everything" — the safer of the two wrong answers, since dropping a
stream from an output that asked to keep it is a silent, harder-to-notice
loss than including an extra one. `bsfs=`/`bsfs/<type>=` parses but is not
applied (needs a `BsfProvider` and a per-stream `BsfChain`, which would
noticeably heavy up `TeeMuxer::new`'s signature for a feature nothing here
exercises end to end). `use_fifo`/`fifo_options` (the muxer's own top-level
options, not part of the per-output grammar) are not auto-applied; a caller
wraps an inner muxer in `crate::fifo::FifoMuxer` itself before handing it to
`TeeMuxer::new`.

### `segment` / `stream_segment`

Successive spans, each its own inner muxer, built from four sub-modules:

| Module | Job |
|---|---|
| `segment::planner` | the pure cut-decision state machine, unit-tested against plain `(pts, is_key)` sequences with no I/O |
| `segment::pattern` | `%d`/`%0Nd` filename numbering |
| `segment::strftime` | `-strftime`'s filename expansion, via `vaco-time` |
| `segment::list` | `-segment_list_type`'s four renderings |

Every option name and default (`segment_time` 2s, `segment_time_delta` 0,
`break_non_keyframes` false, `individual_header_trailer` true,
`reset_timestamps` false, `write_empty_segments` false, …) is taken directly
from `ffmpeg -h muxer=segment`. `stream_segment` lists the identical option
set minus the `segment_format`/`segment_list*` family (measured: `-h
muxer=stream_segment` is `segment`'s listing with those five lines removed)
— modelled as `MUXER_STREAM_SEGMENT` sharing `SegmentMuxer` with
`MUXER_SEGMENT`, since nothing stops a `stream_segment`-driven
`SegmentOptions` from setting `segment_list`, it is simply never asked to
via that descriptor's own name.

**A packet's `pts` is read as `TIME_BASE_Q` microseconds directly**, not
rescaled from a per-stream time base looked up anywhere: `SegmentMuxer`
declares no `stream_time_base` opinion, so a caller driving it through
`MuxWriter` has every packet already in that base by the time it arrives —
and there is nowhere else to look one up from, since `CodecParameters`
(all `add_stream` receives) carries no time base at all; only a `Stream`
does, which this muxer never sees. See `planner`'s module doc.

The segment records themselves retain their start and duration as exact
`vaco_core::Duration` values. Cut spans and the final `pts + duration` span
are compared and subtracted as rationals, so a large timestamp cannot erase a
microsecond segment through `f64` cancellation. List renderers are the one
decimal-output boundary: they use integer decimal arithmetic with six minimum
and fifteen maximum fractional digits; no list type converts timing through a
binary float.

`-segment_list_type`: `flat` (one filename per line), `m3u8`/`hls` (a real
HLS media playlist — `#EXTM3U`/`#EXT-X-VERSION`/`#EXT-X-TARGETDURATION`/
`#EXTINF`/`#EXT-X-ENDLIST`, RFC 8216 structure, not ffmpeg-internal), `ext`
and `csv` (this crate's own assumed column layouts — `name,duration` and
`name,start,end` respectively — not independently confirmed against the
reference), and `ffconcat` (reuses `crate::concat::script`'s own grammar for
the write side, so every list this crate writes parses back with
`concat::script::parse` — a real round-trip property test, not just a
format guess).

What is not modelled precisely: `individual_header_trailer=false` needs
"this is the last segment," only knowable at `write_trailer` time after
every intermediate segment has already been opened and closed — this crate
always gives every segment its own header and trailer regardless of the
flag. `segment_format_options`, `increment_tc`, `segment_atclocktime`,
`segment_clocktime_offset`/`_wrap_duration`, `segment_list_size`,
`segment_list_entry_prefix`, `segment_header_filename` are not implemented
at all.

### `fifo`

Buffers packets (and the trailer call) for one inner muxer, drained on a
background thread. Functionally confirmed: `-f fifo -fifo_format mpegts
-queue_size 8 out.ts` transparently produces a normal `mpegts` file — `fifo`
is a pass-through with buffering, not a format of its own.

**Streams are added before the queue exists.** `FifoMuxer` holds `inner`
directly (no thread) until `write_header` is first called; `add_stream`
forwards straight through. The background thread is spawned only at that
point, taking ownership of `inner` — once it moves onto a worker thread the
calling thread has no way to drive `add_stream` on it, and every muxer in
this workspace requires every stream declared before the header, so streams
*must* be settled before the handoff rather than queued alongside packets.

**Why this is the one component in this set with a thread**: buffering only
decouples the writer from the caller if a *different* thread drains the
queue. The spawn (`spawn_worker`, a standalone top-level function so
`cargo xtask time-gate`'s scan can see the gate) is behind `#[cfg(not(target_family
= "wasm"))]`, mirroring `vaco-sched`'s `run_threaded`; on `wasm32` this
muxer instead drains synchronously on every call — not a hidden slowdown,
but D18's explicit degrade-to-serial shape, since there is no thread to
decouple onto. `vaco-time` is the only door to both the spawn and to
`-recovery_wait_time`'s sleep, since `Instant::now()`/`SystemTime::now()`
panic on `wasm32-unknown-unknown`.

Not wired up: `-recovery_wait_streamtime` (needs the stream's own clock,
which this muxer has no notion of independent of the packets it is hedging
a wall-clock timer against), `-recover_any_error` and
`-restart_with_keyframe` (need the recovery attempt to distinguish error
kinds and keyframes respectively, which the generic `vaco_core::Error` this
muxer receives does not reliably let it do, and no real transport failure
was reproducible against a `MemorySink` to probe against).

## How to change it

* **Grammar/format bugs** (`ffmetadata` escaping, `concat` tokenising, `tee`
  URL parsing) live in one file each (`ffmetadata.rs`, `concat/script.rs`,
  `tee/grammar.rs`) with a fixture-backed unit test and a proptest
  round-trip right next to the code; fix the function and the test in the
  same place. All three also have a fuzz target
  (`fuzz/fuzz_targets/mux_stream_*.rs`) — if a change touches the escaping
  or tokenising rules, rerun it before reporting done.
* **Segmentation timing** lives entirely in `segment::planner`, deliberately
  isolated from any I/O; a new cut rule is a new arm in
  `SegmentPlanner::on_reference_packet` plus a plain `(pts, is_key)` unit
  test.
* **A new inner-muxer capability** for `tee`/`segment`/`fifo` (wiring
  `bsfs=`, say) needs a `BsfProvider` threaded through the relevant `::new`
  constructor — none of the three take one today, by omission, not by a
  trait limitation.
* **`concat`'s stream-count assumption** (every file has the same streams in
  the same order) is the one thing likely to need relaxing first if this
  crate's scope grows; `ConcatDemuxer::streams` and the packet-forwarding
  loop in `read_packet` are where that logic lives.

## Configuration

Every registration except `ffmetadata` is driveable only through its own
real constructor, not through `-f <name>` via the registry (see "the
registry seam does not fit most of these", above):

| Registration | Real constructor | Registry `open` |
|---|---|---|
| `ffmetadata` | `ffmetadata::write`/`parse` (free functions), or `Muxer::set_metadata` through the registry — see above | Works fully now (CL-16) |
| `concat` | `ConcatDemuxer::open_script(script, ConcatOptions, &dyn ConcatSource)` | `Error::Unsupported` after validating the script |
| `tee` | `TeeMuxer::new(&[TeeOutput], Vec<Box<dyn Muxer>>)` | `Error::Unsupported` |
| `segment`/`stream_segment` | `SegmentMuxer::new(pattern, SegmentOptions, SegmentFactory)` | `Error::Unsupported` |
| `fifo` | `FifoMuxer::new(Box<dyn Muxer>, FifoOptions)` | `Error::Unsupported` |

`ConcatOptions`, `SegmentOptions` and `FifoOptions` each default to the
reference's own measured defaults (`-h demuxer=concat` / `-h muxer=segment`
/ `-h muxer=fifo`); see each type's doc comment for the field-by-field
mapping.

## Dependencies

`vaco-core`, `vaco-time` (the clock/thread door for `fifo` and `segment`'s
`-strftime`), `vaco-io`, `vaco-limits`, `vaco-packet`, `vaco-format-core`,
`vaco-codec-core` — the same baseline every sibling mux crate takes, plus
`vaco-time`. No third-party crates. No `vaco-parse-*`/`vaco-demux-*`
dependency (D14.1): `concat` reaches other containers only through the
caller-supplied `ConcatSource`, never by naming a demuxer crate directly.
