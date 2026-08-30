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

### Granularity: rows

A reference picture is published **band by band as its rows become final**, and
the picture after it starts predicting from those rows rather than waiting for
the whole picture. That is what makes an all-P stream — a serial chain of
pictures, which is what both of this project's large fixtures are — parallel at
all. At picture granularity it is not: picture `N + 1` cannot start until
picture `N` is completely finished, CPU flatlines at 129% however many threads
are offered, and the ceiling is `1 / (1 - serial_fraction)` ≈ 1.3x. At row
granularity the same fixture reaches **4.05x at eight threads and 615% CPU**.

Three boundary conditions decide it, and each is exact rather than
conservative. Getting any of them merely "safe" costs speed; getting one wrong
in the other direction publishes not-quite-final samples, and that corruption
would be subtle and content-dependent. They are stated here, and each is pinned
by a test.

#### 1. The filter lags reconstruction by exactly one macroblock row

Clause 8.7's filter is interleaved into the macroblock-row loop
(`reconstruct::PictureReconstructor`), one row behind. It has to be *behind*,
because clause 8.3's intra prediction is defined on **unfiltered** neighbours;
and one row is enough, because the only rows it reads above the current
macroblock row are the single luma row `my * 16 - 1` and chroma row
`my * 8 - 1`, which filtering row `my - 1` is exactly what rewrites (its
vertical edges touch all sixteen of its own luma rows, the last of which *is*
`my * 16 - 1`). Filtering row `my - 1` needs nothing from row `my`, so the lag
is one and no saved copy of the unfiltered top border is needed. A lag-zero
schedule would need one.

#### 2. A row is final only after the *next* row has been filtered

Filtering macroblock row `d` writes **upwards** into the row above it. Luma's
top macroblock edge at `y = d * 16` rewrites `p0`, `p1` and `p2` — rows
`d * 16 - 1`, `- 2` and `- 3`. Chroma's rewrites `p0` alone, row `d * 8 - 1`.
So once row `d` is filtered:

| plane | rows final |
|---|---|
| luma | `reconstruct::luma_rows_final(d) = d * 16 + 13` |
| chroma | `reconstruct::chroma_rows_final(d) = d * 8 + 7` |

`deblock.rs`'s own tests assert that extent from both sides: nothing outside
`d * 16 - 3 ..= d * 16 + 14` moves, **and** row `d * 16 - 3` really does move —
an overhang that were only hypothetical would make the watermark needlessly
conservative and the test vacuous.

#### 3. The wait is derived from the motion vectors, per macroblock row

Before reconstructing macroblock row `my`, `reconstruct::row_reference_reach`
walks that row's own motion vectors and reports, per reference and per plane,
the deepest row clause 8.4.2.2 will actually read:

| | deepest row read | why |
|---|---|---|
| luma | `y + (mv_y >> 2) + 6` | clause 8.4.2.2.1's six-tap reads two rows above the 4x4 block and three below its last one |
| chroma | `cy + (mv_y >> 3) + 2` | clause 8.4.2.2.2's bilinear reads one row below each of the two chroma sub-positions |

Those are the same numbers `sample_luma_block` and `sample_chroma_2x2` use to
size the region they ask a banded plane for, so the bound cannot drift away
from the read. A reference the row does not predict from is not waited on at
all. And a read past what was waited for is *refused* by `PlaneView::block`
rather than served, raising `ReadScratch`'s failure flag, which
`PictureReconstructor` turns into an error at the end of the row — so a bound
that was ever too small is an error, never wrong pixels.

### Reading a plane that is not one allocation

Progressive publication and contiguity are genuinely incompatible under
ordinary borrow rules — a writer cannot hold `&mut` to rows above `R` while a
reader holds `&` to rows below `R` of the same allocation — which is what
`ProgressPicture`'s bands exist to solve. The cost is that a banded plane
cannot be handed to the reader as a `&[u8]`, and the reader is the decoder's
hot loop (`planning/E2E-GAPS.md` §19: `sample_luma_block` 15.87% and
`predict_chroma_inter` 9.87% of self time).

