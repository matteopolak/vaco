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

Muxer-side ordering is *not* reimplemented here: `MuxTimestamps` (M1–M4) and
`InterleaveQueue` come from `vaco-format-core`.

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

## Configuration

| Knob | Where | Default |
|---|---|---|
| Queue bound | `PipelineSpec::with_capacity(Capacity)` | 64 items / 16 MiB |
| Memory ceiling | `PipelineSpec::with_limits(Limits)` | `Limits::permissive()` (1 GiB) |
| Recoverable demux errors per input | `PipelineSpec::with_max_input_errors` | 64 |
| Threads | `Driver::with_threads(n)` | 1 (`Driver::serial`) |
| Container flags and options | `PipelineSpec::add_output_with` | `FormatFlags::empty()`, `FormatOptions::default()` |
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
- `vaco-format-core` — `Demuxer`, `Muxer`, `Stream`, `MuxTimestamps`, `InterleaveQueue`, `FormatFlags`, `FormatOptions`

No external crates at all. Dev only: `proptest`, `divan`, `vaco-pixfmt`,
`vaco-pool`.
