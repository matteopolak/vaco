# `vaco-filter-core`

Layer 5a. The filter framework: what a filter *is*, what a link carries, how
formats get agreed across a whole graph, and the cooperative scheduler that runs
the result.

This is the third crate in a family. `vaco-codec-core` established the shape —
one protocol stated once, a state machine that executes the rules rather than
merely documenting them, a validator that turns a component's misbehaviour into a
loud local test failure, and a mock component that exercises every corner.
`vaco-format-core` followed it for containers. This one follows both, and where
it departs from them the departure is called out.

---

## What it is

| Module | Contents |
|---|---|
| `link` | `Link`, the per-link frame queue, `Status`, and the end-of-stream convention |
| `context` | `FilterContext` — everything a filter can see in one step — and the nine frame-flow rules |
| `negotiate` | `Constraint`, `FormatSet`, the union-find solver, `ConverterFactory`, and the diagnostic renderer |
| `negotiate::loss` | what a conversion costs, measured against the reference |
| `sched` | `Graph`, readiness, quiescence diagnosis, buffer sources and sinks |
| `adapt` | `Simple`, `Sourced`, `Blocked` — `activate` written once per filter *shape* |
| `timeline` | the universal `enable=` expression, over `vaco-expr` |
| `mock` | five worked filters, and the proof that the traits are usable |

### The one idea worth reading first

**Filter-to-filter frame flow is N:M**, exactly as packet-to-frame is in
`vaco-codec-core`. One input can produce several outputs, several inputs can
produce none, and end of stream can produce many. That is why `Filter::activate`
is a bounded step reporting an `Activity` rather than `filter(frame) -> Frame`,
and it is the same reason the codec layer is send/receive rather than
`decode(packet) -> Frame`.

Everything else in the crate follows from wanting that to be safe to write 560
times.

---

## The negotiation model

This was the hard part and it is written down here first because it was designed
before it was implemented.

### The model

Negotiation is a **union-find over `(pad, property)` pairs**.

* A **pad** declares a `FormatSet`: per property, a `Constraint` that is `Any`,
  `OneOf(list)` in preference order, or `Exact(one)`.
* A **filter** declares *ties*: "these of my pads must resolve to the same value
  for this property". A filter that does not care what the format is, only that
  its two sides agree, is `NodeFormats::passthrough` — every pad `Any`, all pads
  tied. That is the common case and it needs no special-casing.
* A **link** adds a tie between the two pads it joins, for every property of its
  media type.

The four properties are `pix_fmt`, `sample_fmt`, `sample_rate` and
`channel_layout` — the ones `LinkFormat` carries, and therefore the ones a link
can actually be wrong about.

### The six steps

1. **Declare.** Each node contributes its pads' constraints. Built at
   *instantiation*, not registration: a filter's accepted formats routinely
   depend on its options (`format=pix_fmts=rgb24`) and on its realised pad count
   (`amix=inputs=3`), and a `&'static` descriptor can carry neither.
2. **Tie.** Node-local ties first, in node order; then links, in `LinkId` order.
3. **Intersect,** at each merge. `Constraint::intersect` keeps the *left*
   operand's preference order; membership is commutative, order is not.
4. **Repair.** The merge that first empties a class **is** the conflict. Because
   merges are ordered, which link gets named is deterministic. A conflicting link
   is repaired by splicing a converter into it; every property conflicting on one
   link is coalesced into a single converter, so a link that disagrees about
   sample format, rate and layout gets one `aresample` rather than three.
5. **Pick.** One value per class: the first candidate in the folded preference
   order, which — because pads fold in `PadRef` order — is the upstream-most
   declared preference.
6. **Configure.** In topological order. Each node's output links first inherit
   geometry and time base from its first input (so a filter that does not alter
   timing gets that for free), then take the negotiated format, then the filter
   may override in `Filter::configure`.

### Why the fold order is load-bearing

Step 5 takes "the first candidate", and step 3 keeps the left operand's order.
Together those mean the *source's* preference wins when both sides accept
several formats — which is why a graph whose two sides already agree never
converts. That matches the reference, verified directly:

```sh
ffmpeg -f lavfi -i "testsrc2=s=32x32:d=0.04" \
       -vf "hflip,format=pix_fmts=rgb24|yuv420p,showinfo" -f null -
#  -> fmt:yuv420p     (the source's native format, though rgb24 is listed first)
```

### How it terminates

Steps 2 and 3 are a single pass; step 4 is the only loop. Each round repairs at
least one `(link, property)` pair; a converter declares concrete, **untied** sets
on its two pads, so the pair it repaired cannot recur; and there are finitely
many pairs. A hard bound of `3 × links + 1` rounds sits on top as a defence, and
hitting it is `Error::Unsupported` naming a `ConverterFactory` that returned a
converter which did not fix what it claimed to. Never a hang.

`negotiation_terminates_and_is_total` in `tests/properties.rs` asserts exactly
this over arbitrary chains — plan 13 §3.2 names it as the property that matters
most for this crate.

### When no assignment exists

Three outcomes, deliberately not collapsed into one:

| Situation | Result |
|---|---|
| Two pads of the *same node* are tied and share nothing | `Error::InvalidData` — the filter's own declaration is contradictory, which is a bug in the filter, not in the user's graph |
| A link's two sides share nothing, and auto-conversion is off or the factory offers nothing | `Error::Unsupported`, with a `Conflict` rendered by `Conflict::render`; reachable through `Graph::last_conflict` |
| A class ends up entirely unconstrained | `Error::Unsupported` — we refuse to invent a format |

The last one is worth defending. Defaulting an unconstrained pixel format to
`yuv420p` is tempting and wrong: a graph with no source constraint has not said
what it carries, and picking silently is how a pipeline ends up transcoding
through 8-bit 4:2:0 because nobody said otherwise. In practice a buffer source
pins its class with `Constraint::Exact`, so this only fires on a graph that was
never going to work.