`reconstruct::RefPlane` is where that lands, and it has two arms:

| arm | when | what a read costs |
|---|---|---|
| `Flat(&[u8])` | one allocation: `-threads 1`, and every test oracle | exactly the indexed fetch this decoder has always done — the same instructions, not "almost" |
| `Banded(PlaneView)` | published band by band | one `PlaneView::block` per 4x4 block, then the same arithmetic against the borrow it returns |

Both arms feed the same clause 8.4.2.2 code in `crate::interp`; only the fetch
closure differs, which is what the in-picture and edge-clamped fetches already
did to each other. **`-threads 1` therefore pays nothing at all for row
granularity** — not the guard rows' memory, not the copy that fills them, and
not the block API. Measured: median ratio 1.009 against the picture-granularity
binary over 10 interleaved single-threaded launches.

The banded arm was the identified first-class regression risk of this work, and
it measured the other way. Rewriting the two hottest functions to take a plane
rather than a slice came with hoisting chroma's per-point fetch closure to the
2x2 group a 4x4 block needs — a banded reference must be asked for their shared
region once, not four times — and that alone was a **2.9% single-threaded
speedup** (9 of 10 interleaved rounds).

### Band height and guard depth

| | value | why |
|---|---:|---|
| band height | 32 rows | publication latency against the guard's cost. The guard is a fixed 8 rows per band, so a 32-row band carries 25% overhead in memory and in the copy that fills it; the *coarsest* progress a reader sees is one chroma band, which at 4:2:0 is 64 luma rows — four macroblock rows |
| guard | 8 rows | **exact.** Clause 8.4.2.2.1's six-tap reads a 9-row region for a 4x4 block. A read of `h` rows straddles a seam at `F` exactly when its first row is in `F - (h - 1) ..= F - 1`, so `h - 1 = 8` guard rows are what make every such read land inside the next band's own allocation. Seven pushes those reads onto the copy path; nine costs memory for nothing |

Both halves of the guard argument are pinned in `vaco-codec-core`'s own tests:
a 9-row read at *every* row of a band-32/guard-8 plane is borrowed rather than
copied, and the same read on a guard-7 plane is copied.

The band height is a power of two so that `ProgressPlane::band_of` — which runs
on every block read — is a shift rather than a division by a runtime value.
`single_band` rounds its own height up to a power of two for the same reason.

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
motion blocks. It is charged. The two sample buffers are charged as two whole
coded pictures rather than exactly — one for the reconstruction planes, one
covering the cropped frame's row-stride padding — a deliberate over-estimate and
the one figure here that is not a measurement.

**Guard rows are the only thing row granularity adds, and they are small.** A
banded DPB entry carries 8 guard rows per 32-row band, so its sample planes cost
25% more than the picture — 3.1 MB on top of 12.4 MB at 4K — against 59 MiB of
macroblock array per in-flight picture. Measured with `/usr/bin/time -l` on the
4K fixture, same session: peak RSS at eight threads is 4192 MiB at picture
granularity and 4304 MiB at row granularity, a 2.7% difference, and one thread
is 3614 MiB either way because at one thread there are no guard rows at all.

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
| band height / guard rows | `decoder::ROW_BAND_HEIGHT` / `ROW_BAND_GUARD`, via `PictureSpec` | 32 / 8 above one thread; one band, no guard, at one thread |

Measured scaling, for choosing a count (medians of 8 interleaved launches, wall
clock, decode only):

| threads | 4K all-P `uhd.mp4` | CPU | B-pyramid 1080p | CPU |
|---:|---:|---:|---:|---:|
| 1 | 7.020s — 1.00x | 100% | 9.726s — 1.00x | 100% |
| 2 | 3.569s — 1.97x | 236% | 5.332s — 1.82x | 220% |
| 4 | 2.285s — 3.07x | 447% | 3.241s — 3.00x | 439% |
| 8 | 1.907s — 3.68x | 625% | 2.445s — 3.98x | 742% |

