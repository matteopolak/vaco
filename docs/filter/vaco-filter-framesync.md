# `vaco-filter-framesync`

Layer 5b. The frame synchroniser: what multi-input filters use to put several
streams on one timeline, and the `eof_action` / `shortest` / `repeatlast` /
`ts_sync_mode` surface that has to behave identically on all sixty-eight of
them.

It is a crate rather than a helper for exactly that reason: `overlay`, `blend`,
`lut2`, `hstack`, `psnr`, `maskedmerge`, `mix`, `paletteuse` and the rest are
written by different people at different times, and a user who learns
`eof_action=pass` on one must get the same behaviour on all of them.

---

## What it is

| Module | Contents |
|---|---|
| `opts` | the four options and the per-input modes they compile to |
| `sync` | `FrameSync`, the event loop, as a pure state machine |
| `adapt` | `Synced`, which turns a `FrameSyncFilter` into a `Filter` |
| `mock` | `Stamp`, a worked two-input filter |

A filter writes `on_event` and its per-input roles. Everything else — which
timestamps become events, which frame each input contributes at each one, what
happens when an input ends early — is `FrameSync`'s.

`FrameSync` is deliberately free of `FilterContext`. The loop is subtle enough
to want testing without a graph, which is what made it possible to pin all
twenty semantic cases against the reference directly.

---

## How the semantics were recovered

Nothing here is written down anywhere, so all of it was measured. The probe
overlays a solid colour whose luma **identifies which frame it is**, then reads
the output back byte by byte:

```sh
SEC="color=c=black:s=2x2:r=4:d=1,geq=lum='(N+1)*40':cb=128:cr=128,format=yuv420p"
ffmpeg -v info -fps_mode passthrough -filter_complex \
  "color=c=white:s=2x2:r=10:d=1[m];${SEC}[s];[m][s]overlay,format=gray,showinfo" -f null -
```

Three things about that command are load-bearing, and each cost a wrong reading
before it was noticed:

* **`-fps_mode passthrough`.** Without it the encoder duplicates frames to reach
  a constant rate, so the output count is the frame rate rather than the event
  count.
* **`format=yuv420p` after `geq`.** `geq` emits an alpha plane that is fully
  transparent, so the overlay composites to nothing and every frame reads white.
* **`showinfo`, not `framecrc`.** `framecrc`'s timestamps are the muxer's, and
  `overlay` sets its output time base to its *main input's* while `blend` uses
  the common one — so the muxer's rescaling hides the difference.

The sync-level machinery is visible directly in the reference's own log, which
is what turned a guess into a mechanism:

```sh
ffmpeg -v verbose -filter_complex "…[m];…[s];[m][s]overlay" -f null - 2>&1 | grep framesync
#  [framesync] Selected 1/50 time base
#  [framesync] Sync level 2
#  [framesync] Sync level 1
#  [framesync] Sync level 0
```

---

## The model

```text
step():
  1. Apply any end of stream whose last frame has been delivered:
     state -> AfterEof, sync -> 0, recompute the sync level.
  2. If no input still has the sync level, nothing can drive the clock: end.
  3. pts = the earliest lookahead among inputs at the sync level.
  4. Every input advances while its lookahead is at or before pts.
  5. Deliver, unless an input whose `before` is Stop has not started.
```

### Four ways this differs from plan 16 §3.2

**1. The sync level is dynamic.** An input that ends has its `sync` set to zero
and the level is recomputed, so the clock passes to whoever is left. That is
what makes `overlay` with a 10 fps main and a 25 fps overlay emit **twelve**
frames for one second — ten at the main's timestamps, then two more at the
overlay's remaining ones:

```text
0 0.1 0.2 0.3 0.4 0.5 0.6 0.7 0.8 0.9 0.92 0.96
```

The plan models `after = Infinity` as "hold the last frame forever", which on
its own never terminates. Holding the frame is only half of it; dropping the
sync level is the other half.

**2. Non-driving inputs advance in bulk.** Step 4 consumes every frame at or
before the event, so the plan's separate rule — "the newest frame with
`frame.pts <= pts`" — falls out rather than being implemented. With a 10 fps
main over a 4 fps secondary numbered 1–4, the secondary contributes
`1 1 1 2 2 3 3 3 4 4`, exactly as measured.

**3. End of stream takes effect one event late.** This is the subtlest rule in
the crate and the one that fixed the model. Under `repeatlast=0` a secondary's
**last** frame is delivered at exactly one event — the first at or after its own
timestamp — and is gone from the next:

```text
main 20 fps, secondary 4 fps for one second, repeatlast=0
  … 0.7=04BA 0.75=0690 0.8=09F6 …          (09F6 is "no overlay")
```

Applying end of stream as soon as it is seen loses that frame entirely.
Measured at 10, 20 and 20-fps-over-a-longer-secondary before the rule was
believed, because the first two readings admitted a simpler explanation that
the third ruled out. In the implementation it is one line: `apply_pending_eof`
runs only when `pending_pts` is `None`, i.e. between events and never in the
middle of determining one.