A **media type mismatch is not a negotiation failure**. It is diagnosed when the
link is made, which is also where the reference diagnoses it:

```
Media type mismatch between the 'Parsed_hflip_0' filter output pad 0 (video)
and the 'Parsed_aformat_1' filter input pad 0 (audio)
```

### The diagnostic

Every constraint records the `NodeId` that narrowed it, so the message names the
node *responsible* rather than the two link endpoints — which are frequently not
the same thing:

```
format negotiation failed for `pix_fmt` on link in:0 -> invert:0

  the link requires one common pix_fmt, but the two sides share none:

    upstream side   accepts  rgb24
      narrowed by   in
    downstream side accepts  gray
      narrowed by   invert

  auto-conversion is disabled; a converter would normally have been
  inserted here.

  fix: enable auto-conversion, or convert to `gray` before invert.
```

### Which conversion gets inserted — measured, not guessed

Core defines the mechanism; `vaco-filter-graph` supplies the policy through
`ConverterFactory` (layer 5a cannot know that a filter called `scale` exists).
But the *policy* needs a cost model, so `negotiate::loss` provides one, and its
weights were measured against ffmpeg 8.1 rather than taken from plan 16 §1.6.4 —
which is wrong in two places.

Method: chain two `format` filters so the auto-inserted `scale` must choose
between exactly two candidates, and read the choice out of `-v verbose`.

```sh
ffmpeg -v verbose -f lavfi -i "testsrc2=s=32x32:d=0.04" \
       -vf "format=pix_fmts=<src>,format=pix_fmts=<a>|<b>,null" -f null - \
  2>&1 | grep auto_scale_1
```

Probing through a filtergraph is normally the trap plan 13 §1b describes. It is
correct **here** because the filtergraph's negotiation *is* the thing under test:
the value read back is a decision the graph layer makes, not a value a parser was
handed. Soundness check: swapping the two candidates never changed the answer in
any of the eighteen pairs, so no list-order or argument-splitting artefact is
leaking in.

| Source | Candidates | Reference chose | Establishes |
|---|---|---|---|
| `yuva444p` | `yuv444p` / `ya8` | `yuv444p` | **chroma-total > alpha** |
| `rgba64le` | `rgb48le` / `rgba` | `rgba` | alpha > depth |
| `yuv444p16le` | `yuv444p` / `gray16le` | `yuv444p` | **chroma-total > depth**, at 8 bits |
| `yuv444p10le` | `yuv444p` / `gray10le` | `yuv444p` | ... and at 2 bits |
| `gbrp10le` | `gbrp` / `gray10le` | `gbrp` | ... from an RGB source too |
| `yuv444p16le` | `yuv444p` / `rgb48le` | `rgb48le` | depth > colour model, at 8 bits |
| `yuv444p10le` | `yuv444p9le` / `rgb48le` | `rgb48le` | ... and at **one** bit |
| `yuv444p10le` | `yuv444p` / `yuv420p10le` | `yuv420p10le` | depth > chroma coarsening |
| `yuv444p` | `yuv420p` / `rgb24` | `yuv420p` | colour model > chroma coarsening |
| `rgb24` | `gbrp` / `yuv444p` | `gbrp` | colour model > packing |
| `yuv420p16le` | `yuv420p10le` / `yuv420p` | `yuv420p10le` | depth loss is graded by bits |
| `yuv444p` | `yuv422p` / `yuv420p` | `yuv420p` | chroma loss is **not** graded by axis |

So the order is

> **chroma-total > alpha > depth > colour model > chroma coarsening > packing**

