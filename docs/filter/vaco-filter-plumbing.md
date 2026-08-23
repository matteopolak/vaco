# vaco-filter-plumbing

T1 graph plumbing, sources/sinks, cutting/joining (FT-4.3, GitHub issue #467):
`split`/`asplit`, `null`/`anull`, `copy`/`acopy`, `setpts`/`asetpts`,
`settb`/`asettb`, `select`/`aselect`, `trim`/`atrim`, `concat`,
`nullsrc`/`anullsrc`, `nullsink`/`anullsink`, `color`.

## What it is

20 of the 24 filters `planning/16-filters.md` §5.3 groups as "Graph plumbing"
(12), "Cutting and joining" (3) and "Sources and sinks" (9) for the T1 set.
Each filter is a module exposing `pub const DESC: FilterDesc` plus a
crate-private `create`; [`registry::PlumbingRegistry`] dispatches by name.

## What is missing, and why: `buffer`/`abuffer`/`buffersink`/`abuffersink`

These four are **not** implemented in this crate, and that is deliberate, not
an oversight. Plan 16 §1.13 puts them in `vaco-filter-core` because they need
privileged access to link internals — a buffer source pushes directly into a
link's queue and a buffer sink holds frames with no downstream consumer.
Reading the real (not the plan-draft) `vaco-filter-core`, that privileged
mechanism already exists and is complete: `Graph::add_source`, `add_sink`,
`send`, `recv`, `close_source`, `source_wants`, `sink_format`. This crate's
own tests (`trim.rs`, `concat.rs`) exercise it directly.

There is nothing left for a leaf crate to implement under these names — a
`Filter` impl here would be a second, unprivileged, functionally inert
mechanism. Mapping the DSL spellings `buffer=`/`abuffer=`/`buffersink`/
`abuffersink` in a `-filter_complex` string onto `Graph::add_source`/
`add_sink` is a job for `vaco-filter-graph` (which owns the DSL) or
`vaco-cli-core` (which owns wiring a real pipeline to the graph), and it is
outside this crate's directory. **Left open in GitHub #467** for whichever
of those two takes it.

## How it works

### `trim`/`atrim` — the measured boundary (`trim.rs`)

Measured directly against ffmpeg 8.1 (commands and full output in `trim.rs`'s
module doc): the kept range is **half-open, `[start, end)`** — a frame
exactly at `start` is kept, one exactly at `end` is dropped — confirmed
independently on both the PTS form and the frame-index form. Automated as
`trim::tests::video_keeps_the_half_open_frame_range`.

The sharper finding is `atrim`: it **cuts a frame that straddles the
boundary** rather than keeping or dropping it whole. With 20-sample input
frames and `start_sample=25:end_sample=45`, the `[20,40)` frame becomes an
output frame at `pts=25` with 15 samples, and `[40,60)` becomes `pts=40` with
5 samples. Reproduced exactly by `slice_audio`, a plain byte-range copy with
no format conversion, and pinned by
`trim::tests::audio_cuts_straddling_frames_at_the_sample_boundary`.

Neither filter rebases timestamps — kept frames retain their *original* PTS
(the classic `trim=...,setpts=PTS-STARTPTS` pattern exists precisely because
`trim` alone does not rebase).

### `concat` — per-stream independent rebasing (`concat.rs`)

Each output stream tracks its own accumulated `offset` (the end timestamp of
every prior segment on *that stream*) and advances to its next segment the
instant its own input pad for the current segment reaches end of stream —
independently of every other stream. This is a stated simplification: the
reference switches every stream in a segment at the same moment, using the
segment's shortest stream as the cut point. For the overwhelmingly common
case (every stream in a segment has the same duration, true of any normally
demuxed file) the two are indistinguishable; they diverge only when a
segment's own tracks disagree in length, which `-unsafe`'s absence would
normally already have rejected upstream. `unsafe` itself is parsed but never
consulted, since this filter does no format-matching validation — negotiation's
tie mechanism already forces every segment's same-index stream to agree.
Verified end-to-end by `concat::tests::rebases_the_second_segment_after_the_first`
(two 5-frame segments, second segment's PTS `0..4` rebased to `5..9`).

### `select`/`aselect` (`select.rs`)

`outputs=1` (the default, and by far the common case) passes a frame when
`expr` is non-zero. `outputs>1` routes to `round(expr) - 1`, clamped — a
structural reading, not measured against the reference's own multi-output
semantics. Implemented variables: `n`, `selected_n`, `prev_selected_n`, `pts`,
`t`, `tb`, `start_pts`, `start_t`; not implemented: `pict_type`,
`interlace_type`, `key`, `scene` (no signal for any of these yet), `pos`
(permanently `NaN`, matching the reference's own current behaviour).

### `settb`/`asettb` (`settb.rs`)

Measured: a literal `"num/den"` **rebases** every frame's PTS into the new
time base exactly (a 25fps stream's `settb=1/90000` moves frame N to PTS
`N*90000/25`) — unlike `asetrate`, which only relabels. Implemented as an
independent `vaco-expr` evaluation of each side of the `/`, with `intb` bound
to the input time base's numerator on the left and denominator on the right,
and `AVTB`/`sr` bound to their documented meanings. This reproduces every
literal numeric form and the `intb`/`AVTB` keywords; a genuinely mixed
expression is not measured against the reference's exact evaluation order.

### `setpts`/`asetpts` (`setpts.rs`)

A `vaco-expr` evaluation per frame. Implemented: `N`, `PTS`, `STARTPTS`,
`PREV_INPTS`, `PREV_OUTPTS`, `T`, `TB`, `SAMPLE_RATE`, `FRAME_RATE`. Not
implemented (evaluate to `NaN`): `RTCTIME`/`RTCSTART` (wall clock),
`PREV_INT`/`PREV_OUTT`, `INTERLACED`, `S`, `SR`. `strip_fps` is parsed but not
applied.

### A framework fact this crate's tests surfaced

`Graph::send` (and, by the same F9 rule, `FilterContext::push_output`)
rescales a frame's timestamp from the frame's own declared `time_base` into
the link's negotiated one if they differ — it does **not** simply trust the
caller's raw tick value against whatever base the link happens to be using.
Two of this crate's own tests initially failed non-obviously (duplicated,
scaled-down PTS sequences) because a test built frames with
`vaco_filter_core::mock::gray_frame`'s hardcoded `1/25` time base while
declaring the source link at a different rate. Worth knowing before writing
another graph-level test in this style: match the mock helper's time base, or
rescaling silently does exactly what F9 says it will.

## How to change it

- Add a filter: create `src/<name>.rs`, declare `mod <name>;` in `lib.rs`, add
  a `[[component]]` entry to `vaco-component.toml`, wire it into
  `registry.rs`'s `NAMES` and `create` match, then run `cargo xtask
  gen-registry`.
- A file holding both media variants of one filter (e.g. `trim.rs` for
  `trim`/`atrim`) nests them as `pub mod video { .. }` / `pub mod audio { .. }`
  rather than reusing the filter names themselves as submodule names — naming
  a submodule `setpts` inside `setpts.rs` would shadow the file's own module
  path (`vaco_filter_plumbing::setpts::setpts`), which is legal but confusing
  and makes the component-fragment `ctor` path read oddly.
- Keep new option/filter/state types `pub(crate)` — see
  `vaco-filter-audio`'s crate doc for why that is what keeps this crate off
  `cargo xtask dup-check`'s ledger.

## Configuration

Options are declared with `#[derive(vaco_opts::Options)]` and parsed via
`set_from_string(args, "=", ":")`, matching `-h filter=<name>` output captured
under `LC_ALL=C` against ffmpeg 8.1 — see each module's doc comment for
exactly which of the reference's options are implemented.

## Dependencies

- `vaco-filter-core` — `Filter`, adapters, format negotiation.
- `vaco-filter-graph` — `FilterRegistry`/`Instantiate`/`Instance`, `pads::of`
  for dynamic pad counts.
- `vaco-expr` — `setpts`/`asetpts`, `settb`/`asettb`, `select`/`aselect`.
- `vaco-opts` — option parsing.
- `vaco-core::parse` — `color` (`Rgba`), `nullsrc`/`color` (`image_size`,
  `video_rate`, `duration`'s exact reference grammar).

## Issues

Closes GitHub #467 (FT-4.3) for the 20 filters this crate implements.
`buffer`/`abuffer`/`buffersink`/`abuffersink` are named in the issue's 24 but
are out of this crate's scope — see "What is missing, and why" above; left
open, or to be handled by whichever of `vaco-filter-graph`/`vaco-cli-core`
wires the DSL spellings onto `vaco-filter-core`'s existing Graph I/O API.