**4. `repeatlast=0` is *exactly* `eof_action=pass`.** Plan 16 §3.3 says
`repeatlast=0` changes only the non-driving inputs and that the two are "nearly
but not exactly" the same. Both set input 0's `after` to `Stop` and every other
input's to `Null`, and both therefore stop the whole filter when input 0 ends:

```sh
# main 0.5 s, secondary 1.0 s, both 10 fps
… overlay=repeatlast=0,showinfo    -> 5 frames
… overlay=eof_action=pass,showinfo -> 5 frames     (identical, event for event)
```

### The option truth table, measured

| Setting | Effect |
|---|---|
| `eof_action=repeat` (default) | every input keeps `after = Infinity` |
| `eof_action=endall` | every input: `after = Stop` |
| `eof_action=pass` | input 0: `after = Stop`; every other: `after = Null` |
| `repeatlast=0` | the same as `pass` |
| `shortest=1` | every input: `after = Stop`, overriding all of the above |

### Two families of per-input roles

There are two, not one, and the difference is observable:

| Family | `sync` | `before` | Filters | Events fire at |
|---|---|---|---|---|
| dual (`FsInput::dual`) | 2, then 1… | Stop, then Null… | `overlay`, `blend`, `lut2` | input 0's timestamps |
| uniform (`FsInput::uniform`) | 1 everywhere | Stop everywhere | `hstack`, `vstack`, `maskedmerge` | the **union** of every input's |

```sh
# main 10 fps, second 25 fps, 0.4 s
… [m][s]blend,showinfo  -> 0 0.1 0.2 0.3 0.32 0.36                        (6)
… [m][s]hstack,showinfo -> 0 0.04 0.08 0.1 0.12 0.16 0.2 … 0.32 0.36     (12)
```

The `before` difference is observable too: `blend` with a secondary starting at
0.5 s still emits from 0 s with nothing composited, while `hstack` emits
nothing until both inputs have started.

### `ts_sync_mode`

`default` takes the frame at or before the event; `nearest` compares it against
the lookahead and takes whichever is closer, **keeping the earlier one on an
exact tie**. Measured with an 8 fps main against a 4 fps secondary, where every
other event lands exactly halfway between two secondary frames:

| main | secondary contributes, `default` | `nearest` |
|---|---|---|
| 10 fps over 4 fps | `1 1 1 2 2 3 3 3 4 4` | `1 1 2 2 3 3 3 4 4 4` |
| 8 fps over 4 fps | `1 1 2 2 3 3 4 4` | `1 1 2 2 3 3 4 4` (ties keep the earlier) |

`nearest` costs one frame of latency, which `FrameSync::latency` reports.

### The common time base

The greatest common divisor of the inputs' time bases, capped at
`MAX_DENOMINATOR` (1,000,000), read straight out of `Selected … time base`:

| inputs | selected |
|---|---|
| 1/10, 1/25 | 1/50 |
| 1001/30000, 1/25 | 1/30000 |
| 1/1000, 1/1001 | 1/1000000 — the cap, not the exact 1/1001000 |
| 1001/30000, 1001/24000 | 1001/120000 |

---

## How to change it

* **Writing a filter.** Implement `FrameSyncFilter` and wrap it in `Synced`.
  `mock::Stamp` is the whole worked example: it copies the secondary frame's
  first byte into the main frame, which is exactly the reference probe's shape,
  so the same vectors assert against both.
* **`on_event` returns a `FrameOut`,** not `()` as plan 16 §3.4 sketches. A
  filter that pushed for itself would have to own the held-back queue when the
  output link is full — sixty-eight times over. The adapter owns it, as `Simple`
  does.
* **A filter that needs different per-input roles overrides `inputs`.** The
  default is `FsInput::dual`; `hstack` and friends want `FsInput::uniform`.
* **A filter that wants its main input's time base on the output** — `overlay`
  does; `blend` does not — sets it in `FrameSyncFilter::configure`, which runs
  *after* the adapter has installed the common one.
* **Gotcha — recording end of stream changes no link.** `sync.close()` touches
  nothing the scheduler can see, so reporting `Activity::Progressed` for it
  breaks rule F6 and parks the node against an epoch that will never move
  again. The adapter loops instead of returning; this was a real deadlock
  before it was fixed.
* **Gotcha — one frame of lookahead is inherent.** The loop cannot say that
  nothing else belongs to an event until it has seen a frame past it. End of
  stream is what releases the last one. The reference holds `frame_next` for the
  same reason.
* **Do not apply an end of stream while an event is half determined.** That is
  what `pending_pts` guards, and it is the difference between a secondary's last
  frame being composited once and never.

---

## Configuration

No environment variables and no feature flags.