Thirty-five ordered pairs are pinned as a table in `loss.rs`'s tests, every one
of them order-independent (swapping the two candidates never changed the
reference's answer).

#### "Going grey" is its own tier, not a colour-model change with extra

This is the part that is easy to get wrong, and this module got it wrong once.

A **YUV↔RGB** change provably sits *below* depth: the reference gives up a whole
colour model rather than one bit of precision, measured at 1, 2, 4 and 8 bits. A
**greyscale destination** sits *above* depth, above chroma coarsening, and above
alpha. They are separate components, and no single "colour model" tier can fit
both — any weighting with `COLOUR_MODEL > DEPTH_PER_BIT` contradicts all four
`-> rgb48le` rows above. `colour_model_above_depth_would_contradict_the_measurements`
is that statement as an executable test.

A related trap: `ya8` is greyscale-with-alpha and has *two* components, so a
naive "one component means grey" test scores it as a plain colour format and gets
`yuva444p -> {yuv444p, ya8}` backwards. Alpha is not a colour component.

#### What plan 16 §1.6.4 should now read

The plan's table is wrong in three ways, not two:

1. It puts chroma resolution **above** colour-model change. Measured, it is below.
2. It grades chroma loss **per halved axis**. Measured, it is a flag — `yuv444p`
   to `yuv422p` and to `yuv420p` score the same and the reference picks between
   them on its enum order.
3. It has **no notion of losing chroma entirely**, which is the single heaviest
   component in the whole model and the only one that outranks alpha.

The corrected tier list, in the plan's own format:

| Component | Condition |
|---|---|
| chroma-total | destination is greyscale and the source is not |
| alpha | source has alpha, destination does not |
| depth (per bit) | `to.depth < from.depth` |
| colour model | YUV↔RGB with chroma preserved |
| chroma coarsening | subsampling coarser than the source; a flag, not a count |
| packing | planar↔packed or endianness; costs a pass, loses nothing |

The absolute weights are ours and only their order is measured; `loss.rs`
carries values that keep every one of the thirty-five pairs strict. The tier
order itself is a set of `const _: () = assert!(..)` statements at module scope,
so a reweighting that breaks it fails to **compile** rather than failing a test —
the technique `vaco-pool` uses to keep `BITSTREAM_PADDING` locked to
`Padded::PAD`.

#### A note on corpus coverage — the mistake worth not repeating

The first version of this table had seventeen rows, was order-independent
throughout, and still had the tiers wrong. Every row offered either a *colour*
destination on both sides or grey as the *source*; none offered grey as a
*candidate* against a colour format, which is precisely the pair that
discriminates the two orderings. The method was sound and the corpus had a hole.

The general lesson, worth applying to the next table anyone measures this way:
**a pairwise-comparison corpus is only as good as its coverage of the
discriminating pairs.** Enumerate the components first, then make sure every
*pair of components* appears with the others held equal — rather than collecting
pairs that look interesting and inferring an order from whatever turns up.

#### Known divergence: the equal-loss tiebreak

When two candidates lose the same thing, the reference falls back on its own
`AVPixelFormat` enum ordering — an implementation artefact D1 says we do not
mirror. Three measured pairs depend on it and we get two wrong:

| Source | Candidates | Reference | Us |
|---|---|---|---|
| `gray` | `gray10le` / `rgb24` | `rgb24` | `gray10le` |
| `gray` | `rgb24` / `gbrp` | `rgb24` | `gbrp` |
| `gray` | `gray10le` / `yuv444p` | `gray10le` | `gray10le` ✓ |

Our tiebreak is `PixFmt`'s own discriminant: deterministic, which is what D6
actually requires, and wrong only for a greyscale source. Closing it needs a
`reference_rank(PixFmt) -> u16` column in `vaco-pixfmt`'s generated table — the
reference's own ordering, recorded as the interface fact it is. That is another
crate's file, so it is **reported, not written** (see *Wanted from other crates*).
`the_grey_source_tiebreak_still_diverges` asserts the divergence still exists, so
closing it fails a test rather than passing silently — the pattern D17.1 rule 3
established.

---

## The frame-flow contract

Nine rules. The scheduler checks six of them and reports a `Violation`; the rest
are structural.

| Rule | Statement |
|---|---|
| **F1** | `take_input` hands over queued frames in order, and `None` when none is queued. It never skips a frame to report end of stream. |
| **F2** | `input_at_eof` is **sticky** and **ordered behind the queue**: false while frames remain, true once the producer closed *and* the queue drained, and true forever after. |
| **F3** | Pushing to a closed output pad is a defect. The frame is refused, not dropped. → `PushAfterClose` |
| **F4** | `close_output` is idempotent. |
| **F5** | `Activity::Eof` may be returned only when every output pad is closed, and the filter is not run again afterwards. → `EofWithOpenOutput`, `ActivateAfterEof` |
| **F6** | `Activity::Progressed` requires that something observable changed. → `ProgressWithoutChange` |
| **F7** | `Activity::NeedInput` requires that at least one input is not yet at end of stream. → `NeedInputAtEof` |
| **F8** | `Activity::Blocked` means an output is full or unwanted. The filter keeps whatever it was holding. |
| **F9** | A pushed frame's timestamps are read in **the output link's** time base, and the framework rescales exactly. Its format must match what negotiation agreed. → `FrameFormatMismatch` |

**F2 is the one that costs most when it is missing.** `vaco-format-core` found
the same rule the hard way on the demuxer side — a demuxer that did not latch end
of stream reported its own trailer as corruption — and its docs asked for it to
be stated next time. This is that. `end_of_stream_is_sticky_and_ordered` in
`tests/properties.rs` asserts it over arbitrary call sequences rather than the
handful a named test would think of.

**F6 deserves a note too.** A filter that claims progress while doing nothing
does not hang — it *spins*, which is worse, because it looks like work. The
scheduler snapshots the link-epoch sum around every `activate` call, so this is
caught in one step and reported rather than burning a core.

### Threading, and what D18 changes

`Filter: Send`, so a `Graph` can be **moved** between threads. It is not `Sync`
and there is no way to drive one graph from two threads at once — the whole
design is a single driver making one bounded call at a time, which is what makes
the schedule deterministic and therefore what makes `framecrc`-style differential
testing meaningful for filtergraphs at all (D6).

Parallelism was always meant to come from elsewhere: pipeline parallelism in
`vaco-sched` (one component per task, bounded channels), and data parallelism
*within* one `activate` call via the `SliceFilter` adapter. Neither is in this
crate today, and **that turns out to suit D18**: `wasm32-unknown-unknown` has no
threads, so a framework whose correctness does not depend on any is portable by
construction. When `SliceFilter` lands it should be an adapter that degrades to a
sequential fan-out when no pool is available, not a load-bearing assumption in
the trait layer. `cargo xtask wasm-check` passes; this crate touches no clock and
spawns nothing.

---

## The scheduler

**Readiness is computed, not asserted.** The reference requires a filter to
declare "I still have work"; forgetting is a hang, and it is a recurring bug
class there. Here the score comes from observable link state, so a filter that
forgets still runs again if any of its links changed. `Activity::Progressed` is a
hint, never the sole mechanism.

The mechanism is a **park epoch**: when a node reports `NeedInput` or `Blocked`,
the scheduler records the sum of its links' epochs. While that sum is unchanged,
nothing the node is waiting on has moved, so it is skipped. Any push, pop, close
or request bumps an epoch and wakes it. Forgetting to "set ready" is therefore
not expressible.

**Quiescence is diagnosed, not tolerated.** `Graph::run` returns:

| Condition | Result |
|---|---|
| every sink drained and closed | `GraphStatus::Eof` |
| a sink is holding frames | `GraphStatus::HasOutput` — drain it |
| a source is open and unfed | `GraphStatus::NeedInput` — feed it |
| none of the above | `GraphStatus::Deadlock(Vec<Stall>)` — a bug, with the node, the link, its depth and whether it is closed |
| the step budget ran out | `GraphStatus::BudgetExhausted` — also a bug, also reported |

`pick` re-scores every node on every step rather than maintaining a heap
incrementally. That is O(nodes × pads) per step, it costs about 0.3 µs per node
per frame (below), and it removes the entire class of bug where something forgets
to mark a node ready. The incremental version would produce the same schedule; it
is an optimisation, not a design, and it should not be attempted without a
profile that wants it.

### Buffer sources and sinks are the scheduler's, not a filter's

`Graph::add_source` / `add_sink` create nodes the scheduler drives itself.
`send`, `recv`, `close_source`, `source_wants` and `sink_format` reach the link
queue directly. They live here because they need privileged access to link
internals and because they are the API boundary every consumer of the subsystem
uses. Making them ordinary filters would have required downcasting through
`Box<dyn Filter>`, which is exactly the sort of thing this design exists to avoid.

---

## The adapters

Almost no filter should implement `Filter` directly. With ~560 filters coming, an
awkward API is paid for 560 times.

| Adapter | Shape | Handles for you |
|---|---|---|
| `Simple<F: FrameFilter>` | 1-in 1-out, one frame in → zero or more out | demand checking, status ordering, the flush loop at end of stream, timeline gating, holding back frames a full link refused |
| `Sourced<F: SourceFilter>` | 0-in 1-out | producing only on demand, the end-of-stream timestamp |
| `Blocked<F: AudioFilter>` | a `FrameFilter` that sees exactly `frame_size` samples | the FIFO, and a correctly short final block |
| `Paired<F: PairedFilter>` | N-in 1-out (N fixed at construction, two by default), strict lockstep | pulling one frame per input before calling the filter, and ending the whole filter the instant any one input runs dry |
| `Fanout<F: FanoutFilter>` | 1-in N-out (N fixed at construction) | waiting for room on **every** output pad before consuming, and the flush loop |
| `Dual<F: DualFilter>` | fixed 2-in 2-out, lockstep inputs | `Paired`'s lockstep-input rule and `Fanout`'s all-outputs-have-room rule combined, plus a pending queue per output pad (the one piece neither existing adapter needed, since neither has more than one of both) |

### `Paired`/`Fanout`, and why they are not `Synced`

Three filter agents independently reported `framepack`, `mergeplanes`,
`alphamerge` and `extractplanes` as blocked, on the theory that a multi-input or
multi-output filter needed a capability `Filter::activate` did not have. It
already had it — `vaco-filter-video-composite`'s `overlay` is two inputs, two
independent timelines, driven by `vaco-filter-framesync`'s `Synced` — so what was
actually missing was the *convenience*: `Simple`-shaped adapters for the other
two multi-pad cases, so every such filter does not re-derive the same forty
lines. `Paired` and `Fanout` are that.

`Paired<F>` is **not** a framesync-free reimplementation of `Synced`, and the
difference is measured rather than a layering excuse (this crate cannot depend
on `vaco-filter-framesync` regardless — `layer-check` would refuse it, since
framesync depends on core, not the reverse). `ffmpeg -h filter=framepack` and
`=mergeplanes` carry no `eof_action`/`shortest`/`repeatlast`/`ts_sync_mode`
section at all, unlike `alphamerge`'s, which has one verbatim. `framepack`
**refuses** two inputs whose time bases differ (`Left and right time bases
differ (1/10 vs 1/5)`) rather than reconciling them, and feeding it a 10-frame
and a 5-frame input at the same rate produces exactly 5 outputs — not 10 with
the shorter input's last frame repeated, which is what `eof_action=repeat` (the
framesync default) would do. `mergeplanes` measures identically. So "paired"
really is a different, simpler shape: every input contributes one frame per
call or the whole filter ends, with **no** per-input timeline in between — not
"framesync with the options hardcoded to `endall`".