**Four threads is where the curve stops being nearly linear**; eight buys
another 15–25% for roughly 40% more CPU. See `planning/E2E-GAPS.md` §21 for the
full tables, the ffmpeg-relative ratios and the memory numbers.

`-threads` is a **global** CLI option here, not the reference's per-stream
`AVCodecContext` one: vaco has no per-codec option store yet, and stating it once
is the overwhelmingly common use. `-threads:v:0 4` is not accepted. Zero means
one, not the reference's "auto" — nothing here auto-detects, and a default that
depended on the machine would make a run's output provenance depend on it too.

## Should it be on by default

**Not yet, and the reason is no longer "it does not help".** It helps a great
deal: 3.07x at four threads on the all-P 4K clip and 3.00x on the B-pyramid one,
where the previous, picture-granularity answer was 1.26x and 2.00x. The first of
the three conditions the picture-granularity pass named — "row granularity
should land first" — is met, and the content shape that dominates this
project's corpus is exactly the one that now benefits most.

Three things should still be true first, and none of them is "more confidence":

1. **The per-picture budget charge should be exact.** It is still a deliberate
   2x over-estimate (two whole coded pictures, one of them standing in for the
   cropped frame's row-stride padding). That is fine while the feature is
   opt-in and wrong once it is not, because a default thread count multiplies it
   by `threads + 1`. Unchanged from the previous pass's own condition.
2. **The fuzz target should decode at more than one thread.** `h264_decode`
   never calls `set_thread_count`, so the row-progress path — the publisher, the
   per-row waits, `PlaneView::block`'s banded arm — has **no fuzz coverage at
   all** today. It is covered by the repository's own invariance tests and by
   many repetitions against the ffmpeg oracle, which is not the same thing.
   Making a decode path implicit while nothing fuzzes it is the wrong order.
3. **The count must be a fixed small bound, not `ncores`.** `min(ncores, 4)` is
   where the measured curve stops being nearly linear, and eight buys another
   15–25% for roughly 40% more CPU. Output is byte-identical at every thread
   count, so this does not make a decode machine-dependent — but the *memory*
   ceiling would be, and `min(ncores, 4)` bounds it at five in-flight pictures,
   about 420 MB at 4K against `Limits::permissive`'s 1 GiB.

## How to change it

| you want to | touch |
|---|---|
| Add frame threading to another codec | Implement the split: a `FrameTask` holding owned data plus `PictureRef`s, a serial half that allocates the `ProgressPicture` and dispatches, and `set_thread_count`. `vaco-codec-h264`'s `frame_task.rs` + `decoder.rs` is the worked example |
| Change the band height or guard | `decoder::ROW_BAND_HEIGHT` / `ROW_BAND_GUARD`. Read "Band height and guard depth" first — the guard is derived from clause 8.4.2.2.1's filter reach, not chosen, and `vaco-codec-core`'s tests will say so |
| Change what a task waits for | `reconstruct::row_reference_reach`, and the two region sizes in `sample_luma_block`/`sample_chroma_2x2` that it must agree with |
| Change the look-ahead depth | `H264Decoder::max_in_flight`. It costs one coded picture plus one frame of budget per slot |
| Add a *new* threading axis | Don't, without reading `vaco_codec_core::threading`'s three-axis note and `docs/app/vaco-sched.md`. `vaco_sched::Driver::with_threads` is pipeline-stage parallelism — whole stages of `demux → decode → filter → encode → mux` running concurrently — and buys nothing on a decode-bound single-stream job, because one decoder is one stage. Frame threading is *inside* that stage and invisible to the scheduler. They compose; they are not alternatives, and neither replaces the other |

## Dependencies

`vaco-codec-core` (`FrameRunner`, `FrameTask`, `TaskCtx`, `Threading`, and
`picture::{ProgressPicture, PictureWriter, PictureRef, PlaneView}`),
`vaco-limits` (`Budget`), `std::thread` + `std::sync::{mpsc, Mutex, Condvar}`.
No `rayon` and no async runtime — plan 14 §7.1 reserves `rayon` for data
parallelism inside a stage and `vaco-sched` declines it for the same reasons.