| Knob | Where | Default | Effect |
|---|---|---|---|
| `FrameSyncOpts::eof_action` | per filter instance | `Repeat` | see the truth table |
| `FrameSyncOpts::shortest` | per filter instance | `false` | overrides `eof_action` |
| `FrameSyncOpts::repeatlast` | per filter instance | `true` | `false` is `eof_action=pass` |
| `FrameSyncOpts::ts_sync` | per filter instance | `Default` | `Nearest` costs one frame |
| `sync::MAX_DENOMINATOR` | compile time | 1,000,000 | the cap on the common time base |
| `adapt::MAX_PULLS_PER_STEP` | compile time | 256 | belt and braces; the loop is already bounded by link capacity |

Constants chosen here rather than taken from the reference:

| Constant | Value | Basis |
|---|---|---|
| `MAX_DENOMINATOR` | 1,000,000 | **Measured**, from the 1/1000 + 1/1001 case falling back |
| `MAX_PULLS_PER_STEP` | 256 | Ours. A correctness device: it turns a mis-written synchroniser from a spin into a diagnosable stall |
| a frame with no timestamp | previous + 1 tick | Ours. Untimed input still advances monotonically instead of collapsing onto zero |

---

## Testing

* **39 tests**: 5 unit, 33 across three integration files, 1 doctest.
* `tests/semantics.rs` — twenty cases, each carrying the reference command and
  the observed output that established it. The event loop is driven directly,
  with no graph, so a failure names the rule rather than the plumbing.
* `tests/graph.rs` — the adapter inside a real `vaco-filter-core` graph:
  backpressure with sixty-four frames through eight-deep links, an empty
  stream, a seek, and the output link's time base.
* `tests/properties.rs` — `proptest`: the loop always terminates, event
  timestamps never go backwards, `default` never looks ahead, one input is a
  passthrough, applying the options is idempotent, and the common time base
  divides its inputs.

### Fuzzing

One target (D6). The untrusted input is the **timestamps**, which come from a
demuxer and are therefore attacker-chosen: negative, out of order, repeated,
`i64::MIN`. Per plan 19 §13, the exit code and the exec count:

| Target | Exit | Execs |
|---|---|---|
| `framesync_event_loop` | 0 | `#1803792` (90 s) |

`find fuzz/artifacts/framesync_event_loop -type f` is empty.

---

## Signature gaps

Interfaces are frozen, so these are **reported, not changed**.

1. **`FilterContext` has no way to report latency or buffered depth to
   `LinkStats`.** Plan 16 §3.5 wants `latency`/`alatency` and `graphmonitor` to
   show real numbers, and wants the deadlock classifier to know that a framesync
   node holding frames while requesting an input is working rather than stuck.
   `FrameSync::latency` and `FrameSync::len` compute it; there is nowhere on the
   context to put it, so `Synced::sync()` exposes the synchroniser and a caller
   would have to downcast to reach it.
2. **`Filter::command` cannot reach the synchroniser.** `eof_action` and
   `shortest` are runtime-commandable on some upstream filters. The adapter
   would have to re-`configure` to apply a change, and `Filter::command` returns
   `Result<()>` with no way to say "I need reconfiguring".
3. **`FilterContext::input_end_pts` is the only route to the end timestamp, and
   it is per pad.** That is enough here, but a filter that wants the
   synchroniser's own end time (`concat`, `xfade`) gets it from
   `FrameSync::end_pts`, which is not reachable through `Filter`.

Nothing in `vaco-filter-core` had to change for this crate, and
`FilterContext::peek_input` — which that crate added specifically for framesync
— turned out **not** to be needed: the pure state machine takes ownership of
frames as they arrive and decides afterwards, which is simpler than peeking and
deciding whether to consume.

---

## Wanted from other crates

* **`vaco-filter-core`: demand does not propagate through an idle filter.** Not
  this crate's to fix, and it does not bite here because a framesync filter
  always has an input queue to work from, but it is the same finding
  `vaco-filter-graph` reports: `Graph::score` gives a filter no priority while
  its inputs are empty, so a request never travels back through one.
* **Nothing from `vaco-frame`.** `Frame: Clone` is an `Arc` clone per plane,
  which is what makes "hold the last frame and hand out copies" free, and it is
  the operation this crate does most.

---

## Deliberately deferred

* **A filter library.** `mock::Stamp` proves the traits; `overlay` and the other
  sixty-seven are a later wave.
* **`FrameSyncFilter::configure` overriding the output *geometry*.** The hook
  exists and runs after the time base is installed; nothing here needs it yet,
  and a hook with no implementor and no test is a guess.
* **Reporting into `LinkStats`.** See signature gap 1: the numbers exist, the
  channel does not.

---

## Dependencies

`vaco-core` (errors, `Rational`, `Timestamp`, exact rescaling),
`vaco-frame` (`Frame`, `FramePool`), `vaco-filter-core` (the `Filter` trait,
`FilterContext`, `FrameOut`, `NodeFormats`), `vaco-pixfmt` (the mock's format).
Dev: `proptest`.

No clock, no threads, nothing platform-coupled: `cargo build --target
wasm32-unknown-unknown` passes unchanged (D18).