A filter that *does* need `eof_action`/`shortest`/`repeatlast`/`ts_sync_mode` —
`alphamerge`, `maskedmerge`, and the rest of the 68 — wants
`vaco-filter-framesync`'s `Synced`, unchanged, exactly as `overlay` already
uses it. `Paired` was not made to fit that case; porting `overlay` onto it was
tried and does not fit; see *The worked examples are the proof*, below, for
what that attempt found.

`Paired<F>` generalises to N inputs rather than being hardcoded to two, because
`mergeplanes` needs up to four (fixed at construction from its own `mapN`s
options) and the alternative — a second, nearly identical adapter for "N-in,
lockstep" — is exactly the kind of duplicate-by-omission D19 exists to prevent.
`PairedFilter::input_count` defaults to two, the common case (`framepack`); a
filter with a construction-time count overrides it.

`Fanout<F>` generalises `vaco-filter-plumbing`'s `split`/`asplit` from "push N
*clones* of the one input frame" to "push N *different* frames the filter
derives from it", which is `extractplanes`' shape (one pad per requested
plane). It keeps `split`'s exact backpressure discipline: check every output
pad has room *before* reading the input, so the N derived frames can always be
pushed immediately afterwards and no per-pad pending queue is needed.

### `Dual`, and the cycle it does not solve

`Simple`/`Blocked` (1-in-1-out), `Sourced` (0-in-1-out), `Fanout`
(1-in-N-out) and `Paired` (N-in-1-out) cover every shape *except*
N-in-M-out with more than one output on the "many inputs" side —
`vaco-filter-overlay`'s `feedback` (`VV->VV`) needed exactly that and
none of the four fit, found and filed as `planning/INTERFACE-GAPS.md`
gap 24. `Dual<F: DualFilter>` closes the shape question: it is `Paired`
and `Fanout` combined (lockstep input consumption, all-outputs-have-room
backpressure) plus one genuinely new piece, a pending queue *per output
pad* — neither `Paired` (one output) nor `Fanout` (one input) needed
more than one of both, so neither had to solve per-pad output
backpressure and per-input lockstep at once. Like `Paired`,
`DualFilter::input_count`/`output_count` default to two (`feedback`'s own
arity) but may be overridden at construction, generalising to N-in/M-out
the same low-cost way `Paired` generalises past two inputs — not a
hardcoded-two adapter, just one with a single, two-and-two consumer so
far. D19 argued against a *new, more general* trait design with no second
consumer; it did not argue for hardcoding the one this crate already had
a working pattern for.

