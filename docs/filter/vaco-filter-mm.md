# vaco-filter-mm

Plan 16 §4.4's multimedia/T1-plumbing row (GitHub #479, FT-4.12f), plus the
graph-plumbing/source-sink filters this crate carried under its previous name
`vaco-filter-plumbing` (FT-4.3, GitHub #467). 37 of the row's 41 filter
names are registered here; `avsynctest`, `cmdsocket`/`acmdsocket` and
`aeval` are the four left, deliberately — see the exactness table below for
why each one.

## What it is

Filters from two plan rows sharing one crate through a rename
. Each filter is a module exposing
`pub const DESC: FilterDesc` plus a crate-private `create`;
[`registry::MmRegistry`] dispatches by name.

## Per-filter exactness, at a glance

Every claim below is backed by a probe or a test cited in "How it works" or
in the filter's own module doc; this table is the index, not the evidence.

| Filter(s) | Exact? | Why not, if not |
|---|---|---|
| `null`/`anull`/`copy`/`acopy` | Yes | Zero options, pure passthrough. |
| `split`/`asplit` | Yes | Fan-out only; `Frame` clone is Arc-cheap. |
| `trim`/`atrim` | Yes | Half-open boundary and the sample-straddle cut both measured. |
| `setpts`/`asetpts` | Partial | `RTCTIME`/`RTCSTART`/`INTERLACED`/`S`/`SR` unimplemented (`NaN`); `strip_fps` parsed, not applied. |
| `settb`/`asettb` | Partial | Literal `num/den`, `intb`, `AVTB` measured exact; a mixed expression's evaluation order is not. |
| `select`/`aselect` | Partial | Routing (`ceil`, NaN/negative→0) and most variables measured exact; `scene` is a structural stand-in (see below); `pict_type`/`interlace_type`/`key` unimplemented. |
| `concat` | Partial | Per-stream independent segment advance, not the reference's shortest-stream-in-segment rule; indistinguishable except when a segment's own tracks disagree in length. |
| `color`/`nullsrc`/`nullsink`/`anullsrc` | — | Correct but misfiled — see the divergence note below. |
| `metadata`/`ametadata` | Partial | Every mode/function measured except `function=expr`'s exact reference evaluation; `print` computes but has nowhere to emit without `file` (no log sink exists yet). |
| `loop`/`aloop` | Partial | Frame-count/PTS semantics measured exact for `loop`; `aloop`'s sample `size`/`start` are frame-granular, not sample-exact. |
| `reverse`/`areverse` | Partial | Content-reversal and idempotence measured; non-uniform frame-duration handling is a structural reading. |
| `segment`/`asegment` | Structural | The "opposite of concat" reading, not independently measured against the reference's own timestamp handling. |
| `interleave`/`ainterleave` | Structural | Merge-by-timestamp measured; `duration`'s three end conditions are a structural reading of the option names. |
| `streamselect`/`astreamselect` | Partial | `map` routing and the runtime `map` command both measured/implemented; duplicate `map` entries (fan-out) are a known, documented gap. |
| `sendcmd`/`asendcmd` | Yes | Grammar and enter/leave edge detection measured against every `filters.texi` worked example; fired commands dispatch through the graph to filter-name or exact instance targets before the triggering frame reaches that target. |
| `cue`/`acue` | Partial | `cue=0` (the default) is a measured, deterministic no-op; a non-zero cue depends on real wall-clock time, not covered by a fast test. |
| `realtime`/`arealtime` | Structural | Implements the documented pacing/discontinuity rule; not measured against the reference's own wall-clock precision. |
| `latency`/`alatency` | Honest no-op | No per-link latency instrumentation exists in this framework for a leaf filter to read. |
| `bench`/`abench` | Partial | `start`/`stop`/elapsed-time accumulation measured against the reference's metadata-key mechanism; no log sink to report through. |
| `perms`/`aperms` | Honest no-op | This project's `Frame` has no read-only/writable bit — an architecture mismatch, not an oversight. |
| `sidedata`/`asidedata` | Partial | 4 of the reference's 28 `type` values map to a `FrameSideDataKind` this project models; `type` is a ranged integer, not named constants (a time-accepted divergence from `metadata`'s treatment). |
| `avsynctest` | Not implemented | A synthetic A/V generator disproportionate to this row — the `vaco-filter-aeffects` precedent for `surround`/`headphone`. |
| `cmdsocket`/`acmdsocket` | Not implemented | Need a real listening socket, outside a filter's normal scope. |
| `aeval` | Not implemented | Deferred for time; the reference's own documentation calls it "slow… for faster processing use a dedicated filter", which is why it was the lowest priority left when the row's budget ran out. |

