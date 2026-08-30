# Frame threading

## What it is

Several pictures decoding at once inside one decoder, opt-in through
`-threads N`, with output bit-identical to the single-threaded decoder at every
thread count. Implemented for H.264 (`vaco-codec-h264`); the framework it is
built on (`vaco-codec-core`) is codec-agnostic, so HEVC and VP9 can adopt it
without a second mechanism.

**It is off by default.** `-threads` unstated, or `-threads 1`, spawns no
threads at all and runs the identical call sequence the decoder ran before this
existed. See "Should it be on by default" below for the measured reason.

## How it works

### The decoder is split in two, not shared between threads

The conventional design propagates decoder state between per-thread contexts.
This one has no shared mutable state to propagate:

| half | what it owns | where it runs |
|---|---|---|
| **serial** (`H264Decoder::split_packet`) | parameter sets, the DPB, reference-list construction, clause 8.2.5 marking, POC, the reorder buffer | the caller's thread, strictly in decode order |
| **parallel** (`H264FrameTask`) | one picture's clause 8.4/8.5 reconstruction, clause 8.7 deblocking, and the crop into a `Frame` | any worker |

The seam is where it is for two reasons, both measured:

- **The parallel half is where the time goes.** `planning/E2E-GAPS.md` §19's
  profile puts reconstruction plus the two deblocking passes at roughly 55% of
  self time before the long tail, against about 9% for entropy decoding.
- **Only the parallel half needs reference *pixels*.** Entropy decoding needs
  the co-located picture's *motion field* — metadata the serial half already
  holds the moment that picture's slice was decoded — and never its samples. So
  the serial half runs ahead of the pixels, and the only dependency graph a task
  waits on is the reference-picture graph.

A task is `Send + 'static` by construction: every field is owned data or an
`Arc`. That bound is the design. A task that can be moved to another thread and
outlive the call that made it cannot be holding a borrow of decoder state, and
the compiler checks it — no `unsafe`, no raw pointers, no hand-rolled
synchronisation.

### Determinism has exactly two mechanisms

1. **`FrameRunner::collect` returns results in dispatch order, never completion
   order.** Every *ordering* decision — pushing to the reorder buffer, bumping
   the lowest-POC picture out of it, an IDR's flush — is applied in
   `H264Decoder::collect_one`, in decode order, on the frames as they are
   collected. The reorder buffer therefore sees exactly the sequence it saw
   single-threaded, whatever order the workers finished in.

   The IDR flush is the case that makes this concrete. Single-threaded, an IDR
   flushes the reorder buffer before it decodes. Under threading, at that point
   the earlier pictures' frames *do not exist yet*, so flushing there would emit
   a shorter and differently ordered sequence. The decision is recorded at split
   time (`InFlight::flush_reorder_first`) and acted on at collection.

2. **`ProgressPicture` bands are published once and read only after
   publication.** A writer owns a band exclusively while filling it, then
   *moves* it into a `OnceLock` — release on `set`, acquire on `get` — which is
   the point where it stops being mutable and starts being shared.
   `PictureWriter` is neither `Sync` nor `Clone`, so exactly one task holds it;
   `PictureRef` is `Send + Sync` and read-only. A task cannot read a sample that
   has not been written because there is no type in the API that lets it.

Nothing else is needed, and in particular nothing about the decode *arithmetic*
changes: a threaded run computes the same values in the same order within each
picture. `-threads N` changes when work happens, never what is computed.

### Why the pool cannot deadlock

Tasks leave one shared queue in dispatch order, and a task only ever waits on
pictures earlier in decode order (`TaskCtx::wait_rows` debug-asserts it). So the
lowest-indexed in-flight task has every predecessor already finished and never
blocks — some worker is always making progress, however few workers there are
relative to pictures in flight.

Two failure paths are closed explicitly: a task that returns `Err` drops its
`PictureWriter`, which marks every plane failed and wakes each waiter with an
error rather than leaving it parked; and a task that *panics* still produces
exactly one reply, from a drop guard, so the collector never waits for a message
nobody will send.