`Dual` is **necessary but not sufficient** for `feedback`. `feedback`'s
own reference usage (`[0][fb]feedback[out][fb]`) loops one output back
as the filter's own next input — a genuine cycle in the filtergraph, not
just an unusual arity — and `Graph::configure()` requires
`Graph::topological_order()`, which hard-rejects any cycle before a
`Dual`-shaped node's pads would ever be negotiated. That is a scheduler
and negotiation limitation, not an adapter-shape one, and is a separate,
open gap (`planning/INTERFACE-GAPS.md` gap 25) — `Dual` exists and is
tested (`crates/filter/vaco-filter-core/tests/graph.rs`'s
`dual_*` tests), but `feedback` itself is still not implementable.

`Simple`'s step order is fixed and matters: **drain what was held back → check
demand → take one input or observe end of stream → evaluate the timeline → call
the filter → queue → push what fits.** Frames the link refuses stay in the
adapter's queue, so a filter is never asked to hold a frame it already produced.

Writability needs no flag. `filter_frame` takes the `Frame` **by value**, and
`frame.plane_mut(0)` is `Arc::make_mut`: it writes through when this filter holds
the only reference and copies exactly once when it does not. The reference needs
a `NEEDS_WRITABLE` pad flag because C cannot express ownership; here there is
nothing to get wrong, and
`a_filter_that_only_reads_shares_the_planes_it_was_given` checks that a
read-only filter really does pass the same buffer through.

---

## The worked examples are the proof

`vaco-format-core` proved its traits with a worked container and it caught real
design errors. The same approach here, with five filters chosen so that between
them they exercise what a 1:1 pixel filter would leave untested:

| Filter | Shape | Proves |
|---|---|---|
| `Counter` | source | on-demand production, the end-of-stream timestamp |
| `Invert` | 1-in 1-out video | copy-on-write, negotiation to an exact format |
| `Gain` | 1-in 1-out audio | the audio path and a fixed input block size |
| `Fps` | 1-in N-out video | **N:M frame flow** and an output time base that differs from the input's |
| `Drop` | 1-in 0-or-1-out | consuming without producing |

### What they caught

Four design errors, none of which a unit test on the types alone would have
found:

1. **A source cannot inherit its output link's geometry**, because it has no
   input to inherit from. Configuration failed with "a link was left without a
   usable format". `SourceFilter::configure` exists because of this, and its doc
   comment says a source must implement it.
2. **`FrameFilter` had no `configure` hook,** so `Fps` had no way to set the
   output time base it exists to change. The first attempt worked around it with
   a private trait and a bespoke `impl Filter for Simple<Fps>`, which does not
   even compile — it collides with the blanket impl. The hook is the right answer
   and it is now on the trait.
3. **The scheduler re-ran parked nodes forever.** A filter returning `NeedInput`
   against a closed input was rescheduled immediately, so a graph that should
   have reported a deadlock burned its whole step budget instead. The park epoch
   is the fix.
4. **Rate conversion dropped the tail of every stream.** `Fps` knew where each
   input *started* but not how far it reached, so the last input covered one
   output slot regardless of the ratio and a 25→50 conversion produced 19 frames
   from 10 inputs instead of 20. Deriving the reach from the frame's own
   `duration` fixed it. The slot mapping also had to be floor rather than
   round-to-nearest, or one second of 25 fps yields eleven frames at 10 fps.

### `overlay` was not ported onto `Paired`, and here is why

When `Paired` landed, the natural next question was whether
`vaco-filter-video-composite`'s `overlay` — the existing multi-input witness —
could be rewritten on top of it instead of `vaco-filter-framesync`'s `Synced`,
which would have been a nice demonstration that the new adapter subsumes the
old pattern. It does not, and not for a narrow implementation reason:

* **`Paired` cannot express `overlay`'s default behaviour at all.**
  `overlay`'s default `eof_action` is `repeat`: once the secondary input ends,
  its last frame is held and composited onto every subsequent main frame,
  and the *main* input keeps driving output past the secondary's end of
  stream. `Paired`'s termination rule is the opposite by design — the first
  input to run dry ends the whole filter, unconditionally, no repeat. Every
  one of `overlay`'s existing tests exercises the default options, so a
  `Paired`-backed `overlay` would not merely differ in some untested corner;
  it would end the stream early on the two-input test graphs
  `vaco-filter-video-composite`'s own suite already uses.
* **`Paired` has no timestamp-based event selection.** `overlay` samples its
  secondary input at "the most recent frame at or before the main's current
  timestamp" (`ts_sync_mode=default`) or the nearest one
  (`ts_sync_mode=nearest`), which only means something because the two
  inputs can run at different frame rates. `Paired` just pulls the next
  available frame from each input on every call — correct for `framepack`
  and `mergeplanes`, which refuse or do not need mismatched rates, and wrong
  for `overlay`, which is *routinely* used with a lower-rate overlay.

Both were confirmed rather than assumed: `cargo test -p
vaco-filter-video-composite` was run before touching anything (43 passed,
recorded above) and the crate was **not** edited — `overlay` still wraps
`Synced`, unchanged, and the same 43 tests still pass. This is the "if it does
not fit, leave it alone" outcome plan 16 flags as an acceptable answer, not a
shortfall: a witness that stops passing its own tests to prove a point is
worse than a slightly redundant adapter.

---

## How to change it

* **Writing a filter.** Implement `FrameFilter` (or `SourceFilter`, or
  `AudioFilter`) and wrap it in the adapter. Read `mock.rs` first; `Invert` is
  twenty lines and does everything the common case needs.
* **A filter that changes geometry or timing must implement `configure`** and
  call `ctx.set_output_link`. Nothing else in the graph can know.