## `color`/`nullsrc`/`nullsink`/`anullsrc`: a recorded divergence, not an oversight

Plan 16 §4.2/§4.3 place these four in `vaco-filter-source`/`vaco-filter-asource`,
not here. Both crates exist but do not yet register these names. This crate
does not own either under the single-writer rule, so rather than delete
working filters with nothing to replace them (a CLI regression for no gain),
they stay here until whoever owns `-source`/`-asource` pulls them across —
`cargo xtask dup-check` is the safety net that catches the day both copies
exist at once.

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
`expr` is non-zero. `outputs>1` routes by the reference's own documented
rule — negative or `NaN` goes to the first output, otherwise `ceil(expr)-1`,
clamped — **fixed** in this pass from a `round(expr)-1` reading that agreed
with `ceil` on integers and disagreed everywhere else (measured against
`ffmpeg 8.1`: `select=outputs=3:expr='1.2'` lands in the *second* output,
which `round` would have put in the first). Also fixed: `NaN`/negative used
to be dropped outright rather than routed to output 0. Implemented
variables: `n`, `selected_n`, `prev_selected_n`, `pts`, `t`, `tb`,
`start_pts`, `start_t`, `scene`; not implemented: `pict_type`,
`interlace_type`, `key` (no signal for any of these yet), `pos` (permanently
`NaN`, matching the reference's own current behaviour).

`scene` uses `vaco_filter_vdsp::normalised_sad` between the current and
previous frame's luma plane — the same 0..1 frame-difference fraction
`vaco-filter-temporal::freezedetect` already treats as its scene signal, per
this row's brief to extend `vdsp` rather than duplicate it. **Not verified
bit-exact** against the reference's own (unread, D7) internal scene-score
formula — only the shape (0 for identical frames, bounded by 1) is
exercised. `NaN` on the first frame of a stream and for `aselect` (no luma
to diff).

### `metadata`/`ametadata` — the first consumer of the frame metadata dictionary (`metadata.rs`)

Reads and writes `Frame::metadata()` (interface gap 11, closed by
`freezedetect` and `vaco-filter-analysis`'s measurement filters, which are
the producers this filter is the first to consume). Every mode measured
against `ffmpeg 8.1` rather than inferred from `-h filter=metadata`'s option
list, which under-specifies all of the following:

- `select`/`add`/`modify` **reject construction** when `key` is unset
  (`add`/`modify` also reject a missing `value`) — matches the reference's
  own `Metadata key must be set` filtergraph-init failure, not a permissive
  default.
- `add` on an existing key and `modify` on an absent key are **both
  no-ops** — confirmed in both directions.
- `delete` with `key`+`value` only removes the entry when the value compares
  true through `function` (default `same_str`); `delete` with no `key`
  clears the whole dictionary.
- `print` emits **nothing at all** — not even the header line — when there
  is nothing to report. Its header is column-padded to fixed widths
  (`frame:{n:<5}pts:{pts:<8}pts_time:{t}`), and `pts_time` uses the same
  trimmed-six-decimal format as `freezedetect`'s `lavfi.*` tags.
- With no `file` option, `print` still computes and records its lines (a
  test-only accessor) but does not emit them anywhere — this project has no
  log sink a filter can write to yet, unlike the reference's `AV_LOG_INFO`.
  `file` set to a real path, or `"-"` for stdout, writes for real.

`mode` and `function` are `#[derive(OptEnum)]` types (see
`vaco-filter-asource::anoisesrc`'s `NoiseColor` for the pattern), so the
reference's own spellings parse directly — `mode=add`, not just `mode=1`.

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

### `loop`/`aloop` — a *suffix* looper, not an in-place one (`looping.rs`)

The natural reading of "loop the `[start, start+size)` window" is that
those frames get replayed in place. Measured against ffmpeg 8.1
(`looping.rs`'s module doc has the full probe table): the whole input
stream always plays through unchanged first, and the window — captured as
it streamed past — is *appended* after it, `loop` more times, with PTS
continuing the original stream's arithmetic progression rather than
reusing the window frame's own original timestamp. `loop=0` is a true
no-op, measured identical to no filter at all. The window is bounded by a
`vaco_limits::Budget` charged from real frame bytes, not from the `size`
option directly, since `size`'s own declared range (32767 frames for
`loop`, unbounded samples for `aloop`) does not itself bound memory.
`aloop`'s `size`/`start` are frame-granular rather than sample-exact.

### `reverse`/`areverse` — content flips, timing does not (`reverse.rs`)

Measured with a luma-stamped-by-frame-index source so content order is
distinguishable from timing: reversing flips the frame *content* order but
keeps the original *pts sequence* — output position `k` gets position
`k`'s original timing paired with position `N-1-k`'s pixel data. The
stronger check this row's brief calls for (a hypothesis that would look
right from one direction and wrong from the other): `reverse,reverse`
composes to the identity, measured against the reference and pinned as a
test. Every retained frame is charged against a `Budget` by its real bytes,
since this filter's whole contract — buffer the entire clip — has no
`size` option to derive a safer bound from.

### `segment`/`asegment` — the structural mirror of `concat` (`segment.rs`)

`filters.texi` calls this filter "the opposite of concat" in so many
words. Where `concat` rebases N segments into one continuous stream,
`segment` cuts one stream into N outputs at a `|`-separated (optionally
`+`-relative) boundary list, with no reason to rebase anything — each
output keeps its slice of the original timeline. Not independently
measured against the reference's own timestamp handling the way `concat`'s
rebasing rule was. Output pad count is capped through the same
`pads::of`/`pads::MAX` limit `concat`/`split` already enforce.

### `interleave`/`ainterleave` — merge by timestamp (`interleave.rs`)

Peeks one frame ahead on every still-relevant input and always takes
whichever has the smallest timestamp in seconds, converted through that
input's own link time base. Measured with two inputs carrying disjoint
even/odd pts sequences, confirming the output is the fully sorted union.
`duration`'s three end conditions (`longest`/`shortest`/`first`) are a
structural reading of the option names — `-h filter=interleave` shows
neither `eof_action` nor `shortest`/`repeatlast`/`ts_sync_mode`, so per
`AGENT-CONSTRAINTS.md` this does not ride on `vaco-filter-framesync`.

### `streamselect`/`astreamselect` — output count follows `map`, not `inputs` (`streamselect.rs`)

The reference's own worked example (`streamselect=inputs=2:map=0`) creates
exactly one output and switches it at runtime with `sendcmd`. Output count
is however many indexes `map` names, not `inputs` itself — an empty `map`
defaults to the identity. The runtime `map` command `filters.texi`
documents is implemented through `Filter::command`, and a command cannot
change the output count (rejected, since pads are fixed at configuration).
**Fuzz-found and fixed**: `inputs` is now bounded through `pads::of` before
`parse_map`'s empty-`map` fallback (`0..inputs`) ever runs — the ordering
used to be reversed, and `streamselect=inputs=999999999` requested an 8 GB
`Vec<usize>` before construction ever got to reject it. Duplicate `map`
entries (two outputs reading one input) are a known, documented gap: only
the first to run in a given step gets that input's next frame.

### `sendcmd`/`asendcmd` — parse, track and dispatch (`sendcmd.rs`)

The command-script grammar is only in `filters.texi`, not `-h` output;
fetched from the public documentation and implemented in full — interval
start/end, `,`-separated commands, `[enter+leave]` flags (default
`[enter]`), `#` comments, and the `expr` flag's expression evaluation.
Every one of `filters.texi`'s own worked examples is reproduced verbatim
as a test, including the three-interval commented-file example. A fired
command goes through `FilterContext::send_command`; the scheduler waits for
the current activation to return, resolves `TARGET` by filter name or exact
instance label, and then calls the target's `Filter::process_command`. This
preserves the split-borrow rule—a leaf never receives another filter's private
state—while ensuring the command lands before the triggering frame can activate
the downstream target. The end-to-end test wires `source -> sendcmd -> target ->
sink` and observes the target command on the frame at the interval boundary.
`Filter::fired` remains test-only evidence for parser and edge ordering.

### `cue`/`acue`, `realtime`/`arealtime`, `latency`/`alatency`, `bench`/`abench`, `perms`/`aperms`, `sidedata`/`asidedata` (`misc.rs`)

Six small filters sharing one file. `cue`/`acue` pass `preroll` seconds
immediately then buffer (Budget-charged) until wall-clock time `cue`
arrives — `cue=0`, the default, is a measured, deterministic no-op, and is
the only path a fast unit test can exercise; a non-zero cue depends on real
time. `realtime`/`arealtime` pace output to wall-clock time via
`vaco-time` (not `std::time`, which panics on wasm32 — D18); a gap past
`limit` resets the timer rather than trying to catch up, per
`filters.texi`. `latency`/`alatency` is an honest no-op: this framework's
cooperative scheduler exposes no per-link latency a leaf filter can read.
`bench`/`abench` is reachable now that the frame metadata dictionary exists
(interface gap 11) — `start`/`stop` and the running avg/max/min are
measured against the reference's own metadata-key mechanism, with no log
sink to report through. `perms`/`aperms` is an architecture mismatch: this
project's `Frame` has no read-only/writable bit, so every mode is a
passthrough. `sidedata`/`asidedata`'s `type` maps 4 of the reference's 28
`AVFrameSideDataType` names to the `FrameSideDataKind` this project
actually models, as a plain ranged integer rather than named constants —
accepted as a time-bounded divergence from `metadata`'s `OptEnum`
treatment.

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
  path (`vaco_filter_mm::setpts::setpts`), which is legal but confusing
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
- `vaco-expr` — `setpts`/`asetpts`, `settb`/`asettb`, `select`/`aselect`,
  `metadata`/`ametadata`'s `function=expr`, `sendcmd`/`asendcmd`'s `expr` flag.
- `vaco-filter-vdsp` — `normalised_sad`, for `select`'s `scene`.
- `vaco-limits` — `Budget`, for the row's size/count/duration-bounded
  filters (`loop`, `aloop`, `reverse`, `areverse`, `segment`, `cue`).
- `vaco-time` — `Instant`/`unix_nanos`/`sleep`, for `realtime`/`cue`/`bench`'s
  wall-clock and pacing needs (D18: never `std::time` directly, which panics
  on wasm32).
- `vaco-opts` — option parsing, including `#[derive(OptEnum)]` for
  `metadata`'s `mode`/`function`, `interleave`'s `duration`, `bench`'s
  `action`, `perms`'s `mode`, `sidedata`'s `mode`.
- `bitflags` — `sendcmd`'s enter/leave edge flags.
- `vaco-core::parse` — `color` (`Rgba`), `nullsrc`/`color` (`image_size`,
  `video_rate`, `duration`'s exact reference grammar).

## Issues

Closes GitHub #467 (FT-4.3) for the 20 filters this crate originally
implemented, and closes GitHub #479 (FT-4.12f, plan 16 §4.4): 37 of the
row's 41 filters landed; `avsynctest`, `cmdsocket`/`acmdsocket` and `aeval`
are left, with reasons in the exactness table above and in `lib.rs`'s crate
doc.
`buffer`/`abuffer`/`buffersink`/`abuffersink` are named in #467's 24 but
are out of this crate's scope — see "What is missing, and why" above; left
open, or to be handled by whichever of `vaco-filter-graph`/`vaco-cli-core`
wires the DSL spellings onto `vaco-filter-core`'s existing Graph I/O API.