### Granularity: picture, not row — and this is the limit

A reference picture is published in **one band**, once, after clause 8.7's two
whole-picture deblocking passes have swept it. Until then no row of it is final,
and a later picture must not predict from undeblocked samples.

That is the binding constraint on how much parallelism is available, and it is
worth being precise about why. With picture granularity, picture `N + 1` cannot
start until picture `N` is completely finished. On content whose pictures form a
serial chain — every P frame referencing the one before it — that leaves nothing
to overlap except the serial half's own work against the parallel half's, which
is a two-stage pipeline and caps out near `1 / (1 - serial_fraction)`.

**Both of this project's large H.264 fixtures are exactly that content.**
`uhd.mp4` is 1 I frame and 74 P frames; `big.mkv` is 8 I frames and 1792 P
frames. Neither has a single B frame, so neither has any picture-level
parallelism to find. ffmpeg's ~3.5x on the same files is therefore **entirely
row-level** frame threading: its picture `N + 1` begins reconstructing as soon as
picture `N` has produced enough rows to cover the motion-vector reach.

Moving to row granularity is the identified next step and the machinery is
already here — `PictureSpec::with_band_height`/`with_guard`, `publish_through`
per band, `PlaneView::block`'s guard-row fast path. What it needs first is two
changes this pass deliberately did not make:

- **Deblocking must become incremental.** `deblock_picture_luma`/`_chroma` are
  whole-picture passes today. Rows of macroblock row `r - 1` are only final once
  macroblock row `r`'s top-edge filtering has run, so publication has to be
  driven from an interleaved reconstruct-then-deblock loop, not from two sweeps
  at the end.
- **Reference reads must become block reads.** A banded plane is not one
  allocation, so `sample_luma_block` and `predict_chroma_inter` cannot keep
  taking a flat `&[u8]` per plane; they would fetch a `BlockRef` per partition
  through `PlaneView::block` instead. That is a rewrite of the two hottest
  functions in the decoder (`planning/E2E-GAPS.md` §19: 15.87% and 9.87% of self
  time), and this document's own record of five reverted inner-loop attempts is
  the reason it is not being folded into the same change as the threading
  scaffolding.

Safe Rust is not the obstacle to row granularity — `ProgressPicture` already
expresses it, and the `frame_runner` tests exercise it. The obstacle is that
contiguity and progressive publication are genuinely incompatible (a writer
cannot hold `&mut` to rows above `R` while a reader holds `&` to rows below `R`
of the same allocation), so progressive publication forces the reader onto a
block API, and that reader is the decoder's hot loop.

## Memory

`N` concurrent pictures multiply the footprint, and the accounting says so in
one place. Per picture, charged to the decoder's own `vaco_limits::Budget`:

| what | charged | released |
|---|---|---|
| a DPB entry's samples (`ProgressPicture`) plus its motion field | `split_packet`, at allocation | eviction — sliding window, MMCO, an IDR's clear, or `flush` |
| an in-flight task's working picture and output frame | `split_packet`, before dispatch | `collect_one`, including on the error path |

The task's own `Budget` exists only to apply the per-allocation caps
(`max_alloc_single`, `max_frame_bytes`) to its individual allocations; the
aggregate across every in-flight picture is the coordinator's charge above,
because that is the number `-threads N` actually multiplies.

**The macroblock array is the big one, which is not what the type names
suggest.** At 4K a coded picture is 12.4 MB and the cropped output frame is
about the same, but `SliceStats::macroblocks` is `MbSummary` × 32,400
macroblocks at 1,888 bytes each — **59 MiB**, five times the two sample buffers
together, because every macroblock carries its full residual and its sixteen 4×4
motion blocks. Measured with `/usr/bin/time -l` on the 4K fixture: peak RSS 2854
MiB at one thread, 3321 MiB at eight, and 8 × 59 MiB accounts for essentially
all of that ~470 MiB. It is charged. The two sample buffers are charged as two
whole coded pictures rather than exactly — one for the reconstruction planes,
one covering the cropped frame's row-stride padding — a deliberate over-estimate
and the one figure here that is not a measurement.