* **A source must implement `configure`.** See error 1 above.
* **`Eof` must be sticky, and so must anything derived from it.** If you add a
  method that reports end of stream, latch it. This is F2 and it has now cost
  two crates in this project a bug each.
* **Do not add a method that returns `&LinkFormat` without saying which
  direction.** `FilterContext::link` is frozen that way and it is the one
  genuinely confusing thing in the API; prefer `input_link` / `output_link`.
* **Adding a negotiated property** means a variant in `Property`, a field in
  `FormatSet` (frozen — so this needs the orchestrator), an arm in
  `merge_property`, `pick` and `install`, and a row in `Property::for_media`. The
  tests that walk `Property::ALL` will tell you what you missed.
* **Gotcha — the fold order is load-bearing.** `Constraint::intersect` keeps the
  *left* operand's preference order, and pads fold in `PadRef` order. Changing
  either changes which format a graph picks, which is a conformance change and
  not an internal one.
* **Gotcha — a converter must not tie its own pads.** `NodeFormats::converter`
  leaves `ties` empty, and that is what makes the termination argument work. A
  converter with tied pads cannot converge and will hit the round bound.
* **Do not add a `HashMap`.** Iteration order is output order (DD2). Everything
  here is a `Vec` indexed by a dense id for exactly that reason.

---

## Configuration

No environment variables and no feature flags. The knobs are constants and
parameters:

| Knob | Where | Default | Effect |
|---|---|---|---|
| `link::DEFAULT_QUEUE_DEPTH` | per link | 8 | Frames a link buffers before refusing a push |
| `Link::with_capacity` | per link | — | Overrides the above; clamped to at least 1 |
| `sched::DEFAULT_STEP_BUDGET` | per graph | 2^20 | Steps before `run` gives up and reports `BudgetExhausted` |
| `Graph::with_step_budget` | per graph | — | Overrides it; fuzz targets use 4096 |
| `Graph::with_pool` | per graph | a fresh `FramePool` | Share one pool across a pipeline so frames recycle through one free list |
| `AutoConvert` | per configure | `All` | `None` is `-noauto_conversion_filters` |
| `timeline::TIMELINE_VARS` | compile time | `t n w h pos` | What an `enable=` expression may name |

Constants chosen here rather than taken from the reference, each recorded as a
choice rather than presented as reproduction:

| Constant | Value | Basis |
|---|---|---|
| `DEFAULT_QUEUE_DEPTH` | 8 | Plan 16 §1.5's `max_frames`. Not observable through any output field |
| `DEFAULT_STEP_BUDGET` | 2^20 | Ours. It is a correctness device, not a tuning knob: it turns a mis-written filter from a hang into a diagnosis |
| `loss::*` weights | see above | **Measured.** The ordering is the reference's; the absolute values are ours, and only their order matters |

### A trap in `enable=`, with the measurement

Truthiness in the expression language is `x != 0`, so `NAN` is **true**. But the
*comparison* functions return `0` for `NAN` rather than propagating it. Those two
facts point opposite ways and both matter:

| `enable=` | `t` is `NAN` | Filter is |
|---|---|---|
| `between(t,10,20)` | comparison yields `0` | **off** |
| `t` | the value *is* `NAN`, and `NAN != 0` | **on** |

Measured rather than assumed, against the pinned reference and against
`vaco-expr` directly, which agree:

```sh
ffmpeg -f lavfi -i "aevalsrc=exprs='between(nan,10,20)':s=1:n=1:d=1" -f f64le -
#  -> 0000000000000000
```

The practical consequence: a time-gated filter is **disabled** on frames with no
timestamp, which is usually what you want and is emphatically not what "NAN is
truthy" would lead you to guess.

---

## Performance

`cargo bench -p vaco-filter-core`, divan, release profile, Apple silicon,
median of 100. Framework overhead only — the filters in the passthrough rows
touch no pixels.

| Bench | Median | Note |
|---|---|---|
| `passthrough_frame/1` | 599 ns | one node: send, run, recv, including frame allocation |
| `passthrough_frame/8` | 2.58 µs | ≈ **0.3 µs per node per frame** marginal |
| `inverting_frame/1` | 625 ns | the same, with 4 KiB actually rewritten |
| `rate_doubling_frame` | 760 ns | one input, two outputs |
| `intersect_two_lists` | 14.7 ns | the solver's inner loop |
| `negotiate_chain/64` | 31.0 µs | 64 nodes, five formats each — once per graph |
| `configure_graph/32` | 22.9 µs | negotiate plus configure, end to end |
| `close_and_rescale` | 12.3 ns | flush plus a rescaled end-of-stream timestamp |

The number to keep an eye on is the marginal 0.3 µs per node per frame, which is
the O(nodes) re-scoring. At 60 fps through a ten-node graph that is 180 µs per
second of video — 0.018% of one core — so the simplicity is bought cheaply. The
`passthrough_frame` rows also include a `FramePool::default()` per frame, so they
are an upper bound rather than a floor.

Negotiation is reported separately on purpose: it happens once per graph, not
once per frame, and putting the two in one table invites the wrong conclusion.

---

## Testing

* **99 tests**: 59 unit, 28 named integration cases, 11 property tests, 1
  doctest — plus five compile-time assertions pinning the loss tier order.
* **`tests/graph.rs` is the proof.** Real graphs, driven to completion, one rule
  pinned each: frame conservation at every stream length, N:M in both directions,
  exact rescaling, backpressure engaging, zero-copy holding, the negotiation
  diagnostic, auto-conversion end to end, and every `Violation` provoked
  deliberately.
* **`tests/properties.rs` is `proptest`.** Intersection is commutative in
  membership, idempotent, and exactly the shared members; negotiation terminates
  and is total; end of stream is sticky under arbitrary call sequences; a 1:1
  filter conserves frames; rate conversion is monotonic.
* **Two fuzz targets** (D6): `filter_timeline_expr` (the crate's only untrusted
  *parser* input — a filter argument string) and `filter_graph_schedule` (an
  arbitrary graph shape and call sequence, the same shape as
  `codec_send_receive` and for the same reason).

