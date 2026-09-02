# `vaco-sched` — the transcode scheduler

## What it is

The thing that owns the `demux → decode → filter → encode → mux` pipeline: it
decides what runs when, keeps memory bounded, gets end-of-stream ordering right,
and — where the machine allows it — runs stages concurrently.

It is a **state machine with a step function**, not a set of threads. `Pipeline`
holds every node and every queue; `Pipeline::step` advances it by one unit of
work; `Driver` is a loop around `step`. Threads, where they exist, are a
property of the driver, never of the state machine.

Nothing here opens a file, chooses a codec or parses a filtergraph. Components
arrive already built (`Box<dyn Demuxer>`, `Box<dyn Decoder>`, a configured
`vaco_filter_core::Graph`, `Box<dyn Encoder>`, `Box<dyn Muxer>`), so this crate
depends on the four framework crates and on no concrete component.

## How it works

### The API in five calls

```rust
let mut spec = PipelineSpec::new();
let input   = spec.add_input(demuxer);          // -i
let output  = spec.add_output(muxer);           // the output URL
let tap     = spec.input_stream(input, 0)?;     // 0:0
let frames  = spec.add_decoder(tap, decoder)?;  // -c:v something
let filtered= spec.add_filter(graph, &[SourceBind::new(frames, src, tb)], &[sink])?;
let packets = spec.add_encoder(filtered[0], encoder, enc_tb)?;
spec.map(packets, output, &params)?;            // -map
let mut pipeline = spec.build()?;
Driver::with_threads(4).run(&mut pipeline)?;
```

### `-map` is fan-out from a tap

`PacketTap` and `FrameTap` are `Copy` handles to a producing port. Using one
twice grows a second wire on that port and each item is cloned onto it —
cheaply, because a `Packet`'s and a `Frame`'s buffers are reference-counted.
So:

- one input stream to five outputs = five `map` calls with the same tap;
- stream copy = `map` a demuxer's tap directly, with no decoder in between;
- copy *and* transcode the same stream = both of the above in one spec.

The two tap types are distinct so wiring a frame producer into a muxer is a
compile error. That is the whole of the routing validation; everything else the
builder accepts is connectable. A tap can only name a node that already exists,
so the graph is **acyclic by construction** rather than by a check.

### Backpressure

Every edge is a `wire::Wire` bounded by a `Capacity` — a maximum item count
*and* a maximum byte count, whichever binds first — and every queued item is
charged to a `vaco_limits::Budget` underneath that.