**Memory pressure is backpressure, not a failed decode.** Nine in-flight 4K
pictures are ~756 MiB against `Limits::permissive`'s 1 GiB — it fits by margin,
not by design, and 8K or a tighter `-max_alloc` would not.
`H264Decoder::split_packet` therefore finishes pictures until the next one's
charge fits *before* allocating anything for it, so `-threads N` is an upper
bound on concurrency rather than a demand, and a tight budget costs speed rather
than the decode. Same rule `vaco-sched`'s wires already encode: never let the
mechanism that exists to bound memory be the thing that stops the pipeline. If
nothing is in flight and one picture still does not fit, the reservation reports
it honestly — waiting cannot change that.

## Configuration

| knob | where | default |
|---|---|---|
| threads | `-threads N` (CLI), `Decoder::set_thread_count` (API) | 1 |
| pictures the serial half may run ahead by | `H264Decoder::max_in_flight` | `threads + 1`, or 1 at one thread |
| the ceiling that actually binds under pressure | `Limits::max_alloc_total` (`-max_alloc`) | 1 GiB (`Limits::permissive`, the CLI default) |
| band height / guard rows | `PictureSpec::with_band_height` / `with_guard` | one band, no guard (`single_band`) |

Measured scaling, for choosing a count (medians, interleaved, wall clock):
1.23x / 1.26x / 1.30x at 2 / 4 / 8 threads on an all-P 4K clip, and 1.78x /
2.00x / 1.99x on a stock-`libx264` B-pyramid 1080p clip. CPU utilisation
flatlines at 129% on the first and 219% on the second, so **above four threads
there is nothing left to gain at this granularity** — see
`planning/E2E-GAPS.md` §20 for the full tables and why.

`-threads` is a **global** CLI option here, not the reference's per-stream
`AVCodecContext` one: vaco has no per-codec option store yet, and stating it once
is the overwhelmingly common use. `-threads:v:0 4` is not accepted. Zero means
one, not the reference's "auto" — nothing here auto-detects, and a default that
depended on the machine would make a run's output provenance depend on it too.

## How to change it

| you want to | touch |
|---|---|
| Add frame threading to another codec | Implement the split: a `FrameTask` holding owned data plus `PictureRef`s, a serial half that allocates the `ProgressPicture` and dispatches, and `set_thread_count`. `vaco-codec-h264`'s `frame_task.rs` + `decoder.rs` is the worked example |
| Move to row granularity | `crate::deblock` (incremental), `crate::reconstruct`'s reference reads (`PlaneView::block`), then one `PictureSpec` and a `publish_through` per macroblock-row group. Read the granularity section above first |
| Change the look-ahead depth | `H264Decoder::max_in_flight`. It costs one coded picture plus one frame of budget per slot |
| Add a *new* threading axis | Don't, without reading `vaco_codec_core::threading`'s three-axis note and `docs/app/vaco-sched.md`. `vaco_sched::Driver::with_threads` is pipeline-stage parallelism — whole stages of `demux → decode → filter → encode → mux` running concurrently — and buys nothing on a decode-bound single-stream job, because one decoder is one stage. Frame threading is *inside* that stage and invisible to the scheduler. They compose; they are not alternatives, and neither replaces the other |

## Dependencies

`vaco-codec-core` (`FrameRunner`, `FrameTask`, `TaskCtx`, `Threading`, and
`picture::{ProgressPicture, PictureWriter, PictureRef, PlaneView}`),
`vaco-limits` (`Budget`), `std::thread` + `std::sync::{mpsc, Mutex, Condvar}`.
No `rayon` and no async runtime — plan 14 §7.1 reserves `rayon` for data
parallelism inside a stage and `vaco-sched` declines it for the same reasons.