### What the generated tests found

**`filter_graph_schedule`, first run, exec 26.** The shrunk sequence was `send,
send, close, run, flush`. After a flush the link's sticky end of stream is
cleared *and* the output pad is re-opened, but the adapter's own "I have
finished" flag is not — because **`Filter` has no `flush` hook a seek could
reach**. The adapter recovered by watching for the input to stop being at EOF,
and closing the source again before the filter next ran meant that never
happened: it returned `Activity::Eof` over an output pad the flush had re-opened,
and downstream would have waited forever.

**This was a finding about the trait, not only about the adapter**, and it is
what `Filter::flush` was added for — see *One approved change to a frozen
interface* above. The adapter also closes its outputs idempotently whenever it
reports `Eof`, as belt-and-braces against a driver that re-opens a link without
calling `flush`. `a_seek_immediately_followed_by_end_of_stream_still_closes_the_outputs`,
`a_seek_that_brings_new_data_restarts_the_filter` and
`a_seek_reaches_the_filter_and_drops_what_it_was_holding` pin all three halves.

Fuzz results, per plan 19 §13 — exit code and exec count, not a verdict:

| Target | Exit | Execs |
|---|---|---|
| `filter_timeline_expr` | 0 | `#2635437` (45 s) |
| `filter_graph_schedule` | 0 | `#575467` (90 s), after `Filter::flush` landed |

`find fuzz/artifacts -type f` is empty.

---

## One approved change to a frozen interface

`Filter::flush(&mut self)`, defaulted to a no-op, was **added after the freeze
with the orchestrator's approval** — the same precedent as
`Muxer::stream_time_base` in `vaco-format-core`, and for the same reason: a
defaulted method breaks no implementation, and without it the interface cannot
express something it has to.

`Graph::flush` clears every link's queue and its sticky end of stream, which is
what a seek does to the *framework*. It could not reach the *filter*. A delay
line, a reorder buffer, an FFT window or a rate converter's held input survived
the seek and was spliced onto the new position; and an adapter that had already
reported `Activity::Eof` could be left holding an output pad the flush had
re-opened, which downstream would have waited on forever. `filter_graph_schedule`
found the second of those at exec 26.

It mirrors `Decoder::flush`: infallible, total, and with a post-state
indistinguishable from a freshly configured filter. Configuration survives —
a compiled `enable=` expression and its cached geometry are not state.
`FrameFilter`, `SourceFilter` and `AudioFilter` each gained a matching
`flush_state`, and `a_seek_reaches_the_filter_and_drops_what_it_was_holding`
pins it against `Fps`, which is the worked example that actually holds a frame.

A source is deliberately **not** rewound: nothing in this interface carries a
seek target, so an exhausted source stays exhausted and re-closes its output.

## Graph introspection: narrow by design