Two bounds instead of plan 12 §7.1's `depth = clamp(target_bytes / frame_bytes,
2, 64)` because `frame_bytes` is not known at build time: the first frame's size
depends on the decoder and an `fps` filter can change it mid-stream. Carrying
both bounds computes the same answer from the data that actually flowed. For 4K
frames the byte cap bites at a depth of three or four; for 40-byte audio packets
the item cap bites at 64.

**A full wire does not block its producer; it makes the producer unrunnable.**
There is no blocking primitive in the crate — no channel, no mutex, no condvar,
no park, no sleep — outside `driver.rs`'s worker pool. Combined with picking the
most downstream runnable node first (mux 4, encode 3, filter 2, decode 1,
demux 0), a pipeline that reads faster than it writes simply stops reading, and
queues stay shallow in addition to being hard-bounded. `queues_stay_shallow…`
asserts a peak of ≤ 8 items on a 200-packet run with a bound of 64.

One rule earns its own paragraph: **an empty wire always has room**, even for an
item larger than its byte cap. Without it, a frame bigger than `max_bytes` is
unschedulable and the pipeline stops with every queue empty — a deadlock caused
by the mechanism that exists to prevent unbounded memory.

Peak occupancy may exceed the item cap by a component's own expansion: a decoder
admitted with one packet may emit its whole reorder delay. That overshoot is
bounded by the codec's declared delay; the `Budget` is the hard ceiling under
it, and exceeding *that* is `Error::LimitExceeded`, never a stall.

### End of stream

Uniform rule: **a node that has seen end of stream on its inputs sends `None`
into its component, drains until the component reports `Eof`, emits everything
it got, and only then closes its outputs.** A decoder's reorder delay, a
filter's window and an encoder's lookahead all come out of that drain.

Two details that are easy to get wrong and are handled explicitly:

- A node that is holding an input the component refused (`OutputPending`) must
  not act on the end marker yet, and the planner delivers each port's end
  exactly once — so `CodecWork::pending_eof` and `FilterWork::pending_eof`
  *remember* it. Forgetting is how a pipeline hangs one item short.
- The muxer writes its trailer only when every input port is finished **and**
  the interleave queue has released everything. Writing it early is the classic
  remux corruption bug.

### Cancellation is two things

- `Pipeline::cancel()` — abort. Sets the shared `vaco_codec_core::CancelToken`
  (the one that already exists; D19 forbids a third). No trailer is written, so
  a truncated output cannot pass for a finished one.
- `Pipeline::stop_reading()` — graceful. Demuxers close their ports at their
  current position, codecs drain, the trailer is written, and the result is a
  valid, shorter file. This is the mechanism `-t`, `-frames`, `-fs` and
  `-shortest` need.

### Timestamps

Every stage boundary is a time-base change. `timing.rs` has one function per
item kind, both use `Rounding::NearestAwayFromZero`, and nothing else in the
crate rescales. That is `vaco_core::Rounding`'s own default and what
`MuxTimestamps`'s M1 step applies, so the pipeline-side hops and the muxer-side
chain agree. Rescaling happens exactly once per item, when a consumer takes it
out of a wire — the wire records the producer's base, the input port records the
consumer's.

Muxer-side ordering is *not* reimplemented here, and — since gap 8
 closed — no longer partially reimplemented
either. `node::MuxWork` used to drive a raw `Box<dyn Muxer>` directly rather
than going through `vaco_format_core::mux::MuxBuilder`/`MuxWriter`, because
that wrapper consumes `self` at each phase transition and a step-driven node
needs to keep *something* around between calls to `advance`. The fix is not
to avoid that consumption but to place it correctly: `PipelineSpec` now holds
a `MuxBuilder` per output from `add_output`/`add_output_with` onward —
`PipelineSpec::map` calls `MuxBuilder::add_stream` instead of
`Muxer::add_stream` directly — and `PipelineSpec::build` consumes it with one
call to `MuxBuilder::open`, handing the resulting `MuxWriter` to `MuxWork`.
`MuxWork` itself shrank to a thin driver: `Option<MuxWriter>`, `write_packet`
and `end_stream` per port, and `finish` once every input is done. M1–M11 (the
whole state machine), M15 (`query_codec`) and M30 (`set_metadata`'s ordering)
all now come from `vaco-format-core` rather than being re-derived here — which
is what closed all three faces of gap 8 in one change:

- **M12** (`init` runs once, after every stream is declared, before anything
  reads a time base) is `MuxBuilder::open`'s job now, not `build_work`'s own
  hand-rolled call to `muxer.init()?` ahead of a `stream_time_base` loop. Same
  ordering, moved to where the framework already tests it.
- **M30** (`set_metadata` runs after streams and time bases are settled, but
  before the header) is `MuxBuilder::open`'s as well.
  `PipelineSpec::set_output_metadata` attaches the metadata to the builder at
  any point before `build`; the call itself can happen before or after
  `map`, because the ordering that reaches the muxer is fixed by `open`, not
  by call order at this layer. This is the fix for gap 8a: the CLI used to
  call `Muxer::set_metadata` directly, before `add_output` had taken the
  muxer, because there was no later point to reach it from — see
  `docs/app/vaco-cli.md`.
- **M15** (`query_codec`, asked before the muxer does anything) now actually
  runs: `PipelineSpec::map` calls `MuxBuilder::add_stream`, which asks
  `query_codec` first and returns `Error::Unsupported` before the raw
  `Muxer::add_stream` is ever reached. Previously `map` called
  `Muxer::add_stream` directly, and the check — implemented, tested — was
  simply never asked.
- **M6** (the bitstream-filter stage, `BsfChain`/`BsfProvider`) now runs too:
  every packet handed to `MuxWriter::write_packet` passes through
  `check_bitstream`/`BsfChain` before `Muxer::write_packet` sees it.
  `PipelineSpec::set_output_bsfs` is the seam for supplying real filters; no
  caller in this workspace calls it yet (see the gotcha below), so every
  output still runs M6 against `vaco_format_core::mux::NoBsfs`, which is
  correct — no muxer's `check_bitstream` asks it for anything — but is not
  the same claim as "a real bitstream filter now converts a stream that
  needed it". No `vaco-bsf-*` crate exists in this workspace yet, and neither
  `vaco-mux-avi` nor `vaco-mux-mpegts` (which each carry their own inline
  length-prefix-to-Annex-B conversion) call `check_bitstream` to ask for one
  — they use the trait's default (`Keep`), so M6 is a no-op for them
  regardless of which path drives it. Their inline conversions are **not**
  dead code from this change; closing that requires a filter crate, a
  `BsfProvider`, and those two crates' own `check_bitstream` to start asking
  — three changes outside this crate, done together so a bisect stays
  possible if remuxing breaks.

`build_work`'s `KindSpec::Mux` arm is now three lines: take the `MuxBuilder`
out of the (by-then-consumed) `NodeSpec`, call `.open()`, wrap the result. The
`Result<Work>` return type `build_work` already had for the M12 fix carries
the `?`.

### Why it cannot deadlock

1. The state machine has no blocking primitive, so a stall is a *scheduling*
   state, not a parked thread. `step` returns `Advance::Idle` and `classify`
   returns `Finish::Stalled(Vec<StallReport>)` with a per-node reason.
2. `vaco_limits::ProgressGuard` turns a livelock — steps that change nothing —
   into `LimitError::NoProgress` after 64 consecutive stalls.
3. The graph is acyclic by construction, wires are single-producer
   single-consumer, and an empty wire always has room, so some node downstream
   of any backlog is always runnable.
4. The threaded driver dispatches exactly `k` jobs and receives exactly `k`
   replies; a drop guard sends a reply even if a job unwinds, so the count can
   never come up short.

## How to change it

| You want to | Touch |
|---|---|
| Add a node kind | `node.rs`: a `Work` variant, its `ready`, `batch` and `advance`; `spec.rs`: a `KindSpec` variant and a builder method; `pipeline.rs`: `build_work` and the priority table |
| Change the scheduling policy | `Pipeline::check_out` — the priority constants are in `PipelineSpec::build`'s `match` |
| Change what "full" means | `wire::Capacity` and `Wire::has_room`. Read the empty-wire paragraph above first |
| Add per-edge capacities | `Wire::new` already takes one; only `PipelineSpec` says "one for all" |
| Add a driver | `driver.rs`. A driver may only use `begin_step`, `check_out`, `check_in`, `end_step` and `classify`; anything needing more is a state-machine change, not a driver |

Gotchas:

- `Graph::send` takes the frame **by value** and documents `OutputPending` as
  "retry with the same one", but the frame has already been moved in and dropped
  by then. `FilterWork::advance` works around it by copying the frame first —
  only when `source_wants` is false, so the copy is off the hot path.
- A filter node that cannot hand a frame to its graph must still *run* the
  graph. Returning early instead is a livelock: draining the graph's sinks is
  what makes room for the frame being held.
- Adding a node kind that expands its input (N:M) must keep `batch() == 1`, or
  the wire bound stops meaning anything.
- `KindSpec::Mux::builder` is `Option<Box<MuxBuilder>>`, not `Box<MuxBuilder>`.
  Not defensive padding: `MuxBuilder::with_metadata`/`with_bsfs` consume
  `self` and return a new `MuxBuilder`, and `PipelineSpec::set_output_metadata`/
  `set_output_bsfs` have only `&mut self` on the spec to work with — the
  `Option` is what lets them `take()` the value out, call the consuming
  method, and put the result back. It reads `Some` at every point a caller of
  this crate can observe it.
- Nobody calls `PipelineSpec::set_output_bsfs` yet. It exists so the seam is
  wired end to end, but until some crate implements `BsfProvider` for a real
  filter (there is no `vaco-bsf-*` crate in this workspace) every output's M6
  stage runs against `NoBsfs`. Do not read "the plumbing compiles" as "a
  stream needing conversion now gets one" — check the muxer's own
  `check_bitstream` too.

## Configuration

| Knob | Where | Default |
|---|---|---|
| Queue bound | `PipelineSpec::with_capacity(Capacity)` | 64 items / 16 MiB |
| Memory ceiling | `PipelineSpec::with_limits(Limits)` | `Limits::permissive()` (1 GiB) |
| Recoverable demux errors per input | `PipelineSpec::with_max_input_errors` | 64 |
| Threads | `Driver::with_threads(n)` | 1 (`Driver::serial`) |
| Output-side format options | `PipelineSpec::add_output_with(muxer, &options)` | `FormatOptions::default()` (container flags always come from `Muxer::flags()`, via `MuxBuilder::new` — never a parameter) |
| Output metadata (`-metadata`) | `PipelineSpec::set_output_metadata(output, metadata)` | none |
| Output bitstream filters (M6) | `PipelineSpec::set_output_bsfs(output, bsfs)` | `vaco_format_core::mux::NoBsfs` |
| Livelock tolerance | `ProgressGuard::DEFAULT_MAX_STALLS` | 64 |

## D18: why this shape

D18 requires every library to build for `wasm32-unknown-unknown`, which has no
threads, and requires parallelism to be optional **at the API level, not merely
feature-gated at the call site**.

A step function satisfies that with no conditional in the caller:
`Driver::with_threads(4).run(&mut pipeline)?` compiles and runs on every target.
Where threads are unavailable the count is clamped and `Driver::threads()`
reports what was actually granted. Both drivers run the same state machine, so a
bug found under one reproduces under the other and the wasm build is not a
second code path nobody exercises.

`std::thread` and `std::sync::mpsc`, not `rayon` and not an async runtime:

- plan 14 §7.1 reserves `rayon` for data parallelism *inside* a stage and states
  the invariant that no `rayon` closure performs a queue operation — easiest to
  keep by not having the dependency here;
- `rayon` on wasm is a `NATIVE_ONLY` problem, and D18's allowlist default exists
  so OS coupling does not spread;
- the workload is CPU-bound with under ten stages and almost no waiting, so an
  executor would multiplex something we do not have while colouring every trait
  in the tree, and D10 makes such an adoption a reviewed decision.

## Measurements

`benches/pipeline.rs` (divan). All figures are medians on one machine; the
ratios are the point, not the absolute times.

**The threaded driver was rebuilt because the first design measured backwards.**
A `std::thread::scope` per wave — the simplest correct design, with no channel
in it at all — was **45x to 60x slower than serial** (300 µs against 14–19 ms on
a 200-packet transcode) and stayed slower at every job grain. A 200-packet
transcode is roughly 800 jobs, and a thread spawn is tens of microseconds. That
is plan 12's PF-0.x pattern for the fifth time on this project.

The replacement spawns `n` workers once per run. Threads against serial,
120-packet transcode, per-item work varied:

| work/item | serial | 2 threads | 4 threads |
|---|---|---|---|
| ~0 | 193 µs | 2.64 ms (**13.7x slower**) | 1.64 ms (**8.5x slower**) |
| ~4 µs | 781 µs | 3.00 ms (3.8x slower) | 2.06 ms (2.6x slower) |
| ~21 µs | 5.39 ms | 7.31 ms (1.36x slower) | 4.40 ms (**1.22x faster**) |
| ~210 µs | 51.1 ms | 40.8 ms (1.25x faster) | 27.6 ms (**1.85x faster**) |

Break-even for four threads is around **20 µs of work per job**. A 1080p H.264
frame decode is 1–10 ms, so threads are right for transcoding and wrong for
remuxing — and the peak observed speedup is 1.85x on four threads, not 4x,
because planning and committing are serial.

Two hypotheses that did not survive:

- *Shallow queues cost planning passes.* Refuted: capacity 1, 2, 4, 16 and 64
  are within noise of each other (296–330 µs).
- *Readiness scanning is quadratic in stream count.* Overstated: per-packet cost
  goes 0.55 µs → 0.61 µs → 1.00 µs → 2.04 µs for 1, 4, 16 and 64 streams — a
  3.7x rise across a 64x rise in stream count, closer to `sqrt(n)` than to `n`.

`classify()` on an idle pipeline costs 166 ns.

## What it does not do yet

The full list, with whether the API accommodates each later, is the table in the
crate-level docs (`src/lib.rs`). The short version: `-re`, `-shortest`, `-fs`,
`-t`/`-ss`/`-frames`, `-copyts`, per-edge capacities, thread-count tuning and
seek are all additive. Mid-stream parameter change adds a variant to a public
enum. **Loopback decoders `[dec:N]` are the one that needs a design change**,
because the builder's acyclicity is structural.

## Dependencies

- `vaco-core` — `Error`, `Rational`, `Timestamp`, `Rounding`, `rescale_rnd`
- `vaco-limits` — `Budget` (the memory ceiling), `ProgressGuard` (the livelock guard)
- `vaco-packet`, `vaco-frame` — what travels on a wire
- `vaco-codec-core` — `Decoder`, `Encoder`, `Stage`, `CancelToken`, `CodecParameters`
- `vaco-filter-core` — `Graph`, `GraphStatus`, `LinkFormat`, `NodeId`
- `vaco-format-core` — `Demuxer`, `Muxer`, `Stream`, `FormatOptions`, and (since gap 8) `mux::{MuxBuilder, MuxWriter, BsfProvider}`, `metadata::MuxMetadata`. `MuxTimestamps`/`InterleaveQueue` are no longer named directly here — `MuxBuilder`/`MuxWriter` own them now.

No external crates at all. Dev only: `proptest`, `divan`, `vaco-pixfmt`,
`vaco-pool`.