`ffmpeg -h filter=graphmonitor`/`agraphmonitor` (`vaco-filter-scope`, issue
#480) draw the *whole graph's* live state — every link's queue depth, EOF
status, format — and building that crate found this was not just
unimplemented but **not expressible**: `FilterContext` exposed only the
current node's own pads, keyed through `self.node: &NodeLinks`, which
holds only this node's own `LinkId`s. Filed as
`planning/INTERFACE-GAPS.md` gap 22.

Closing it added two read-only accessors: `FilterContext::graph_nodes(&self)
-> &[NodeInfo]` (each node's id, scheduler label, `&'static str` filter
name) and `FilterContext::graph_links(&self) -> Vec<LinkView>` (each
link's id, `PadRef` endpoints, media type, queue depth, capacity, EOF
flag, and its existing `LinkStats`). Most of the underlying data already
existed — `LinkStats`'s own doc comment already named `graphmonitor` as
an intended consumer, and `links: &mut LinkArena` on `FilterContext` was
already a reference to the *entire* arena, not just the current node's —
so the actual gap was which methods exposed it, not missing data. Node
labels were the one new field: `Graph` gained `node_labels:
Vec<NodeInfo>`, built incrementally in `push_node` rather than collected
from `self.nodes` on every call, to keep the scheduler's hot path
allocation-free.

**The design question was what a filter is allowed to see, not what data
exists.** A filter that can reach arbitrary graph state — push to another
node's link, close another node's pad, or reach another node's `Filter`
implementation — is a filter that can be written to depend on scheduling
order, which plan 16 §1.1's own boundary ("a filter can never reach
another filter's private state, only link state") exists to prevent, and
is a materially worse property than the missing introspection capability
ever was. `NodeInfo`/`LinkView` are deliberately **read-only snapshots**
taken at call time, and deliberately exclude scheduler-internal state
(`parked_at`, `self_driven`, `last_run`) that `graphmonitor`'s `mode`
flags do not need. A general graph accessor was considered and rejected
in favour of the narrowest surface that serves the two named consumers.

Verified end-to-end: `tests/graph.rs`'s
`a_filter_can_read_every_nodes_label_and_every_links_state` builds a real
3-node graph and confirms a filter can see the *other* nodes' labels and
an upstream link's state, then (as a deliberate check that the test has
teeth) `graph_nodes`/`graph_links` were temporarily stubbed to return
empty and the test failed with a clear diagnostic before the real
implementation was restored.

**Not done here**: `graphmonitor`/`agraphmonitor` are still not
implemented as filters in `vaco-filter-scope` — this closes the framework
capability those filters need, not the filters themselves.

## Signature gaps

Interfaces are frozen (plan 19 §6), so these are **reported, not changed**. In
descending order of what they cost.

1. **`FilterContext::link(pad)` does not say which direction.** Every sibling
   method names one (`take_input`, `push_output`, `input_at_eof`); this one takes
   a bare index over two pad lists. The only total reading is a concatenated
   space — inputs then outputs — which is what is implemented and documented, and
   it is easy to get wrong by one. It should be two methods;
   `input_link`/`output_link` were added alongside and are what filters should
   use.
2. **`FilterContext` has no way to *set* an output link format**, so a filter
   that changes geometry or timing could not express it. `set_output_link` was
   added; without it `scale`, `crop`, `fps`, `setpts` and every rate filter are
   unimplementable.
3. **`FormatSet` has no colour properties.** The plan negotiates colour space,
   range, primaries, transfer and alpha mode alongside pixel format. The frozen
   set has four properties and none of them is colour. Colour is *carried* on the
   link (`LinkFormat::Video::color`) and inherited from upstream, which works,
   but a filter cannot declare "I only accept full-range input" and have the
   graph insert a converter for it. Adding them is a `FormatSet` change and
   therefore the orchestrator's.
4. **`FormatSet::intersect` returns `Option<Self>`,** so it cannot say *which*
   property failed — and the diagnostic's whole value is that detail.
   `intersect_detailed` was added beside it, returning `Err(Property)`, and the
   frozen method delegates.
5. **`close_output(pad)` carries no timestamp.** `tpad`, `xfade` and `concat`
   need the time the stream ended at. `close_output_at(pad, pts)` was added; the
   frozen method is that with `Timestamp::NONE`.
6. **`Status::Failed` carries no error,** because `vaco_core::Error` is not
   `Clone` — it holds a `std::io::Error` — and a terminal status may be read more
   than once. Only the *fact* propagates down the link; the error value goes back
   to whoever called `Graph::run_once`. Stringifying it would lose the variant a
   caller matches on, so it is left as a gap rather than papered over. See
   *Wanted from other crates*.
7. **`FilterDesc` cannot carry a filter's accepted formats,** because it is
   `Copy + 'static` and a filter's formats depend on its options. `NodeFormats`
   is built at instantiation and handed to `Graph::add` separately. This is a
   departure from plan 16 §1.1, which puts `formats` on the descriptor; that
   cannot work for `format=pix_fmts=rgb24` or `amix=inputs=3`.
8. **`FilterDesc` has no `PadSpec::Dynamic`.** Pad counts are fixed by the
   `&'static [Pad]` slices, so `amix=inputs=N` and `split=N` cannot realise their
   pads. `FilterFlags::DYNAMIC_INPUTS` exists and nothing can act on it.
9. **`Filter::command` takes `(&str, &str)` and returns `Result<()>`.** The plan
    wants flags (`ONE`, `FAST`) and a `CommandReply::Text` for commands that
    answer. `ebur128`'s metadata query has nowhere to go.

## Wanted from other crates

* **`vaco-pixfmt`: a `reference_rank(PixFmt) -> u16` column.** The reference's
  own pixel-format ordering, which is its equal-loss tiebreak. Recording it is
  recording an interface fact, the same category as a format name; it would close
  the last known divergence in `negotiate::loss` and it is one generated column.
* **`vaco-core`: `Error` is not `Clone`.** That is why a filter failure cannot be
  fanned out down a link. Either `Clone` (which means boxing or `Arc`-ing the
  `io::Error`), or a small `Copy` failure-kind sibling that a status can carry.
  The second is probably right and is cheap.
* **`vaco-frame`: nothing.** `FramePool::acquire_video`/`acquire_audio` take no
  `Budget`, which is exactly what a filter needs — pooled allocation with no
  plumbing. It was the right shape already.

## Deliberately deferred

* **`SliceFilter` and slice threading.** It needs a thread pool; `rayon` is
  listed as a dependency for this crate in plan 16 §4.1 but is not in the
  crate's manifest, and adding one is a decision, not an edit. The design should
  be an adapter that fans out to a pool when one exists and runs sequentially
  when it does not — see *Threading* above for why D18 makes that the right shape
  anyway.
* **`Synced` / framesync.** It lives in `vaco-filter-framesync` by plan 16 §4.1,
  and it is that crate's to write against `FilterContext::peek_input`, which
  exists for it. `Paired`/`Fanout` (this crate) are a different, simpler
  shape for the multi-pad filters that do not need a per-input timeline — see
  *`Paired`/`Fanout`, and why they are not `Synced`*, above — not a
  replacement for it.
* **The graph DSL, escaping, and auto-conversion policy.** `vaco-filter-graph`'s.
  Core supplies `ConverterFactory`, `ConverterSpec`, `NegotiationPlan::splice`
  and `Graph::configure_converting`; the policy — that `scale` fixes pixel
  formats and `aresample` fixes audio — is deliberately not here, because layer
  5a must not know those filters exist.
* **`MediaOpener`** (plan 16 §4.4), which `vaco-filter-movie` needs. Nothing in
  this crate exercises it, and a trait with no implementor and no test is a
  guess.
* **Queued and targeted commands.** `Filter::command` is implemented and the
  `enable` command works through it; `Graph::send_command`, timed delivery and
  target matching belong with the graph, which knows instance tags.

---

## Dependencies

`vaco-core` (errors, `Rational`, `Timestamp`, exact rescaling, `MediaType`),
`vaco-frame` (`Frame`, `FramePool`, plane views), `vaco-pixfmt`, `vaco-sampfmt`,
`vaco-chlayout`, `vaco-color` (the format vocabulary negotiation ranges over),
`vaco-expr` (the `enable=` expression), `bitflags`, `smallvec`.
Dev: `proptest`, `divan`.

**No `vaco-opts`.** The frozen manifest declared it, on the plan's assumption
that `FilterDesc` would carry `options: &'static OptionSchema`. It cannot —
`FilterDesc` is `Copy + 'static` and a filter's options are per-instance — so the
edge was never used and has been removed. An unused edge misrepresents the
layering and makes the crate hostage to a dependency it does not need. A filter
crate that wants an option schema depends on `vaco-opts` itself.

No external media crate. No filter crate — that is the `ConverterFactory` seam's
whole purpose.
