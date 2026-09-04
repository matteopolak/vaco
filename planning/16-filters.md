# 16 — The Filter Subsystem

Plan for `vaco-filter-*`: the libavfilter-equivalent layer. Conforms to `planning/00-decisions.md`
(D1 Rust-native API, D2 `forbid(unsafe_code)`, D3 permissive-only dependencies, D7 clean-room) and to the
layering in `planning/10-architecture.md` §3 layer 5. Primary source: `planning/research/04-libavfilter.md`.
Dependency verdicts: `planning/research/09-dependency-licence-register.md`.

**Scope.** ~560 registered filter names upstream, of which ~87 are per-vendor hardware duplicates of ~20
distinct operations. Six infrastructure crates plus ~30 filter crates. This document specifies the
framework in enough detail to implement it, gives the exact user-facing filtergraph grammar, and tiers
the inventory.

**Clean-room framing, stated once.** Under D7 we port *nothing* — not the LGPL filters, not the GPL ones.
Every filter is written from a public specification, a published paper, the *documented* behaviour in
`filters.texi` (documentation is an interface fact, freely usable), or black-box observation of the
reference binary. The GPL list matters for one reason only: those filters are disproportionately likely to
have **no** published description, so their only available specification is source we may not read. §5.4
assesses each one individually.

---

# 1. `vaco-filter-core` — the framework

This is the load-bearing part of the plan. Everything else is filters written against it.

## 1.1 Object model

Four types, mirroring the roles FFmpeg splits across `AVFilter`/`AVFilterContext`/`AVFilterLink`/`AVFilterGraph`
but with Rust ownership instead of refcounts.

```rust
/// Static, const-constructible description. Lives in the registry; instantiating nothing.
/// This is what backs `vaco -filters` and `vaco -h filter=scale`.
pub struct FilterDesc {
    pub name:        &'static str,
    pub description: &'static str,
    pub inputs:      PadSpec,          // static pads, or Dynamic
    pub outputs:     PadSpec,
    pub caps:        FilterCaps,
    pub formats:     FormatDecl,       // §1.6
    pub options:     &'static OptionSchema,   // from vaco-opts
    pub timeline:    TimelineSupport,
    /// Construct an instance. Options are already parsed and validated against `options`.
    pub new:         fn(&Options, &InitCtx) -> Result<Box<dyn Filter>>,
}

pub enum PadSpec {
    Static(&'static [Pad]),
    /// Count is derived from options at init time (amix=inputs=N, split=N, ...).
    Dynamic { min: u32, max: u32, name_fn: fn(u32) -> Cow<'static, str>, media: MediaType },
}

pub struct Pad { pub name: &'static str, pub media: MediaType }
```

```rust
/// The instantiated filter's own behaviour. One method is mandatory.
pub trait Filter: Send {
    /// Do one bounded step of work. See §1.2.
    fn activate(&mut self, ctx: &mut FilterCtx<'_>) -> Result<Activity>;

    /// Called once per output pad, in topological order, after formats are fixed.
    /// Sets w/h/SAR/time_base/frame_rate/channels on the outlink.
    fn config_output(&mut self, _ctx: &mut ConfigCtx<'_>, _pad: usize) -> Result<()> { Ok(()) }
    /// Called once per input pad before `config_output`. Most filters ignore it.
    fn config_input(&mut self, _ctx: &mut ConfigCtx<'_>, _pad: usize) -> Result<()> { Ok(()) }

    /// Runtime command. Default: apply against the option schema, honouring RUNTIME flags.
    fn process_command(&mut self, _c: &Command) -> Result<CommandReply> { Err(Error::CommandNotSupported) }
}

pub enum Activity {
    /// Made progress. `again` = "I know I have more to do right now, re-queue me."
    Progressed { again: bool },
    /// Did nothing; waiting on the named inputs. The framework marks those links wanted.
    NeedInput(InputMask),
    /// Did nothing and cannot: downstream has not asked for anything.
    Idle,
    /// All outputs have been given a terminal status. The node is retired.
    Done,
}
```

```rust
/// Owned by the graph; the filter never holds one across calls.
pub struct FilterCtx<'g> {
    links:    &'g mut LinkArena,
    node:     &'g mut NodeState,       // pad→LinkId maps, timeline state, opts, nb_threads
    exec:     &'g Executor,            // slice-thread pool (§1.11)
    pool:     &'g FramePool,
}
```

**Why the arena.** The Rust obstacle is that `activate` needs `&mut self` on the filter *and* `&mut` on links
shared with neighbours. Solution: nodes and links live in two separate `Vec`s owned by the `Graph`, indexed by
`NodeId`/`LinkId`. The driver does

```rust
let Node { filter, state } = &mut self.nodes[id.0];       // borrows self.nodes
let mut ctx = FilterCtx { links: &mut self.links, node: state, .. };  // borrows self.links
filter.activate(&mut ctx)?;
```

which is ordinary disjoint-field borrowing — no `unsafe`, no `RefCell`, no `Rc`. A filter can never reach
another filter's private state, only link state, which is exactly the invariant FFmpeg maintains by
convention. Where a filter genuinely needs `&mut Graph` (only `movie`/`amovie`-style sources that own a
sub-pipeline, and the auto-convert insertion pass), we `Option::take` the boxed filter, run, and put it back;
one pointer move, still safe.

## 1.2 The scheduling model — decision

**Chosen: cooperative `activate` with a framework-computed readiness scheduler.**

Alternatives considered:

| Model | Shape | Why not |
|---|---|---|
| **async/await** | each filter is `async fn run(ctx)`, awaiting `ctx.input(i).next().await` | Multi-input await reads beautifully, but: (a) filter state becomes a self-referential generator, so every node is a `Pin<Box<dyn Future>>` and per-frame state lives in an opaque compiler-generated enum that no debugger, no `graphmonitor`, and no crash dump can inspect — we lose the ability to answer "why is this graph stuck?", which is *the* recurring filtergraph support question; (b) execution order becomes the executor's business, and we need bit-deterministic frame ordering for the D6 differential harness; (c) cancellation/EOF becomes drop-order semantics rather than an explicit status value. |
| **push dataflow** | `filter_frame(frame)` recursing downstream | No backpressure (a fast source with a slow sink allocates without bound), and it cannot express "I need the input that is *behind* in time", which every framesync filter requires. This is precisely why upstream abandoned it. |
| **`activate`** | one bounded step, framework re-queues | Explicit, inspectable state; deterministic ordering; natural backpressure via bounded per-link queues; multi-input synchronisation is just "look at both queues and decide". |

Two deliberate improvements over upstream:

1. **Readiness is computed, not asserted.** FFmpeg requires the filter to call `ff_filter_set_ready()` when it
   still has work; forgetting is a hang, and it is a recurring upstream bug class. We derive readiness from
   observable link state (§1.9) and treat `Activity::Progressed { again: true }` as a *hint*, not the sole
   mechanism. A filter that forgets it still gets re-queued if any of its links changed.
2. **Quiescence is diagnosed, not tolerated.** If the driver's ready set empties while a sink is not at EOF and
   no source is starved, that is a graph bug; we emit the blocked node, the link it is waiting on, that link's
   queue depth and status, rather than hanging.

## 1.3 The `Simple` and `FrameSync` adapters — the most important API decision after the trait

Writing `activate` correctly (status forwarding, wanted-frame propagation, EOF ordering, writability, timeline
gating, slice fan-out) is the hard part, and ~80% of the inventory does not need any of it. So `activate` is
implemented **once per shape** and almost no filter implements `Filter` directly.

```rust
/// 1-in, 1-out, one frame in → zero or more frames out. The default shape.
pub trait FrameFilter: Send {
    fn filter_frame(&mut self, ctx: &mut FrameCtx<'_>, input: Frame) -> Result<FrameOut>;
    /// Called once after input EOF, before the output status is set. Drain internal buffers.
    fn flush(&mut self, _ctx: &mut FrameCtx<'_>) -> Result<FrameOut> { Ok(FrameOut::None) }
}
pub enum FrameOut { None, One(Frame), Many(SmallVec<[Frame; 4]>) }

/// Blanket adapter: implements Filter for any FrameFilter.
pub struct Simple<F: FrameFilter>(pub F);
impl<F: FrameFilter> Filter for Simple<F> { fn activate(..) { /* written once */ } }
```

The adapter handles, in this order: check downstream demand → consume one input frame or a status →
evaluate the timeline expression → make the frame writable (`Arc::make_mut`) if the filter will mutate it →
call `filter_frame` → rescale timestamps if the output time base differs → push → set status on EOF.

Sibling adapters:

- `SliceFilter` (§1.11) — for the 157 filters that slice-thread; the adapter owns the fan-out.
- `FrameSyncFilter` (§3) — for the 68 multi-input filters; the adapter owns the alignment.
- `AudioFilter` — like `FrameFilter` but with a declared input frame size (`set_frame_size(n)`), the framework
  running a per-link `AudioFifo` so FFT-domain filters always see exactly N samples and a correctly-sized
  short final frame. Upstream's `ff_inlink_set_frame_size`; easy to forget and impossible to retrofit.
- `SourceFilter` — 0-in, 1-out; `fn produce(&mut self, ctx) -> Result<FrameOut>` called only when demanded.

Budgeting note: every hour spent on these five adapters is repaid ~100×. They are the first thing built and
the most heavily tested component in the subsystem.

## 1.4 Pads and links

```rust
pub struct Link {
    pub src: (NodeId, u32),           // node + output pad index
    pub dst: (NodeId, u32),
    pub media: MediaType,

    // --- negotiated, fixed after configuration ---
    pub format: LinkFormat,           // pix_fmt | (sample_fmt, rate, layout)
    pub color: ColorProps,            // space, range, primaries, trc, chroma_loc, alpha_mode
    pub w: u32, pub h: u32, pub sar: Rational,
    pub time_base: Rational,
    pub frame_rate: Rational,         // 0/0 = unknown/VFR
    pub hw: Option<Arc<HwFrameCtx>>,  // §7

    // --- runtime ---
    queue: FrameQueue,                // bounded FIFO
    audio_fifo: Option<AudioFifo>,    // when a frame size was requested
    status_in: Option<(Status, Ts)>,  // arrived from upstream, not yet visible downstream
    status_out: Option<Status>,       // delivered downstream; terminal
    frame_wanted: bool,               // downstream asked
    // --- diagnostics ---
    stats: LinkStats,                 // frames/samples passed, peak depth, latency
}

pub enum Status { Eof, Error(ErrorCode) }
```

One output pad connects to exactly one input pad — fan-out is `split`/`asplit`, as upstream. This is kept
deliberately: implicit fan-out would make frame ownership ambiguous and buffer accounting impossible.

**Dynamic pad counts** are resolved at instantiation: `FilterDesc::new` receives the parsed options and
returns, alongside the boxed filter, the realised pad lists. Concretely `InitCtx` exposes
`ctx.set_input_count(n)` / `set_output_count(n)`; the graph builder reads them back before link resolution.
Consequence for the graph parser: link resolution *must* run after instantiation (§2.5).

**Pad flags we drop.** `AVFILTERPAD_FLAG_NEEDS_WRITABLE` exists because C cannot express ownership. Ours does:
`filter_frame` takes `Frame` **by value**, and `frame.plane_mut()` performs `Arc::make_mut`, copying only if
the buffer is genuinely shared. The flag becomes a no-op and is removed. `AVFILTERPAD_FLAG_FREE_NAME` is
allocation bookkeeping and disappears with `Cow<'static, str>`.

## 1.5 Frame queues and backpressure

Per-link `FrameQueue` = `VecDeque<Frame>` plus a running byte count. Two caps:

- **Per-link**: `max_frames` (default 8) — soft; a filter may exceed it only while draining.
- **Per-graph**: `max_buffered_bytes` (default 512 MiB, `-filter_buffer` overrides) tracked in a shared
  `QueueBudget`. When the budget is exceeded, the driver stops granting `frame_wanted` to sources; the
  scheduler drains toward sinks instead. This is upstream's `FFFrameQueueGlobal` cap, expressed as a
  scheduler policy rather than an allocator hook.

Backpressure therefore has two layers: within the graph, `frame_wanted` is the pull signal; between the graph
and the rest of the pipeline, `vaco-sched`'s bounded channels (arch §6.1) are the signal. A filtergraph never
allocates unboundedly because it never produces without demand — the only exception is a filter with intrinsic
latency (`atempo`, `afir`, `tpad`, `loop`, `reverse`), which buffers by design and declares
`FilterCaps::BUFFERS_INPUT` so `latency`/`alatency` and the deadlock diagnostic can account for it.

## 1.6 Format negotiation — one design replacing two generations

Upstream carries two coexisting mechanisms: the legacy `query_formats()` callback that mutates
refcounted `AVFilterFormats` lists shared between links, and the newer declarative
`FILTER_PIXFMTS_ARRAY` / `FILTER_SINGLE_PIXFMT` / `FILTER_QUERY_FUNC2` states. The shared-refcount part is
the bug-prone half: `ff_set_common_formats` "shares" a list between links, and that sharing *is* the
constraint — it means "these links must agree" — but it is encoded as pointer identity with manual
ref/unref.

**Our design keeps the declarative surface and replaces pointer-sharing with an explicit equality relation.**

### 1.6.1 What a filter declares

```rust
pub enum FormatDecl {
    /// Default. Any format the two sides can agree on; all same-media pads share one value.
    Passthrough,
    /// One static list, applied to every same-media pad, all pads sharing one value.
    Static(&'static FormatList),
    /// Exactly one format on every same-media pad.
    Single(Format),
    /// General case: fill in per-pad constraints and declare equality groups.
    Query(fn(&QueryCtx, &mut Negotiation) -> Result<()>),
}
```

`Negotiation` is what a `Query` fn writes into:

```rust
pub struct Negotiation<'a> {
    pub inputs:  &'a mut [PadFormats],
    pub outputs: &'a mut [PadFormats],
    groups: EqualityGroups,     // union-find over (PadRef, Property)
}

impl Negotiation<'_> {
    /// "All these pads must end up with the same value for this property."
    pub fn tie(&mut self, prop: Property, pads: &[PadRef]);
    /// Sugar: tie every same-media pad, the common case.
    pub fn tie_all(&mut self, prop: Property);
}

pub struct PadFormats {
    pub pix_fmts:     Constraint<PixelFormat>,
    pub color_spaces: Constraint<ColorSpace>,
    pub color_ranges: Constraint<ColorRange>,
    pub color_prims:  Constraint<ColorPrimaries>,
    pub color_trcs:   Constraint<TransferCharacteristic>,
    pub alpha_modes:  Constraint<AlphaMode>,
    pub sample_fmts:  Constraint<SampleFormat>,
    pub sample_rates: Constraint<u32>,
    pub ch_layouts:   Constraint<ChannelLayout>,
}

/// Ordered by the filter's preference, best first.
pub enum Constraint<T> { Any, OneOf(SmallVec<[T; 12]>) }
```

Each `Constraint` also records the `NodeId` that introduced it — provenance, used only for diagnostics
(§1.6.5). That single field is the difference between a usable error message and FFmpeg's
"Impossible to convert between the formats supported by filter X and filter Y".

### 1.6.2 The wildcard rule for the "generic" properties

Colour space, colour range, colour primaries, transfer characteristics and alpha mode differ from pixel
format in one respect: an *unspecified* value is not a distinct format, it is "whatever arrives, preserved".
Upstream captures this in `merge_generic`. We encode it in the intersection rule:

```
intersect(A, B):
    Any            ∩ X              = X
    OneOf(a)       ∩ OneOf(b)       = OneOf( [x for x in a if x in b] )   // a's order wins
    // wildcard handling, for generic properties only:
    UNSPECIFIED ∈ a  and  b ≠ ∅     ⇒  UNSPECIFIED absorbs into b: result = b ∪ (a ∩ b)
```

i.e. a filter that says "I accept unspecified colour range" accepts a concrete one too and propagates it.
Same for `ChannelLayout::Unspecified(n)`, which matches any layout with exactly `n` channels — so
`amerge` can accept 6-channel input without caring whether it is 5.1 or 5.1(side).

### 1.6.3 The algorithm

```
negotiate(graph):

  round = 0
  loop:
    # ---- 1. DECLARE -----------------------------------------------------
    for node in graph.nodes:
        cfg = fresh Negotiation for node's realised pads
        apply node.desc.formats:
            Passthrough  -> every pad = Any;   cfg.tie_all(p) for every property p of the media type
            Static(list) -> every pad = OneOf(list); cfg.tie_all(p) for the property the list covers,
                            Any + tie_all for the rest
            Single(f)    -> every pad = OneOf([f]); tie_all
            Query(f)     -> f(&qctx, &mut cfg)
        record cfg into the global constraint table

    # ---- 2. LINK-EQUALITY ------------------------------------------------
    for link L in graph.links:
        for prop p applicable to L.media:
            union( group(L.src_pad, p), group(L.dst_pad, p) )

    # ---- 3. INTERSECT ----------------------------------------------------
    for class C in groups:
        S(C) = fold(intersect, constraints of every pad in C)

    # ---- 4. REPAIR -------------------------------------------------------
    empty = { C : S(C) = ∅ }
    if empty is non-empty:
        if !graph.auto_convert:  fail with diagnostic (§1.6.5)
        for each C in empty:
            L = the link whose union created the emptiness      # (union-find records the
                                                                #  merge that made it empty)
            for each property p that is empty across L:
                insert converter node for (p, L.media) between L.src and L.dst   # §1.7
        round += 1
        if round > graph.links.len() * 3:  fail "conversion did not converge"
        continue          # re-run with the new nodes
    break

  # ---- 5. PICK -----------------------------------------------------------
  for class C in topological order of the classes' earliest link:
      if |S(C)| == 1: value = the element
      else:           value = argmin over S(C) of conversion_loss(upstream_actual, candidate)
                              with the filter's declared preference order as the tiebreak
      assign value to every pad in C

  # ---- 6. CONFIGURE ------------------------------------------------------
  for node in topological order:
      for each input pad:  node.config_input(pad)
      for each output pad: node.config_output(pad)     # sets w/h/SAR/time_base/frame_rate
  validate: every link has a fixed format, non-zero dimensions (video) / channel count (audio),
            and a non-zero time_base.
```

Two properties worth stating:

- **Termination.** Each repair round inserts at least one converter and converters are `Passthrough`-free
  (they declare concrete input and output sets), so a repaired link cannot become empty again for the same
  property. The bound is `3 × |links|` and is a hard error, not a hang.
- **Determinism.** Union-find iteration order is by `LinkId`, and `PICK` is a total order (loss score, then
  the filter's declared preference, then the format enum's discriminant). Two runs on the same graph pick
  the same formats. Required for D6.

### 1.6.4 The loss function used by PICK

`conversion_loss(from, to) -> u32` is computed from `vaco-pixfmt` descriptors, derived independently from
the pixel-format table (D7: descriptor metadata is generated from our own declarative table, arch §3 layer 1):

| Component | Weight | Condition |
|---|---:|---|
| depth loss | 0x8000 × (bits lost) | `to.depth < from.depth` |
| chroma resolution loss | 0x4000 per halved axis | subsampling coarser than the source |
| colour-model change | 0x2000 | YUV↔RGB, or either ↔ grey |
| alpha loss | 0x1000 | source has alpha, target does not |
| colour-space change | 0x0800 | matrix coefficients differ |
| range change | 0x0400 | limited↔full |
| chroma-siting change | 0x0100 | |
| endianness/packing change | 0x0010 | costs a pass, loses nothing |

Audio uses the same shape: sample-depth loss dominates, then float→int, then rate change (any resampling is
a fixed cost), then downmix (channel loss weighted by which channels are dropped).

### 1.6.5 Failure diagnostics

Negotiation failure is the single most common filtergraph user error, and upstream's message names two
filters that are frequently not the ones responsible. Because every `Constraint` carries the `NodeId` that
narrowed it, we can report the actual culprit even when it is several links away:

```
error: format negotiation failed for `pix_fmt` on link  scale@0:default -> myfilt@0:default

  the link requires one common pixel format, but the two sides share none:

    upstream side   accepts  yuv420p yuv422p yuv444p nv12  (+8 more)
      narrowed by   scale@0      (declared list)
      narrowed by   format@0     (option pix_fmts=yuv420p,yuv422p,yuv444p,nv12)

    downstream side accepts  gbrpf32le gbrapf32le
      narrowed by   myfilt@0     (declared list)

  auto-conversion is disabled (-noauto_conversion_filters); a `scale` filter would
  normally have been inserted here.

  fix: remove -noauto_conversion_filters, or insert `format=gbrpf32le` before myfilt@0.
```

Three things this gets right that upstream does not: it names the *narrowing* node rather than the two link
endpoints, it says what would have happened with auto-conversion on, and it prints a concrete fix. The
diagnostic renderer lives in `vaco-filter-core` and is exercised by snapshot tests, one per failure mode.

## 1.7 Auto-inserted conversion filters

`vaco-filter-core` must not know that a filter named `scale` exists — layer 5a cannot depend on layer 5b
crates. So core defines the mechanism and the graph layer supplies the policy:

```rust
// vaco-filter-core
pub trait ConverterFactory: Send + Sync {
    /// What to insert to fix this property mismatch on this link, if anything.
    fn converter(&self, prop: Property, media: MediaType, hint: &ConvHint)
        -> Option<ConverterSpec>;
}
pub struct ConverterSpec { pub filter: &'static str, pub args: String }
```

```rust
// vaco-filter-graph — the default policy, matching FFmpeg's AVFilterFormatsMerger table
impl ConverterFactory for DefaultConverters {
    fn converter(&self, prop, media, hint) -> Option<ConverterSpec> {
        match (media, prop) {
            (Video, PixFmt | ColorSpace | ColorRange | ColorPrimaries | ColorTrc)
                => Some(spec("scale",  &self.sws_opts)),
            (Video, AlphaMode)
                => Some(spec("premultiply_dynamic", "")),
            (Audio, SampleFmt | SampleRate | ChLayout)
                => Some(spec("aresample", &self.swr_opts)),
            _   => None,       // hardware-context mismatch: never auto-inserted, see below
        }
    }
}
```

**Coalescing.** If a link is empty on pix_fmt *and* colour range *and* colour space, one `scale` fixes all
three — the repair pass groups properties by the converter they map to and inserts one node per group. This
matters: naive per-property insertion would stack three `scale` filters and triple the cost.

**Where the options come from.** Three sources feed `sws_opts`, in increasing precedence:

1. `Graph::default_scale_opts` — set by the application (`vaco -sws_flags bicubic`).
2. The `sws_flags=...;` **graph-string prefix** (§2.1). Parsed by `vaco-filter-graph`, stored on the graph,
   applied to every auto-inserted `scale` in that graph. This is the exact upstream mechanism
   (`AVFilterGraph.scale_sws_opts`) and we reproduce it because it is user-facing syntax.
3. Nothing per-link — auto-inserted converters are not individually addressable. If a user needs per-link
   control, they write the `scale` filter explicitly, which is the documented answer upstream too.

The audio equivalent is `-aresample_swr_opts`, stored as `Graph::default_resample_opts`. There is no
graph-string prefix for it; only the application-level option exists, matching upstream.

**Hardware contexts are never auto-converted.** A CPU→GPU or GPU→CPU link is an error naming
`hwupload`/`hwdownload`/`hwmap` as the fix, exactly as upstream. Auto-inserting a device transfer would hide
a per-frame PCIe round-trip behind a silent default, which is the wrong thing to do quietly (§7).

**Disabling.** `Graph::set_auto_convert(AutoConvert::None | ::All)`, surfaced as
`-noauto_conversion_filters`. With it off, step 4 of §1.6.3 fails immediately with the diagnostic above.

**Recorded in the graph dump.** Auto-inserted nodes are named `auto_scale_0`, `auto_aresample_0`,
`auto_premultiply_0` and are flagged `inserted: true` so `-dumpgraph`, `graphmonitor` and the error
messages can distinguish them from what the user wrote. Upstream names them `auto_scale_N` too; we match
the naming because scripts grep for it.

## 1.8 EOF, status and timestamp propagation

### 1.8.1 Status

A link carries at most one terminal `Status`, and it is *ordered behind the frames already queued*:

```rust
impl InputLink<'_> {
    /// The only way a filter reads an input.
    pub fn poll(&mut self) -> Poll {
        if let Some(f) = self.queue.pop() { return Poll::Frame(f); }
        if let Some((s, pts)) = self.status_in.take() { return Poll::Status(s, pts); }
        Poll::Pending
    }
    pub fn peek(&self) -> Option<&Frame>;
    pub fn len(&self) -> usize;
    pub fn request(&mut self);            // set frame_wanted upstream
    pub fn set_frame_size(&mut self, n: u32);   // audio: install the FIFO
}

impl OutputLink<'_> {
    pub fn wanted(&self) -> bool;
    pub fn push(&mut self, f: Frame) -> Result<()>;
    pub fn set_status(&mut self, s: Status, pts: Ts);   // terminal; further pushes are a defect
    pub fn is_closed(&self) -> bool;
}
```

`Poll::Status(Eof, pts)` carries the timestamp at which the stream ended, which `tpad`, `xfade`, `concat`
and framesync all need. `Status::Error(code)` propagates downstream identically, so a decoder error
surfaces at the sink rather than being swallowed mid-graph.

Generic helpers, covering ~90% of filters and used by the adapters:

```rust
ctx.forward_status_all();     // any input EOF -> all outputs EOF, with pts rescaled per outlink tb
ctx.forward_wanted_all();     // any output wanted -> request on all inputs
ctx.propagate_eof(from_in, to_out);
```

### 1.8.2 The rule that prevents the classic hang

An output that is `wanted` and whose filter returns `Idle` without requesting any input is a defect: nothing
will ever wake it. The framework asserts this in debug and, in release, force-requests every input as a
recovery, logging once. This turns upstream's silent hang into a loud, attributable warning.

### 1.8.3 Timestamps

- Frames carry `pts: Option<i64>` in the **link's** time base. There is no `AV_NOPTS_VALUE` sentinel; absence
  is `None`. Every arithmetic path must handle `None` explicitly, which the type system enforces.
- A filter that does not alter timing leaves `Link::time_base` equal to its input's; the adapters do this
  automatically. A filter that alters rate (`fps`, `atempo`, `framerate`, `minterpolate`, `setpts`) sets its
  own in `config_output` and the adapter rescales on push via `vaco-core`'s explicit rounding modes
  (`Rounding::NearInf` by default, matching `av_rescale_q`'s default).
- `duration` propagates alongside `pts`; audio duration is derived from `nb_samples` and the rate rather
  than carried, so it cannot disagree with the data.
- `setpts`/`asetpts` evaluate a `vaco-expr` expression with the documented variable set
  (`N`, `NB_CONSUMED_SAMPLES`, `PTS`, `STARTPTS`, `PREV_INPTS`, `PREV_OUTPTS`, `PREV_INT`, `PREV_OUTT`,
  `T`, `TB`, `RTCTIME`, `RTCSTART`, `FRAME_RATE`, `SAMPLE_RATE`, `INTERLACED`, `POS`→`NAN`, `S`, `SR`).
  Interface names are reproducible per D9; the expressions themselves are evaluated by our own engine.
- **Monotonicity is not enforced.** Filters may emit non-monotonic pts (`reverse`, `setpts`); the muxer is
  where that becomes an error. The graph only warns once per link, gated behind `-loglevel verbose`.

## 1.9 The readiness scheduler

```rust
pub struct Graph {
    nodes: Vec<Node>,
    links: Vec<Link>,
    ready: BinaryHeap<Ready>,        // (priority, seq, NodeId)
    queued: FixedBitSet,             // dedup: a node is in the heap at most once
    seq: u64,                        // FIFO tiebreak -> determinism
    budget: QueueBudget,
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Ready { prio: Priority, seq: Reverse<u64>, node: NodeId }

enum Priority {
    Idle       = 0,   // nothing known; only reached via an explicit request
    Wanted     = 1,   // an output is wanted but no input is available
    HasStatus  = 2,   // an input carries an unconsumed EOF/error
    HasFrame   = 3,   // an input has a queued frame and the output is wanted
    SelfDriven = 4,   // returned Progressed{again:true} last time
}
```

Readiness is **recomputed** whenever a link's observable state changes — a push, a pop, a status set, a
`request()`. `Link` mutation goes through methods that call `graph.notify(link)`, which re-scores both
endpoints and re-heaps them if the score rose. That is why forgetting to "set ready" cannot hang us: the
signal is the state change, not the filter's memory of it.

```
run_once(graph) -> Progress:
    node = graph.ready.pop()  else return Progress::Quiescent
    graph.queued.clear(node)
    act = graph.activate(node)?
    match act:
        Progressed{again: true}  -> graph.push_ready(node, SelfDriven)
        Progressed{again: false} -> (readiness follows from the notifications the step produced)
        NeedInput(mask)          -> for i in mask: graph.links[node.input(i)].request()
                                    // request() notifies the *upstream* node, waking it
        Idle                     -> nothing; see §1.8.2
        Done                     -> graph.retire(node)
    Progress::Stepped

run(graph, deadline) -> GraphStatus:
    loop:
        match run_once(graph):
            Stepped   -> continue (respect deadline / step budget for -filter_threads=1 fairness)
            Quiescent -> break
    classify_quiescence(graph)
```

`classify_quiescence` is the diagnostic that upstream lacks:

| Condition | Result |
|---|---|
| every sink at EOF | `GraphStatus::Eof` |
| some source link is `wanted` and unfed | `GraphStatus::NeedInput(Vec<OpenPad>)` — normal; `vaco-sched` feeds a buffersrc |
| every open output has frames pending | `GraphStatus::HasOutput(Vec<OpenPad>)` |
| none of the above | `GraphStatus::Deadlock { node, link, queue_depth, status }` — a bug, reported with the full link table |

**Fairness and latency.** Draining depth-first toward sinks minimises buffered bytes; draining
breadth-first minimises latency for live use. The heap's priority order is naturally sink-biased
(`HasFrame` beats `Wanted`), which is the right default. `-filter_latency low` flips the tiebreak to
prefer the node nearest a source, at the cost of deeper queues.

**Determinism.** Given the same node set, the same link states and the same input frames, `run_once`
picks the same node every time: priorities are a total order and `seq` breaks ties FIFO. Slice threading
(§1.11) is data-parallel within one `activate` call and does not affect ordering. This is what makes
`framecrc`-style differential testing meaningful for filtergraphs.

## 1.10 Timeline (`enable=`) and runtime commands

### 1.10.1 Timeline

Every filter declaring timeline support gets a universal `enable` option, parsed once into a compiled
`vaco-expr` program.

```rust
pub enum TimelineSupport { None, Generic, Internal }

pub struct Timeline {
    program: Option<expr::Program>,
    enabled: bool,          // result for the frame currently being processed
    vars: TimelineVars,     // t, n, w, h, pos(-> NaN, warn once)
}
```

- **`Generic`** — the framework evaluates before `filter_frame` and, when false, forwards the input frame
  to output 0 untouched. Registration asserts the filter has exactly one input and one output of the same
  media type and that the two links negotiate to one equality class; otherwise "pass through unchanged"
  is not well defined. Upstream leaves this as a convention; we check it at registry-build time.
- **`Internal`** — the framework computes `ctx.enabled` and the filter consults it. Needed when the filter
  must keep temporal state advancing while disabled (`tmix`, `deflicker`, `atadenoise`), or when the
  decision is per-slice (`SliceJob::enabled`, so a slice-threaded filter evaluates once and fans the bool
  out rather than evaluating per band).

Variables: `t` (seconds, `NAN` when pts is `None`), `n` (frame index), `w`, `h`, and `pos` which is
permanently `NAN` with a one-shot deprecation warning — matching upstream's current behaviour so scripts
that reference it do not hard-fail.

Evaluation happens exactly once per frame per node, in `FilterCtx::begin_frame`, and the result is
recorded in `LinkStats` so `graphmonitor` can show which nodes are currently gated off.

### 1.10.2 Commands

```rust
pub struct Command<'a> { pub name: &'a str, pub arg: &'a str, pub flags: CommandFlags }
pub enum CommandReply { Ok, Text(String) }
```

Default `process_command` implementation, provided by a derive on the option schema: look the command name
up in the filter's `OptionSchema`, require `OptionFlags::RUNTIME` (rendered as `T` in `-h filter=`), parse
the argument with the option's own parser, assign, and call an optional `on_option_changed` hook so filters
that precompute tables (biquads, curves, lut3d) can rebuild them. `enable` is handled generically for every
timeline filter. Filters override only when a command is not a plain option (`ebur128`'s `reset`,
`volume`'s `replaygain_noclip`, `astreamselect`'s `map`).

Delivery, three paths:

1. **Immediate** — `Graph::send_command(target, name, arg, flags)`. `target` is `all`, a filter name
   (`"volume"` = every instance), or an instance tag (`"volume@boost"`). `CommandFlags::ONE` stops at the
   first match; `::FAST` rejects commands the filter says are slow.
2. **Queued** — `Graph::queue_command(ts, target, ...)`, consumed at the first frame whose pts ≥ ts.
   Backs `-filter_complex` timed commands and the CLI.
3. **In-graph** — the `sendcmd` / `asendcmd` filters parse a command script and enqueue at path 2. Grammar
   reproduced exactly (interval `START[-END] COMMANDS`, `COMMANDS = [FLAGS] TARGET COMMAND ARG`, flags
   `enter`/`leave`/`expr`, `;`-separated, `#` comments, `-/file` loading).

**`zmq` / `azmq` are out.** libzmq is FFI (D10 Gate 1) and is MPL-2.0 (Gate 2) — excluded twice over. In its
place, an optional `cmdsocket` / `acmdsocket` filter accepting the same `TARGET COMMAND ARG` grammar over a
line-oriented TCP socket (pure `std::net`, no dependency). We do **not** claim `zmq` compatibility or
register the name; a graph using `zmq` gets an error naming `cmdsocket` as the replacement.

## 1.11 Slice threading with safe disjoint mutable access

157 filters slice-thread upstream. In C the framework hands every worker the same `AVFilterContext*` and
trusts each to write only its own rows. We express the same parallelism as *ownership*, which makes the
discipline checked rather than assumed.

```rust
pub trait SliceFilter: Send + Sync {
    /// &self, not &mut self: shared filter state is read-only during the parallel section.
    fn slice(&self, job: SliceJob<'_>) -> Result<()>;
    fn slice_count(&self, height: u32, threads: u32) -> u32 { threads.min(height) }
}

pub struct SliceJob<'a> {
    pub index: u32, pub count: u32,
    pub y0: u32, pub y1: u32,               // rows of the OUTPUT this job owns
    pub input:  &'a [PlaneRef<'a>],         // whole input planes, immutable, full height
    pub output: &'a mut [PlaneBandMut<'a>], // exactly rows [y0,y1) of each output plane
    pub scratch: &'a mut Scratch,           // this job's private scratch, from a pre-split Vec
    pub enabled: bool,                      // timeline, evaluated once per frame
}
```

The adapter builds the bands with `chunks_mut` / `split_at_mut` on each output plane (accounting for chroma
subsampling, so a job owning luma rows `[y0,y1)` owns chroma rows `[y0>>vsub, y1>>vsub)` and slice
boundaries are forced to multiples of the subsampling factor). It then runs them under a per-graph
`rayon::ThreadPool` sized from `-filter_threads`.

Three consequences, all improvements:

- **Vertical-overlap kernels are free.** A blur with radius R, or a deinterlacer reading `y-1`/`y+1`, reads
  from `input`, which is whole and immutable. No band clamping, no duplicated edge logic, no risk of
  reading a neighbour's half-written output. In C this is a per-filter correctness obligation.
- **In-place is opt-in and explicit.** The framework allocates a distinct output frame from `vaco-pool`
  unless the filter declares `SliceMode::InPlaceRowIndependent`, in which case the *same* frame is split
  once and each band is both read and written by its owner — so the kernel is statically prevented from
  reading outside its band. A filter needing in-place *and* overlap is simply not slice-threadable in that
  mode, and says so rather than being subtly wrong.
- **Reductions are deterministic.** Filters that accumulate (`signalstats`, `histogram`, `psnr`, `entropy`)
  write per-band accumulators into `scratch`, and the adapter folds them in **index order** after the
  parallel section. Floating-point sums therefore do not depend on thread scheduling — required for D6.

Filter state that must mutate across the whole frame (temporal buffers, ring indices) is updated outside
the parallel section, before or after; `&self` in `slice` makes that structurally obvious.

`nb_threads` resolution order: per-filter `threads=` option → graph `-filter_threads` → `-threads` →
`available_parallelism()`. Per arch §6 we do **not** inherit upstream's hardcoded 16-thread ceiling; the
default is measured per filter class and recorded in the benchmark suite.

## 1.12 Capability flags

```rust
bitflags! {
    pub struct FilterCaps: u32 {
        const DYNAMIC_INPUTS   = 1 << 0;  // pad count from options
        const DYNAMIC_OUTPUTS  = 1 << 1;
        const SLICE_THREADS    = 1 << 2;
        const METADATA_ONLY    = 1 << 3;  // touches no sample data -> may pass hw frames through
        const HWDEVICE         = 1 << 4;  // accepts an explicit device context
        const MANAGES_HWFRAMES = 1 << 5;  // suppress automatic hw_frames_ctx propagation
        const BUFFERS_INPUT    = 1 << 6;  // intrinsic latency; excluded from deadlock detection
        const COMMAND_SUPPORT  = 1 << 7;  // has runtime-settable options
    }
}
```

Mapping from upstream, with the deltas called out:

| Upstream | Ours | Note |
|---|---|---|
| `DYNAMIC_INPUTS` / `DYNAMIC_OUTPUTS` | same | |
| `SLICE_THREADS` | same | but implied by implementing `SliceFilter` |
| `METADATA_ONLY` | same | |
| `HWDEVICE` | same | |
| `FF_FILTER_FLAG_HWFRAME_AWARE` | `MANAGES_HWFRAMES` | inverted-sense name; the upstream one reads backwards |
| `SUPPORT_TIMELINE_GENERIC` / `_INTERNAL` | `TimelineSupport` enum on the descriptor | a 3-state enum, not two flags that can both be set |
| `AVFILTERPAD_FLAG_NEEDS_WRITABLE` | **removed** | ownership + `Arc::make_mut` carries it |
| `AVFILTERPAD_FLAG_FREE_NAME` | **removed** | `Cow<'static, str>` carries it |
| — | `BUFFERS_INPUT` | new; needed by the deadlock diagnostic and by `latency` |
| — | `COMMAND_SUPPORT` | new; derived from the option schema, drives `-h filter=` output |

The registry build (arch §6, generated module) asserts consistency at compile time: `Generic` timeline
support implies exactly 1-in/1-out same-media; `SLICE_THREADS` implies a `SliceFilter` impl;
`DYNAMIC_INPUTS` implies the descriptor's `PadSpec::Dynamic`.

## 1.13 Buffer sources and sinks

`buffer` / `abuffer` (sources) and `buffersink` / `abuffersink` live **in `vaco-filter-core`**, not in a
leaf crate, because they need privileged access to link internals (a source pushes directly into the link
queue; a sink holds frames without a downstream) and because they are the API boundary every consumer of
the subsystem uses. Their public surface is the graph's I/O API:

```rust
impl Graph {
    pub fn source(&mut self, label: &str) -> Result<SourceHandle>;
    pub fn sink(&mut self, label: &str) -> Result<SinkHandle>;
}
impl SourceHandle {
    pub fn send(&mut self, g: &mut Graph, f: Frame) -> Result<()>;
    pub fn close(&mut self, g: &mut Graph);              // EOF
    pub fn wants(&self, g: &Graph) -> bool;              // backpressure signal for vaco-sched
}
impl SinkHandle {
    pub fn recv(&mut self, g: &mut Graph) -> Result<Option<Frame>>;
    pub fn format(&self, g: &Graph) -> &LinkFormat;      // valid after configuration
    pub fn set_frame_size(&mut self, g: &mut Graph, n: u32);   // abuffersink
    /// Restrict what the sink accepts, before configuration; drives negotiation from the tail.
    pub fn constrain(&mut self, g: &mut Graph, c: PadFormats);
}
```

`SinkHandle::constrain` is upstream's `av_buffersink_set_*` family and it matters: it is how `vaco` tells
the graph "the encoder only takes yuv420p", which is what makes the auto-`scale` land in the right place.

---

# 2. `vaco-filter-graph` — the filtergraph DSL

This is user-facing syntax we must match exactly. Every rule below is an interface fact (D9: option and
syntax names are reproducible; upstream's prose is not) and every escaping rule is verified against the
reference binary by the differential harness rather than against its source.

## 2.1 Grammar

```
GRAPH        ::= [ WS ] [ SWS_PREFIX ] FILTERCHAIN { ";" FILTERCHAIN } [ WS ]

SWS_PREFIX   ::= "sws_flags" "=" FLAGSTRING ";"

FILTERCHAIN  ::= [ WS ] FILTER { [ WS ] "," [ WS ] FILTER } [ WS ]

FILTER       ::= { LINKLABEL } FILTER_NAME [ "=" ARGUMENTS ] { LINKLABEL }

FILTER_NAME  ::= NAME [ "@" NAME ]

LINKLABEL    ::= "[" [ WS ] NAME [ WS ] "]"

ARGUMENTS    ::= ARGUMENT { ":" ARGUMENT }

ARGUMENT     ::= POSITIONAL | KEYVALUE
POSITIONAL   ::= VALUE
KEYVALUE     ::= NAME "=" VALUE

NAME         ::= ( ALNUM | "_" | "-" ) { ALNUM | "_" | "-" }
VALUE        ::= { ESCAPED_CHAR | QUOTED_RUN | PLAIN_CHAR }
LIST_VALUE   ::= VALUE { "|" VALUE }          -- option-level, not graph-level (see §2.3)
WS           ::= { " " | "\t" | "\n" | "\r" }
```

Rules the grammar alone does not carry:

1. **`sws_flags=` must be the very first token of the whole graph string**, and everything from the `=` up
   to the first `;` is one opaque flag string handed verbatim to every auto-inserted `scale` (§1.7). It is
   not a filter and not part of any chain. A `sws_flags=` appearing anywhere else is parsed as an ordinary
   filter name and fails with "no such filter: sws_flags".
2. **Positional arguments must precede all `key=value` arguments.** Mixing is legal; reversing the order is
   an error: `scale=w=640:480` → `error: positional argument after named argument`. Positional values map
   to the option schema's declared order, which is per-filter and part of the compatibility surface.
3. **Whitespace is skipped around `,` `;` `[` `]` and around the `=` that introduces arguments; it is
   significant everywhere inside a VALUE.** `drawtext=text=a b` draws `a b`. `drawtext = text=a` is the
   same filter as `drawtext=text=a`.
4. **An empty filterchain is an error** (`a;;b`), as is an empty graph, a chain that is only whitespace,
   and a label with an empty name (`[]`).
5. **Instance tags.** `name@id` gives the instance an explicit id. Without one, the instance is
   auto-named `name@N` with `N` a per-name counter in parse order (`scale@0`, `scale@1`). The tag is what
   `sendcmd` targets, what `graphmonitor` displays, and what error messages name. Duplicate explicit tags
   for the same filter name are an error.
6. **Chain-internal auto-connection.** See §2.5.

## 2.2 Worked examples of the surface

```
                                       # simplest: one chain, unlabeled ends
scale=640:480,format=yuv420p

                                       # graph-level scaler options
sws_flags=bicubic+accurate_rnd;scale=1280:-2

                                       # two chains joined by a named label
[0:v]scale=640:360[small];[small][1:v]overlay=10:10[out]

                                       # instance tags for sendcmd targeting
[0:a]volume@boost=2.0[a];[a]acompressor@comp=threshold=0.1[out]

                                       # dynamic pad counts come from options
[0:a][1:a][2:a]amix=inputs=3:duration=longest[out]
[0:v]split=3[a][b][c]

                                       # list-valued option (| separated)
format=pix_fmts=yuv420p|yuv422p|nv12

                                       # positional then named
overlay=10:10:eof_action=pass
```

## 2.3 Escaping — three levels, exactly

There are three independent layers. Each is a separate mechanical pass, and the canonical rule of thumb is
that **each level doubles the backslashes**.

### Level 1 — inside an option value

The value scanner honours two mechanisms:

- `\X` — a backslash escapes the following character, which is then literal. `\\` is a literal backslash.
- `'...'` — a single-quoted run: every character between the quotes is literal, including `:` `\` `[` `]`
  `,` `;`. **Quotes do not nest and a `'` cannot appear inside a quoted run** — close the quote, escape the
  apostrophe, reopen: `'it'\''s'` yields `it's`.

Characters that *must* be escaped at level 1: `:` (argument separator), `'`, `\`. Additionally `|` inside
a list-valued option, since `|` is the list separator at option-parse time (which is a *fourth*,
option-internal layer that only list-valued options have — worth naming so it is not mistaken for level 2).

### Level 2 — inside the filtergraph

The graph scanner runs first, with the same two mechanisms (`\X` and `'...'`) but a different stop set:
`[` `]` `,` `;` terminate a filter description. So a `,` intended as data must be escaped here even if it
was already fine at level 1.

Because the two levels use the same escape character and are applied in sequence (graph scan → argument
split → option unescape), a character needing protection at both levels needs `\\` at the outer layer.

### Level 3 — the shell

Not implemented by us, but it is part of every real command line and our documentation must show it. In
`sh`, `'...'` protects everything except `'`; a literal `'` is written `'\''`.

### The canonical worked example

Drawing the literal text `this is a 'string': may contain one, or more, special characters`:

```
level 1 — as the value of drawtext's `text` option, in a file or via the API:
    this is a 'string': may contain one, or more, special characters

level 1 escaped — so the `:` does not split the argument list:
    text=this is a \'string\'\: may contain one, or more, special characters

level 2 — embedded in a filtergraph, so the `,` do not split the chain and the level-1
          backslashes survive the graph scanner:
    drawtext=text=this is a \\\'string\\\'\\: may contain one\, or more\, special characters

level 3 — passed through sh, single-quoted:
    ffmpeg -i in -vf 'drawtext=text=this is a \\\'\''string\\\'\''\\: may contain one\, or more\, special characters' out
```

The equivalent using a quoted run at level 1, which most users find easier:

```
    -vf "drawtext=text='this is a '\\\''string'\\\''\: may contain one, or more, special characters'"
```

**These strings are test vectors, not prose.** They go into `crates/filter/graph/tests/escaping.rs` as
snapshot cases and into the differential harness, where the acceptance criterion is that our parser and the
reference binary produce the same rendered output for the same command line. That is the clean-room-correct
way to pin escaping behaviour: observe the shipped binary, never read its source.

**Escape hatches we must also provide**, because they exist precisely to let users avoid all of the above,
and scripts depend on them: `drawtext`'s `textfile=`, `sendcmd`'s `filename=`, and the CLI-level
`/`-prefixed option name that loads a value from a file (`drawtext=/text=/path/to/file.txt`). That last one
is implemented in `vaco-opts` at argument-parse time, not in the graph parser.

## 2.4 Parser design

Hand-written, single-pass, span-carrying recursive descent over `&str`. No regex, no parser-combinator
dependency — the grammar is small and the error quality matters more than the line count.

```rust
/// The core primitive. One escaping-aware scan, parameterised by what terminates a token.
fn next_token(s: &str, at: &mut usize, stop: StopSet) -> Result<Token>;

bitflags! { struct StopSet: u8 {
    const ARG   = 1;   // ':'
    const GRAPH = 2;   // '[' ']' ',' ';'
    const LIST  = 4;   // '|'
} }
```

`next_token` walks bytes, tracking whether it is inside a quoted run, honouring `\`, and stopping only on an
*unescaped, unquoted* character in the stop set. This single function is what makes the levels compose
correctly: `\[` inside an argument never starts a link label, because the graph scan asked for
`StopSet::GRAPH` and the backslash suppressed it.

Every `Token` carries `Span { start, end }` into the original string. Errors render with a caret:

```
error: unterminated quoted string
  --> filtergraph:1:34
   |
 1 | [0:v]drawtext=text='hello world,scale=640:480
   |                    ^ quote opened here, never closed
```

Pipeline:

```
parse(src) -> Ast
    1. strip a leading `sws_flags=...;`  (store on the Ast)
    2. split into chains on unescaped `;`
    3. per chain, split into filters on unescaped `,`
    4. per filter, scan leading `[label]`s, then NAME[@ID], then optional `=ARGS`, then trailing `[label]`s
    5. per filter, split ARGS on unescaped `:` into raw (span, text) pairs — do NOT unescape yet
```

Argument *values* are unescaped lazily, at option-application time, because the correct unescaping depends
on the option's type (a list-valued option splits on `|` first, then unescapes each element). Doing it
eagerly is the classic source of "why does my regex option need four backslashes" bugs.

The `Ast` is a public type. It is what `-dumpgraph`, `Graph::to_dot()` and any future GUI consume, and it
round-trips: `parse(print(parse(s))) == parse(s)`, property-tested with `proptest`.

## 2.5 Instantiation, link resolution, validation

**Order matters and is not obvious.** Pad counts depend on options (`amix=inputs=3`, `split=4`), so links
cannot be resolved from the AST alone:

```
build(ast, registry) -> Graph
  1. INSTANTIATE   for each filter node in parse order:
                     look up FilterDesc by name (error: "no such filter: 'x'"; suggest by edit distance)
                     parse arguments against the option schema (positional order, then key=value)
                     call desc.new(); the filter may set its realised input/output pad counts
                     assign the instance tag (explicit `@id`, else `name@N`)
  2. RESOLVE       walk chains left to right, connecting labels and unlabeled pads (below)
  3. VALIDATE      structural checks (below)
  4. NEGOTIATE     §1.6
  5. CONFIGURE     §1.6.3 step 6
```

### Link resolution

Two mechanisms, applied in this order per filter node:

**Explicit labels.** Maintain `open_outputs: HashMap<Name, (NodeId, pad)>`.
- A leading `[L]` on a filter: if `L` is in `open_outputs`, connect and remove it. Otherwise record `L` as
  an *unmatched input* — it may be satisfied by a later chain, or it stays open and is exported.
- A trailing `[L]`: if `L` is an unmatched input, connect and remove. Otherwise insert into `open_outputs`.
  A second trailing `[L]` for a name already in `open_outputs` is an error: `duplicate output label 'L'`.

**Unlabeled auto-connection, within a chain.** After processing filter *k*, carry forward the list of its
output pads that received no label. When processing filter *k+1*, connect that carried list positionally to
*k+1*'s unlabeled input pads, in pad order, taking the shorter length. Leftovers on either side stay open.

Concretely, `split[a][b],scale` is an error (`split`'s outputs are both labelled, `scale` has no input),
while `split,scale` connects `split:output0 → scale:input0` and leaves `split:output1` open. And
`[0:v][1:v]overlay,scale=640:480` connects `overlay:default → scale:default` with no label needed.

**Exported endpoints.** Whatever remains unmatched at the end becomes the graph's open pads:

```rust
pub struct OpenPad { pub label: Option<String>, pub node: NodeId, pub pad: u32, pub media: MediaType }
impl Graph { pub fn open_inputs(&self) -> &[OpenPad]; pub fn open_outputs(&self) -> &[OpenPad]; }
```

For the simple `-vf` / `-af` case (one chain, one open input, one open output, both unlabeled) the
conventional names `in` and `out` are assigned, and `vaco` attaches a buffersrc/buffersink automatically.
For `-filter_complex`, labels of the form `N:v` / `N:a` / `N:s` are resolved against input files by
`vaco-cli-core`, and any remaining open output must be mapped with `-map [label]`.

### Validation

Run before negotiation; each produces a span-anchored diagnostic:

| Check | Message |
|---|---|
| unknown filter name | `no such filter: 'scael'` + `did you mean 'scale'?` |
| media type mismatch across a link | `cannot connect audio output volume@0:default to video input scale@0:default` |
| unconnected input pad | `input pad 'default' of overlay@0 is not connected` |
| unconnected output pad, not exported | `output pad 'output1' of split@0 is not connected; label it and -map it, or use split=1` |
| duplicate output label | `duplicate output label 'v'; first defined at 1:14` |
| label used as input twice | `label 'v' consumed twice; insert split` |
| cycle | `filtergraph contains a cycle: scale@0 -> crop@0 -> scale@0` |
| dynamic pad count out of range | `amix accepts 1..=32 inputs, got 0` |

**Cycles.** The graph must be a DAG. The `feedback` filter appears to violate this but does not: it exposes
its loop as two ordinary pads that the *user* wires, and the framework treats the back edge as a link with
one frame of intrinsic delay (`BUFFERS_INPUT`), which breaks the cycle for both scheduling and validation.
Cycle detection is a Kahn topological sort, whose result is reused by negotiation step 6 and by the
scheduler's initial priority assignment.

### Introspection

- `Graph::to_dot()` — Graphviz, with format/size/timebase on every edge. Not upstream-compatible; better.
- `Graph::dump()` — a textual table close in spirit to `avfilter_graph_dump`, used by `-dumpgraph` and
  `graphmonitor`. We do not attempt byte-identical output here; the differential harness allowlists it,
  because it is diagnostic prose rather than an interface fact.

---

# 3. `vaco-filter-framesync` — multi-input timestamp alignment

68 filters need it (overlay, blend, psnr, ssim, lut2, the masked* family, the stack family, remap,
displace, guided, threshold, paletteuse, premultiply, mergeplanes, alphamerge, midequalizer, corr,
xcorrelate, convolve/deconvolve, mix, xmedian, varblur, vif, xpsnr, streamselect, limitdiff, bm3d, msad,
scale2ref, hysteresis, colormap, identity, and the `t`-prefixed temporal variants). It is its own crate
because it is the second-largest shared behaviour after the core itself, and because its option set
(`eof_action`, `shortest`, `repeatlast`, `ts_sync_mode`) is a documented user-facing surface that must be
identical on all 68.

## 3.1 Model

```rust
pub struct FrameSyncOpts {
    pub eof_action: EofAction,      // repeat | endall | pass
    pub shortest:   bool,           // force termination when any input ends
    pub repeatlast: bool,           // hold the last secondary frame after its EOF
    pub ts_sync:    TsSyncMode,     // default | nearest
}

pub enum EofAction { Repeat, EndAll, Pass }
pub enum TsSyncMode { Default, Nearest }

/// Per-input behaviour outside its own frame range.
pub enum ExtendMode {
    Stop,       // no event may occur here; the event stream ends
    Null,       // no frame available; the callback sees None for this input
    Infinity,   // hold the first (before) / last (after) frame forever
}

pub struct FsInput {
    pub before: ExtendMode,     // behaviour before this input's first frame
    pub after:  ExtendMode,     // behaviour after this input's EOF
    /// 0 = passive (sampled, never advances the clock).
    /// The highest value present becomes the graph's sync_level; only inputs at
    /// that level generate events. overlay: main=2, overlay=1 -> only main drives.
    pub sync: u32,
}
```

## 3.2 The event loop

```
configure(inputs):
    time_base = fold over inputs: gcd_q(acc, in.time_base, max_den = 1_000_000)
                fallback 1/1_000_000 if the reduction would overflow
    sync_level = max(in.sync for in in inputs)
    latency    = 1 frame if ts_sync == Nearest else 0     # Nearest must look one frame ahead

step():
    # 1. can we determine the next event time?
    for in in inputs where in.sync == sync_level:
        if in has no frame and not at EOF:
            request(in); return Pending            # backpressure: pull the input that is behind
    candidates = [ in.head_pts for in in inputs if in.sync == sync_level and in.has_frame ]
    if candidates is empty:
        return Eof                                 # every driving input is exhausted
    pts = min(candidates)                          # rescaled into the common time_base

    # 2. sample every input at `pts`
    for in in inputs:
        frame[i] = match in.state:
            BeforeFirst           -> apply in.before   # Stop -> abort the event; Null -> None;
                                                       # Infinity -> peek the first frame
            Running               -> match ts_sync:
                Default  -> the newest frame with frame.pts <= pts
                Nearest  -> the frame minimising |frame.pts - pts|, needing one frame of lookahead
            AfterEof              -> apply in.after
    if any input applied ExtendMode::Stop:
        set output status Eof at `pts`; return Eof

    # 3. fire
    on_event(ctx, FrameSyncEvent { pts, frames })

    # 4. consume
    for in in inputs where in.sync == sync_level and in.head_pts == pts:
        drop in.head
    # non-driving inputs advance only past frames strictly older than the next event time
```

## 3.3 Option → mode mapping

The user-facing options are sugar over `ExtendMode`, and getting this table right is the whole
compatibility surface:

| User setting | Effect |
|---|---|
| `eof_action=repeat` (default) | every input: `after = Infinity` — hold the last frame |
| `eof_action=endall` | every input: `after = Stop` — first EOF ends the graph output |
| `eof_action=pass` | the `sync_level` input: `after = Stop`; all others: `after = Null` — the main stream continues untouched once a secondary ends |
| `shortest=1` | overrides the above with `after = Stop` on **every** input |
| `repeatlast=0` | non-driving inputs: `after = Null` instead of `Infinity` |
| — (always) | every input: `before = Infinity` when it has produced at least one frame; `Stop` on an input that reaches EOF having produced none |
| `ts_sync_mode=default` | most recent frame with `pts <= event_pts` |
| `ts_sync_mode=nearest` | frame nearest `event_pts`, one frame of added latency |

`shortest` and `eof_action` interact (upstream: `shortest` wins), and `repeatlast=0` is nearly but not
exactly `eof_action=pass`. These three cases have caused real user confusion upstream, so they get an
explicit truth table in `docs/filter/framesync.md` and one differential test each.

## 3.4 The adapter

As with `Simple`, no filter implements the loop:

```rust
pub trait FrameSyncFilter: Send {
    fn on_event(&mut self, ctx: &mut FrameCtx<'_>, ev: &mut FrameSyncEvent<'_>) -> Result<()>;
    /// Declare per-input roles at configure time; default: input 0 sync=2, the rest sync=1.
    fn inputs(&self, n: usize) -> Vec<FsInput> { .. }
}

impl FrameSyncEvent<'_> {
    pub fn pts(&self) -> i64;
    pub fn get(&self, i: usize) -> Option<&Frame>;
    /// Take ownership; CoW-writable. Taking input 0 is the usual "modify the main frame" path.
    pub fn take(&mut self, i: usize) -> Option<Frame>;
}

pub struct Synced<F: FrameSyncFilter> { .. }
impl<F: FrameSyncFilter> Filter for Synced<F> { .. }   // written once
```

A filter like `overlay` therefore writes `on_event` plus its blending kernel, and gets the entire
`eof_action`/`shortest`/`repeatlast`/`ts_sync_mode` surface for free and identically to the other 67.

## 3.5 Latency and diagnostics

`FrameSync` reports its own buffered depth and configured latency to `LinkStats`, so `latency`/`alatency`
and `graphmonitor` show real numbers, and so the deadlock classifier (§1.9) knows that a framesync node
holding frames while requesting an input is working, not stuck.

---

# 4. Filter crate decomposition

Driven by the shared-helper dependency map in the research document: a helper used by filters that would
otherwise land in different crates forces that helper into a lower crate that both depend on. That
constraint, not category tidiness, is what produces the layering below.

## 4.1 Infrastructure (layer 5a) — six crates

| Crate | Contents | Depends on |
|---|---|---|
| `vaco-filter-core` | `Filter` trait, adapters (`Simple`, `SliceFilter`, `AudioFilter`, `SourceFilter`), pad/link model, frame queues + audio FIFO, negotiation engine, scheduler, timeline, commands, capability flags, buffer sources/sinks, diagnostics renderer | `vaco-core`, `-frame`, `-pixfmt`, `-sampfmt`, `-chlayout`, `-color`, `-opts`, `-expr`, `-pool`, `rayon` |
| `vaco-filter-graph` | DSL parser, AST, instantiation, link resolution, validation, converter policy, graph dump/dot | `-core`, `vaco-registry` |
| `vaco-filter-framesync` | §3 | `-core` |
| `vaco-filter-draw` | `drawutils` equivalent: format-aware colour parsing (`#rrggbbaa`, named colours, `@0.5` alpha suffix), plane-correct fill/blend/box/line, chroma-siting-aware subsampled blending, alpha compositing, 8–16-bit and packed/planar paths | `-core` |
| `vaco-filter-vdsp` | shared video kernels that cross crate boundaries: `scene_sad`, `edge_common`, box-blur core, `motion_estimation`, affine `transform`, `bbox`, SAD/hadamard (`pixelutils`), LUT sampling/interpolation (1D/3D/Hald/tetrahedral), morphology neighbourhood core, integral images | `-core`, `vaco-simd` |
| `vaco-filter-adsp` | shared audio kernels: `generate_wave_table` (LFOs), biquad coefficient design, window functions, EBU R128 loudness core, partitioned FIR convolution, WSOLA/phase-vocoder core | `-core`, `vaco-tx`, `vaco-simd` |

**Why `vdsp` and `adsp` exist as separate crates.** From the research dependency map: `scene_sad` is used by
`framerate` (temporal), `freezedetect` (analysis), `identity`/`msad` (analysis), `minterpolate` (motion),
`scdet` (analysis) and `select` (multimedia) — six filters that belong in five different category crates.
Same for `edge_common` (`edgedetect` → blur, `cropdetect` → analysis, `blurdetect` → analysis), the box-blur
core (`boxblur` → blur, `avgblur` → blur, but also the GPU path), `transform` (`deshake` → motion) and
`motion_estimation` (`mestimate`, `minterpolate`). Hoisting them is forced, not stylistic.

`vaco-filter-text` is listed under §6 because its dependency set is different in kind.

## 4.2 Video filter crates

| Crate | Filters | Extra deps | Tier |
|---|---|---|---|
| `vaco-filter-scale` | scale, scale2ref, format, noformat, setrange, setparams, setfield, setsar, setdar, colorspace, colordetect, pixdesctest, zoompan | `vaco-scale`, framesync (scale2ref) | T1 |
| `vaco-filter-geometry` | crop, pad, rotate, transpose, hflip, vflip, shear, scroll, il, field, tile, untile, framepack, fillborders, swaprect, swapuv, shuffleframes, shufflepixels, shuffleplanes, extractplanes, alphaextract, alphamerge, mergeplanes, addroi, ccrepack, lenscorrection, perspective, stereo3d, tiltandshift | draw, framesync | T1/T2 |
| `vaco-filter-v360` | v360 | vdsp | T3 |
| `vaco-filter-temporal` | fps, framestep, tpad, tmix, tblend, tmedian, tlut2, tmidequalizer, decimate, mpdecimate, deflicker, lagfun, freezedetect, freezeframes, dejudder, fsync, random | vdsp (scene_sad), framesync | T1/T2 |
| `vaco-filter-deinterlace` | yadif, bwdif, w3fdif, estdif, separatefields, weave, doubleweave, fieldorder, fieldmatch, fieldhint, detelecine, telecine, idet, vfrdet, interlace, tinterlace, kerndeint, pullup, repeatfields, phase | vdsp | T2/T3 |
| `vaco-filter-denoise` | atadenoise, nlmeans, bm3d, dctdnoiz, fftdnoiz, bilateral, deband, deblock, removegrain, chromanr, dedot, gradfun, hqdn3d, owdenoise, vaguedenoiser, spp, fspp, uspp, pp7 | vdsp, `vaco-tx`, framesync (bm3d) | T3 |
| `vaco-filter-blur` | unsharp, cas, avgblur, gblur, dblur, varblur, yaepblur, guided, boxblur, smartblur, sab | vdsp (boxblur core), framesync (varblur, guided) | T2 |
| `vaco-filter-convolve` | convolution, morpho, erosion, dilation, inflate, deflate, median, sobel, prewitt, roberts, scharr, kirsch, edgedetect, blurdetect, convolve, deconvolve, corr, xcorrelate | vdsp (edge_common, morphology), `vaco-tx`, framesync | T2/T3 |
| `vaco-filter-color` | curves, colorbalance, colorchannelmixer, colorcontrast, colorcorrect, colorize, colorlevels, colortemperature, huesaturation, hue, vibrance, exposure, selectivecolor, grayworld, greyedge, normalize, monochrome, midequalizer, lut, lutrgb, lutyuv, lut2, geq, pseudocolor, colormap, limitdiff, tonemap, eq, histeq, colormatrix | `vaco-color`, `vaco-expr`, framesync | T2 |
| `vaco-filter-lut` | lut1d, lut3d, haldclut, haldclutsrc | vdsp (LUT sampling), `.cube`/`.3dl`/`.dat`/`.m3d` parsers, framesync | T2 |
| `vaco-filter-key` | chromakey, chromahold, colorkey, colorhold, hsvkey, hsvhold, lumakey, backgroundkey, despill, premultiply, unpremultiply, premultiply_dynamic, maskedmerge, maskedclamp, maskedmax, maskedmin, maskedthreshold, maskfun, threshold, hysteresis, floodfill | framesync | T2/T3 |
| `vaco-filter-overlay` | overlay, blend, xfade, mix, multiply, xmedian, displace, remap, feedback | draw, framesync | T1/T2 |
| `vaco-filter-stack` | hstack, vstack, xstack | framesync, shared layout parser | T2 |
| `vaco-filter-motion` | mestimate, minterpolate, framerate, deshake, vidstab-equivalent (`stabdetect`/`stabtransform`) | vdsp (motion_estimation, transform, scene_sad) | T3 |
| `vaco-filter-analysis` | psnr, ssim, ssim360, xpsnr, vif, vmafmotion, msad, identity, blackdetect, blockdetect, bitplanenoise, entropy, siti, signalstats, readeia608, readvitc, showinfo, photosensitivity, scdet, bbox, codecview, blackframe, cropdetect, signature | vdsp, framesync | T2/T3 |
| `vaco-filter-scope` | histogram, thistogram, waveform, vectorscope, oscilloscope, datascope, pixscope, ciescope, graphmonitor, agraphmonitor, drawgraph, adrawgraph | draw, `vaco-color` | T3 |
| `vaco-filter-source` | nullsrc, color, colorchart, colorspectrum, testsrc, testsrc2, rgbtestsrc, yuvtestsrc, smptebars, smptehdbars, pal75bars, pal100bars, allrgb, allyuv, gradients, cellauto, life, mandelbrot, sierpinski, zoneplate, perlin, nullsink | draw | T1/T3 |
| `vaco-filter-artistic` | noise, vignette, pixelize, epx, xbr, hqx, super2xsai, amplify, delogo, removelogo, cover_rect, find_rect | vdsp | T3 |
| `vaco-filter-palette` | palettegen, paletteuse, latticepal, elbg | framesync (paletteuse) | T2 |
| `vaco-filter-draw-vf` | drawbox, drawgrid, qrcode, qrcodesrc, quirc-equivalent (`qrdecode`) | draw, `qrcode` crate | T2/T3 |

## 4.3 Audio filter crates

| Crate | Filters | Extra deps | Tier |
|---|---|---|---|
| `vaco-filter-aformat` | aformat, aresample, asetrate, asettb, asetnsamples, anull, acopy, channelmap, channelsplit, join, pan | `vaco-resample`, `vaco-chlayout` | T1 |
| `vaco-filter-amix` | amix, amerge, amultiply, adecorrelate | | T1 |
| `vaco-filter-aeq` | the biquad family (equalizer, bandpass, bandreject, allpass, bass/lowshelf, treble/highshelf, tiltshelf, highpass, lowpass, biquad), firequalizer, superequalizer, anequalizer, aiir, afir, atilt, asubboost, asubcut, asupercut, asuperpass, asuperstop, afreqshift, aphaseshift, aemphasis | adsp, `vaco-tx` | T2 |
| `vaco-filter-adynamics` | acompressor, sidechaincompress, agate, sidechaingate, alimiter, acrusher, adrc, dynaudnorm, compand, mcompand, loudnorm, speechnorm, apsyclip, asoftclip, adynamicequalizer, adynamicsmooth, volume, volumedetect | adsp (ebur128) | T1/T2 |
| `vaco-filter-aeffects` | aecho, adelay, compensationdelay, chorus, flanger, aphaser, tremolo, vibrato, apulsator, crystalizer, aexciter, deesser, dialoguenhance, crossfeed, stereotools, stereowiden, extrastereo, earwax, haas, surround, headphone, virtualbass, dcshift, atempo, hdcd | adsp (wave tables, WSOLA), `vaco-tx` | T2/T3 |
| `vaco-filter-adenoise` | afftdn, afwtdn, anlmdn, anlms, arnndn, adenorm | `vaco-tx` | T3 |
| `vaco-filter-aanalysis` | astats, aspectralstats, ebur128, drmeter, silencedetect, replaygain, apsnr, asdr, asisdr, axcorrelate, aderivative, aintegral, ashowinfo, aphasemeter | adsp, `vaco-tx` | T2/T3 |
| `vaco-filter-asource` | anullsrc, sine, anoisesrc, aevalsrc, afirsrc, afireqsrc, afdelaysrc, sinc, hilbert, anullsink | `vaco-expr`, `vaco-tx` | T1/T3 |

## 4.4 Multimedia crates

| Crate | Filters | Extra deps | Tier |
|---|---|---|---|
| `vaco-filter-mm` | concat, interleave, ainterleave, select, aselect, segment, asegment, streamselect, astreamselect, trim, atrim, loop, aloop, reverse, areverse, cue, acue, realtime, arealtime, latency, alatency, bench, abench, perms, aperms, metadata, ametadata, sidedata, asidedata, sendcmd, asendcmd, cmdsocket, acmdsocket, split, asplit, setpts, asetpts, null, anull, aeval, avsynctest | `vaco-expr`, vdsp (scene_sad for `select`), framesync (streamselect) | T1 |
| `vaco-filter-movie` | movie, amovie | `vaco-format-core`, `vaco-codec-core`, `vaco-registry` — a **layer-6 consumer inside layer 5**, so it lives above the registry and is wired in by `vaco-cli-core` | T2 |
| `vaco-filter-avvis` | showspectrum, showspectrumpic, showcqt, showcwt, showfreqs, showspatial, showvolume, showwaves, showwavespic, avectorscope, a3dscope, abitscope, ahistogram, spectrumsynth | draw, `vaco-tx` | T3 |

`vaco-filter-movie` is the one genuine layering wrinkle: a filter that opens and decodes a file needs the
demuxer and decoder registries, which sit at layer 6, above filters. Resolution: `vaco-filter-core` defines
a `MediaOpener` trait; `vaco-filter-movie` depends only on that trait; `vaco-cli-core` (layer 7) injects the
concrete registry-backed implementation into the graph at construction. The layer-check script gets one
reviewed exception entry, not a cycle.

## 4.5 GPU and hardware

| Crate | Contents | Tier |
|---|---|---|
| `vaco-filter-gpu` | wgpu device/queue management, `GpuFrame`, WGSL compute kernels (§7), `hwupload`/`hwdownload`/`hwmap` for the wgpu device | T3 (T2 for `vaco-play`) |
| `vaco-hw-*` | the D2-allowlisted FFI crates; supply zero-copy interop only (§7.4) | opt-in |

**Count.** 6 infrastructure + 21 video + 8 audio + 3 multimedia + 1 GPU + 1 text + 1 subtitle = **41 crates**,
inside the 120–160 workspace budget from arch §1.7. Each lands with its `docs/filter/<crate>.md` entry, per
the repository documentation standard.

---

# 5. Tiering

## 5.1 Definitions

| Tier | Definition |
|---|---|
| **T1** | Without these, `vaco` is not a usable transcoder. Every one is on the critical path for the first end-to-end milestone. |
| **T2** | Commonly used; their absence is a visible functionality gap against ffmpeg for ordinary work. |
| **T3** | The long tail. Individually low-frequency, collectively most of the inventory. Implemented opportunistically and in parallel; each is small and independent. |
| **T4** | Cannot ship in the default build as designed: it needs an external library that fails a D10 gate, it is a per-vendor hardware duplicate, or it is GPL-upstream *and* has no published algorithm we can work from. Every T4 entry carries a stated alternative. |

## 5.2 Counts

| Tier | Count | Share | Rationale |
|---|---:|---:|---|
| T1 | 48 | 8.6% | The set named in the brief plus the sources/sinks and metadata filters they cannot function without. |
| T2 | 122 | 21.8% | Colour, deinterlace, text, blur/sharpen, geometry, audio EQ/dynamics, quality metrics, palette/GIF. |
| T3 | 214 | 38.2% | Everything else implementable in pure safe Rust from public descriptions. |
| T4 | 176 | 31.4% | 87 hardware-backend duplicates + ~40 external-library-gated + ~12 GPL-without-spec + ~37 deprecated, test-only, or superseded. |
| **Total** | **560** | | |

The T4 share looks alarming until it is decomposed: **half of it is 87 per-vendor hardware duplicates of
~20 distinct operations**, which §7 replaces with ~16 WGSL kernels in one crate. Netting that out, the real
"cannot do" set is ~89 filters, 16% of the inventory, and most of those are single-purpose wrappers around
a research library.

## 5.3 T1 — the 48

**Video core (13):** `scale`, `format`, `noformat`, `crop`, `pad`, `overlay`, `fps`, `transpose`, `hflip`,
`vflip`, `setsar`, `setdar`, `setparams`.
**Audio core (11):** `aresample`, `aformat`, `volume`, `amix`, `amerge`, `channelmap`, `channelsplit`,
`join`, `pan`, `asetnsamples`, `asetrate`.
**Graph plumbing (12):** `split`, `asplit`, `null`, `anull`, `copy`, `acopy`, `setpts`, `asetpts`, `settb`,
`asettb`, `select`, `aselect`.
**Cutting and joining (3):** `trim`, `atrim`, `concat`.
**Sources and sinks (9):** `buffer`, `abuffer`, `buffersink`, `abuffersink`, `nullsrc`, `anullsrc`,
`nullsink`, `anullsink`, `color`.

Rationale for the additions beyond the brief's list: `setsar`/`setdar`/`setparams` are required for correct
anamorphic and colour-tagged remuxes and are trivially cheap; `select`/`aselect` are the only way to express
frame-accurate stream editing and are used by the differential harness itself; `copy`/`acopy` and `settb`
are needed by the auto-inserted graph machinery and by `-vf` no-op cases; `color` is the source every
`overlay` and `pad` test fixture is built from.

## 5.4 GPL-upstream filters — published-algorithm assessment

Required by the brief. The question for each is not "may we read the source" (never), but "does a public
description exist that is precise enough to implement from?"

### Accessible — implement from published sources, normal tier

| Filter | Public basis | Tier | Fidelity expectation |
|---|---|---|---|
| `boxblur` | Separable moving-average; any image-processing text. | T2 | Bit-exact achievable. |
| `eq` | The brightness/contrast/gamma/saturation formulae are stated in the filter documentation, which is an interface fact. | T2 | Bit-exact achievable. |
| `blackframe` | Count pixels below a threshold; behaviour fully documented. | T3 | Exact. |
| `cropdetect` | Threshold + scan for non-black rows/columns; documented, including the `round`/`reset` semantics. | T2 | Exact for the common case; edge heuristics need differential tuning. |
| `mpdecimate` | Documented as 8×8-block SAD against `hi`/`lo`/`frac` thresholds. | T3 | Exact. |
| `repeatfields` | Honour the MPEG `repeat_first_field` flag. Trivial. | T3 | Exact. |
| `stereo3d` | Mechanical layout conversion; the layout set is the option list. | T3 | Exact. |
| `colormatrix` | BT.601/BT.709/FCC/SMPTE-240M matrices are in ITU-T H.273. | T3 | Exact — and `colorspace` already supersedes it. |
| `interlace` / `tinterlace` | Line interleaving with an optional vertical low-pass; the "complex" mode is a standard BBC-style vertical filter, published. | T2 | Close; coefficient choice needs differential checking. |
| `phase` | Documented auto field-phase heuristic. | T3 | Close. |
| `perspective` | 3×3 homography + interpolation; standard computer vision. | T3 | Close (interpolation rounding). |
| `histeq` | Textbook histogram equalisation; the local/global modes are documented, and the antecedent (VirtualDub HistEq) is publicly described. | T3 | Close. |
| `smartblur` | Threshold-gated difference between a Gaussian-blurred copy and the original; documented at formula level. | T3 | Close. |
| `owdenoise` | **Coifman & Donoho, "Translation-Invariant De-Noising" (1995)** — the overcomplete/cycle-spinning wavelet method, fully published. | T3 | Algorithmically faithful; not bit-exact. |
| `vaguedenoiser` | Wavelet shrinkage; Donoho–Johnstone soft/hard/garrote thresholding, published, and the threshold modes are the documented option set. | T3 | Algorithmically faithful. |
| `spp` / `fspp` / `uspp` | Overlapped-block DCT thresholding — **Nosratinia, "Enhancement of JPEG-compressed images by re-application of JPEG" (2001)**, the published basis for the method. | T3 | Algorithmically faithful; `fspp`'s speed approximations are authorial and we substitute our own. |
| `signature` | **ISO/IEC 15938-3 Amd.4, MPEG-7 Video Signature Tools** — a full published standard. Strongest case on the list. | T3 | Standard-conformant, therefore interoperable. |
| `cover_rect` / `find_rect` | Template matching by SAD / normalised cross-correlation; textbook. Upstream's use of the codec ME engine is an implementation choice, not the algorithm. | T3 | Functionally equivalent. |
| `kerndeint` | Donald Graft's Kernel Deinterlacer; the kernel coefficients and the threshold logic are published in the original filter's own documentation. | T3 | Close. |
| `super2xsai` | Super2xSaI (Derek Liauw Kie Fa); the pixel-neighbourhood rules are publicly described in the emulator-scaler literature. | T3 | Close. `xbr`/`hqx` (not GPL upstream) already cover the use case. |
| `hqdn3d` | The structure — separable spatial low-pass plus a temporal IIR, both gated by a non-linear difference curve — is described in the documentation and in general denoising literature. The exact curve shape is authorial. | T3 | **Not bit-exact.** We derive our own curve and document the divergence. |
| `delogo` | Documented as interpolation of the box interior from its border pixels. | T3 | Close; exact interpolation weights are ours. |
| `pp7` | DCT-domain deblocking; the method is the classic post-filter described in the H.263 Annex J / MPEG-4 post-processing literature. | T3 | Algorithmically faithful. |

### Not accessible — T4, with alternatives

| Filter | Why | What we ship instead |
|---|---|---|
| `nnedi` | The architecture is describable, but the filter's value is entirely in its **trained weight file**, which is an authorial artifact we may not copy and cannot derive. | `estdif` or `bwdif` for deinterlacing; for upscaling, a wgpu compute kernel with weights we train ourselves — a real project, deferred. |
| `mcdeint` | Motion-compensating deinterlace defined by its use of a specific encoder's ME; no formal description of the heuristics. | `bwdif`, `estdif`, or `minterpolate`-assisted deinterlace. |
| `pullup` | The IVTC field-matching heuristic is not published in usable detail. | `fieldmatch` + `decimate` (not GPL upstream), which is the recommended modern chain anyway; and `detelecine` for known patterns. |
| `sab` | Shape-adaptive blur described only at parameter level. | `yaepblur`, `guided`, or `bilateral` — all non-GPL and better. |
| `mptestsrc` | The pattern set is authorial content, not an algorithm. | `testsrc2`, `zoneplate`, `smptehdbars`. |

## 5.5 T4 — external-library filters under D10 Gate 1

Gate 1 (zero FFI) removes every one of these as a *binding*. For each: does a pure-Rust crate clear all
three gates, do we implement it, or is it out of scope?

| Upstream filter | Upstream library | Verdict |
|---|---|---|
| `subtitles`, `ass` | libass (C) | **Implement, with a pure-Rust crate assessed as a base.** `ass-core` (MIT, claims 100% safe Rust) clears Gates 1 and 2 but fails Gate 3 today on adoption and maturity, and carries the clean-room verification task in register §9.6. Plan: our own `vaco-ass` on the cosmic-text stack, using `ass-core` as a reference point only if its provenance is verified. See §6.3 for the honest fidelity assessment. |
| `zscale` | libzimg | **Superseded.** `vaco-scale` (arch §3) is ours and covers resize + colourspace + depth. We register `zscale` as an alias with a deprecation notice mapping its options onto `scale`+`colorspace`. |
| `libvmaf`, `libvmaf_cuda` | libvmaf | **Implement — large.** The VMAF feature extractors (VIF, ADM/DLM, motion) and the SVM model format are published by Netflix, and the model files themselves are BSD-licensed data. But this is a research-grade metric: a faithful implementation is ~8–12 person-weeks and validating it against published scores is most of that. T3, scheduled late. `vif`, `vmafmotion`, `psnr`, `ssim`, `xpsnr` are ours and land far earlier. |
| `librubberband` | librubberband (GPL anyway) | **Implement.** A phase-vocoder + WSOLA time/pitch engine in `vaco-filter-adsp`, exposed as `atempo` (already ours) plus a new `astretch`. Quality parity with Rubber Band is a genuine DSP project; we ship a good-not-best implementation and say so. |
| `bs2b` | libbs2b | **Implement.** Bauer stereo-to-binaural is a small documented crossfeed filter (two shelving filters plus a delay); ~200 lines. T3. |
| `sofalizer` | libmysofa | **Implement.** SOFA is **AES69**, a published standard, stored in NetCDF-4/HDF5. The HDF5 reader is the work, not the convolution. A pure-Rust `hdf5` reader that clears Gate 3 does not exist; we would write a narrow AES69-subset reader. T3, medium. `headphone` (our own HRTF convolution from plain coefficient files) covers most of the need first. |
| `ladspa`, `lv2` | plugin hosting via `dlopen` | **Out of scope.** Hosting a native plugin is FFI by definition; there is no pure-Rust form of it. If ever wanted, it belongs in an opt-in crate outside the default build, and D9's "prefer `exec`-ing the user's binary" note points at the better answer. |
| `frei0r`, `frei0r_src` | frei0r plugin ABI | **Out of scope.** Same reason. |
| `ocv` | libopencv | **Out of scope.** Its filters (smooth/erode/dilate) are already covered natively by `convolution`/`erosion`/`dilation`/`morpho`. |
| `ocr` | libtesseract | **Out of scope.** No pure-Rust OCR clears Gate 3. Document the gap. |
| `asr` | pocketsphinx | **Out of scope.** Superseded by Whisper-class models anyway. |
| `whisper` | whisper.cpp | **Deferred, feasible.** `candle` (MIT/Apache, pure Rust) has a working Whisper implementation and clears Gates 1–2; Gate 3 needs a real assessment of its dependency depth. T3+, behind a non-default feature because of model-weight distribution. |
| `dnn_processing`, `dnn_detect`, `dnn_classify`, `sr`, `derain` | ONNX Runtime / OpenVINO / TensorFlow / libtorch | **One backend, ours-adjacent.** All four upstream backends are FFI. `tract` (Apache-2.0/MIT, pure-Rust ONNX inference) is the only candidate that clears Gate 1; it needs a Gate 3 assessment (tree depth, unsafe count). Plan: a single `vaco-filter-dnn` crate behind a non-default feature exposing `dnn_processing`/`dnn_detect`/`dnn_classify` over ONNX only. `sr` and `derain` become presets of `dnn_processing`, as upstream already recommends. |
| `iccdetect`, `iccgen` | lcms2 | **Implement, narrow.** We need ICC v2/v4 *parsing* and matrix/TRC extraction, not a full CMM. ~1–2 person-weeks in `vaco-color`. T3. |
| `ocio` | OpenColorIO | **Out of scope.** The OCIO config format is large and its value is ecosystem compatibility, which a partial implementation does not deliver. `lut3d` + `colorspace` + `tonemap` cover the common transforms. |
| `lensfun` | liblensfun | **Partially implement.** The lens database is the asset, and it is CC-BY-SA/GPL — we cannot ship it. `lenscorrection` (ours, explicit coefficients) stays; `lensfun` is out of scope with that noted. |
| `libplacebo` | libplacebo + Vulkan | **Superseded by `vaco-filter-gpu`.** Its scale/tonemap/colour-management pipeline is exactly what §7's WGSL kernels provide, portably. Not an alias — the option surfaces differ too much to fake. |
| `vidstabdetect`, `vidstabtransform` | libvidstab (GPL) | **Implement.** Two-pass stabilisation: pass 1 is feature-point motion estimation (we have `motion_estimation` and `transform` in `vdsp`), pass 2 is trajectory smoothing plus an affine warp. The transform-file format is ours; we register `stabdetect`/`stabtransform` and do **not** claim `.trf` compatibility. T3. |
| `qrencodesrc`, `qrencode` | libqrencode | **Use a crate.** `qrcode` (MIT/Apache, pure Rust) clears all three gates. T3, easy. |
| `quirc` | libquirc | **Use a crate.** `rqrr` (MIT/Apache) clears Gates 1–2; Gate 3 assessment needed. Registered as `qrdecode`. |
| `drawvg` | cairo | **Implement on a Rust rasteriser.** `tiny-skia` (BSD-3-Clause, pure Rust, a Skia-subset port) clears Gates 1–2 and is well adopted; Gate 3 favourable. T3. |
| `lcevc` | liblcevc_dec | **Out of scope.** MPEG-5 LCEVC is patent-encumbered and the only implementation is proprietary. |
| `flite` | libflite | **Out of scope.** Speech synthesis is not in this project's remit. |
| `coreimage`, `coreimagesrc` | Apple CoreImage | **Out of scope** (FFI, platform-specific). `vaco-filter-gpu` covers the same ground portably. |
| `elbg` | avcodec's ELBG | **Implement.** ELBG vector quantisation is published (Patané & Russo, 2001) and we need it for `palettegen` anyway. T3. |
| `zmq`, `azmq` | libzmq | **Out of scope**, replaced by `cmdsocket` (§1.10.2). |

## 5.6 T4 — hardware-backend duplicates

87 filter names across VAAPI (13), OpenCL (21), Vulkan (19), CUDA (12), QSV (7), D3D11/12 (6), VideoToolbox
(3), AMF (6). They implement roughly 20 distinct operations. All are FFI and all fail Gate 1 as bindings.

**They are not 87 pieces of work for us.** §7 replaces the whole set with ~16 WGSL compute kernels in
`vaco-filter-gpu`, reachable on Vulkan, Metal, D3D12 and GL through one safe API. We register the *generic*
names (`scale`, `overlay`, `transpose`, `gblur`, `tonemap`, …) as GPU-capable rather than registering
`scale_vaapi` and friends; a graph naming `scale_vaapi` gets an error naming `hwupload,scale,hwdownload`
as the replacement. This is a deliberate CLI-compatibility divergence and belongs in the differential
harness allowlist.

## 5.7 Deinterlace GPL status — checked

The brief asks specifically. Per the research inventory, `yadif`, `bwdif`, `w3fdif`, `estdif`,
`fieldmatch`, `decimate`, `detelecine`, `telecine`, `idet` and `separatefields`/`weave` are **not**
GPL-gated upstream. The GPL deinterlacers are `kerndeint`, `nnedi`, `mcdeint`, `pullup`, `repeatfields`,
`phase`, and `interlace`/`tinterlace`.

That distinction changes nothing about how we write them — D7 forbids porting either way — but it does
change where the *specification* comes from, which is the real constraint:

- **`yadif`**: the temporal/spatial predictor with a spatial-interpolation check is described in public
  write-ups and in the VapourSynth/AviSynth documentation of its many reimplementations. T2, tractable,
  and the highest-value deinterlacer to get right first.
- **`bwdif`**: `yadif`'s decision logic combined with **Martin Weston's three-field interpolation, published
  by BBC R&D** (and in the corresponding patent family, which is expired — a published patent is a
  specification we may read). T2.
- **`w3fdif`**: the BBC R&D white paper gives the filter coefficients directly. T2, the easiest of the three.
- **`estdif`**: edge-slope tracing; the documented option set (`interp`, `rslope`, `redge`, `ecost`,
  `mcost`, `dcost`) describes the algorithm closely enough. T3.

---

# 6. Text and subtitle rendering

FFmpeg's text stack is libfreetype + libharfbuzz + libfontconfig + libfribidi, plus libass for ASS/SSA.
All five are C, so all five fail D10 Gate 1. The register's recommendation stands and is explicitly
confirmed unaffected by D10: `cosmic-text` + `swash` + `ttf-parser` + `fontdue` are general typography
crates, permissive, pure Rust, well adopted, and they replace four of the five outright.

## 6.1 `vaco-filter-text` — the stack

| Upstream role | Ours | Notes |
|---|---|---|
| libfontconfig — font discovery and matching | `fontdb` (via cosmic-text) + our own alias table | See the divergence note below. |
| libfribidi — Unicode bidi | `unicode-bidi` (via cosmic-text) | Implements UAX #9. |
| libharfbuzz — shaping | `rustybuzz` (via cosmic-text) | Register open task §9.7 — confirm the port is a rewrite, not a near-verbatim translation carrying Old-MIT obligations. Resolve before adoption. |
| libfreetype — outline parsing and rasterisation | `ttf-parser` + `swash` | `fontdue` is a lighter alternative for the simple path; we standardise on swash for subpixel positioning and colour-font support. |
| — | `vaco-filter-draw` | Composites the alpha mask into the frame's real pixel format. |

**The FreeType attribution obligation disappears.** Under the FTL, binary distribution requires a documented
notice that the software is based in part on the work of the FreeType Team. Not carrying FreeType removes a
standing, per-release obligation — a small but genuine operational win, and it is the reason not to reach for
`freetype-sys` even if Gate 1 were relaxed (D9 also flags `freetype-sys` as declaring MIT over FTL/GPL-2,
exactly the metadata lie that gate exists to stop).

**Architecture.** `vaco-filter-text` exposes a `TextLayout` API — not a filter — so that `drawtext`,
`subtitles`, `drawgraph`, `datascope`, `pixscope` and the scope filters all render glyphs the same way:

```rust
pub struct TextRenderer { fonts: FontDb, shape_cache: LruCache<ShapeKey, ShapedRun>, glyph_cache: GlyphCache }
impl TextRenderer {
    pub fn layout(&mut self, text: &str, style: &TextStyle, wrap: Wrap) -> Layout;
    /// Rasterise to an 8-bit coverage mask plus a bounding box. The mask is then
    /// composited by vaco-filter-draw in the destination pixel format.
    pub fn rasterise(&mut self, layout: &Layout, transform: Affine) -> AlphaMask;
}
```

Glyph caching is not optional: `drawtext` with `%{pts}` re-lays out every frame, and a 1080p60 burn-in
without a shaped-run cache and a rasterised-glyph cache will not run in real time. libass caches
aggressively for the same reason.

## 6.2 `drawtext` compatibility surface

The option set is an interface fact and we reproduce it: `text`, `textfile`, `text_source`, `fontfile`,
`font`, `fontsize`/`fontsize_expr`, `fontcolor`/`fontcolor_expr`, `alpha`, `box`, `boxcolor`, `boxborderw`,
`borderw`/`bordercolor`, `shadowx`/`shadowy`/`shadowcolor`, `x`/`y` (expressions), `line_spacing`,
`text_align`, `y_align`, `tabsize`, `fix_bounds`, `expansion`, `reload`, `start_number`, `rtl`.

Text expansion (`expansion=normal`) reproduces the documented directive set: `%{pts[:fmt[:offset]]}`,
`%{expr:...}`, `%{eif:expr:fmt[:width]}`, `%{frame_num}`/`%{n}`, `%{gmtime}`, `%{localtime}`,
`%{metadata:key[:default]}`, `%{pict_type}`, and `%{expr_int_format}`. Position expressions get the
documented variables (`w`, `h`, `text_w`/`tw`, `text_h`/`th`, `line_h`/`lh`, `main_w`/`main_h`, `x`, `y`,
`n`, `t`, `line_ascent`, `line_descent`, `max_glyph_a/d/w/h`, `rand(min,max)`), evaluated by `vaco-expr`.

**Two deliberate divergences, both documented and allowlisted in the differential harness:**

1. **`ft_load_flags`** is FreeType-specific. We accept the option, map what has meaning (`no_hinting`,
   `monochrome`, `render`), and warn once on the rest rather than failing — scripts pass it reflexively.
2. **Font-name resolution differs.** fontconfig resolves `font=Sans` through system configuration,
   per-language preferences and user config in `~/.config/fontconfig`. `fontdb` scans font directories and
   matches by family name. We ship a built-in alias table (`sans-serif`/`Sans`, `serif`, `monospace`,
   `cursive`, `fantasy` → a platform-appropriate ordered fallback list) plus a `-font_dirs` option, and we
   accept that `font=Sans` may select a different face than on an fontconfig system. `fontfile=` is exact
   on both and is what tests use.

## 6.3 Subtitle rendering — `vaco-filter-subtitle`

Two filters: `subtitles` (demux + decode a subtitle track or file, then render) and `ass` (render an
ASS/SSA file directly). Both need `vaco-filter-movie`'s `MediaOpener` for the demux half.

**Bitmap subtitles reach parity immediately.** DVD (VOBSUB), PGS/HDMV, DVB and XSUB are decoded to RGBA
bitmaps with a position and a palette; rendering is a composite. There is no typesetting problem, no font
problem, and no libass involvement upstream either. This is a week of work and it covers a large share of
real usage.

**Text subtitles split by complexity.** SRT, WebVTT, MicroDVD, SAMI and plain SubStation with no override
tags are a layout-and-draw job that the §6.1 stack handles directly. Also cheap.

**ASS/SSA typesetting is the hard part.** Honest assessment follows.

### 6.3.1 What libass actually does

1. **The override-tag language.** `\pos \move \org \fad \fade \t \clip \iclip \p (vector drawing with
   beziers) \frx \fry \frz \fax \fay \fsp \fscx \fscy \be \blur \bord \shad \xbord \ybord \xshad \yshad
   \1c–\4c \1a–\4a \k \kf \ko \K \q \r \an \a \i \b \u \s \fn \fs \fe \c`. Roughly 45 tags, several with
   animated (`\t`) forms that interpolate over an event's lifetime.
2. **VSFilter bug-compatibility.** libass deliberately reproduces VSFilter behaviours because that is what
   subtitles were authored against: `ScaledBorderAndShadow` semantics, the difference between `\be` and
   `\blur`, storage-size versus PlayRes scaling, the `WrapStyle` line-breaking rules, and the collision
   avoidance that shifts overlapping events. Getting these wrong makes real-world files visibly wrong even
   when every tag is "implemented".
3. **Its own outline pipeline.** Glyph outlines and `\p` drawing paths are stroked to produce borders
   (libass ships its own stroker rather than using FreeType's), offset to produce shadows, and blurred with
   a specific radius→sigma mapping on the alpha bitmap.
4. **Font handling.** Per-run fallback with HarfBuzz shaping, plus **fonts embedded as Matroska
   attachments** — essential for fansub-style content, and something we must support by feeding
   `AttachedFile` payloads into `fontdb` as in-memory sources.
5. **Caching.** Shaped runs, outlines, stroked outlines, blurred bitmaps, and composited event bitmaps are
   all cached; without this, complex karaoke is not real-time.

### 6.3.2 Plan and honest gap statement

- **Build `vaco-ass` ourselves**, on the §6.1 stack plus `tiny-skia` for path filling and stroking (which
  gives us the `\p` drawing mode and border generation for free, and clears all three gates).
- **Assess `ass-core` as a parser base only.** It clears Gates 1 and 2 and self-describes as zero-unsafe,
  but it fails Gate 3 today on adoption and maintenance, and register task §9.6 requires independent
  verification that it carries no libass-derived logic before we could take it. Use it, if at all, for the
  script/section/tag parser — the least risky and least valuable part — not the renderer.
- **Scope in three stages**: (a) full ASS v4+ script parsing, styles, and the static tag set including
  `\pos`, `\an`, colours, borders, shadows, `\fs`, `\fn`, `\fscx/y`, `\frz`, `\clip`; (b) animation
  (`\t`, `\move`, `\fad`/`\fade`), karaoke (`\k` family), vector drawing (`\p`), 3-D rotation
  (`\frx`/`\fry`, `\fax`/`\fay`, `\org`); (c) VSFilter compatibility quirks and collision resolution.

**The gap, stated plainly.** Stage (a) is ~4 person-weeks and gets ordinary dialogue subtitles looking
right. Stages (a)+(b) are ~10–12 person-weeks and cover the great majority of what people actually author.
Stage (c) — matching libass on a wild corpus, including the deliberate VSFilter bugs — is not a
person-weeks problem; libass has ~20 years of accumulated compatibility fixes and matching it is a
multi-quarter project that is really its own product. **We should not claim libass parity.** The plan is:
ship stages (a)+(b), publish a known-divergence list, gate `subtitles`/`ass` behind a "rendering may differ
from libass for complex typesetting" note in the docs, and treat visual differential testing against the
reference binary (SSIM threshold per frame, not byte equality) as the acceptance criterion. Bitmap and
simple-text subtitles are held to exact comparison; ASS is not.

There is no shortcut available: libass is ISC-licensed and would otherwise be adoptable, but it is C, and
Gate 1 admits no exceptions. This is the single largest capability that D10 costs the filter subsystem, and
it is worth recording as such.

---

# 7. Hardware filters — the wgpu path

## 7.1 The upstream situation

FFmpeg reaches the GPU through six mutually incompatible per-vendor APIs: OpenCL (21 filters), Vulkan
(19), CUDA (12), VAAPI (13), QSV (7), plus D3D11/12, VideoToolbox and AMF. The same handful of
operations — scale, overlay, transpose, blur, tonemap, deinterlace — is implemented four, five or six
separate times, once per backend, each with its own device setup, its own memory model, its own kernel
language, and its own bugs. That is ~87 filter names and, more importantly, ~87 independent maintenance
surfaces for ~20 distinct operations.

The reason is historical and structural: there was no portable GPU compute API when this work started, and
C has no way to write one kernel and run it everywhere.

## 7.2 What wgpu changes

`wgpu` (MIT OR Apache-2.0, pure Rust at the crate level, explicitly confirmed permitted under D10) targets
Vulkan, Metal, D3D12 and GL from one WGSL source. One compute kernel, written once, runs on Intel, AMD,
NVIDIA and Apple silicon, on Linux, Windows and macOS.

**This is a genuine architectural advantage and it should be stated as one.** It is not "we can also do
GPU filters" — it is that the GPU filter subsystem collapses from ~87 implementations to ~16 kernels in one
crate, with one device abstraction, one memory model and one test surface. FFmpeg cannot do this without
adopting a portable compute layer it does not have, and retrofitting one across eight existing backends is
not a realistic path for it. Of everything in this plan, this is the change most likely to leave us
structurally *ahead* rather than merely at parity.

The initial kernel set, covering the operations behind those 87 names:

| Kernel | Replaces upstream |
|---|---|
| `convert` (pixel format + full colorimetry) | the format half of every `scale_*` |
| `scale` (bilinear / bicubic / lanczos, separable) | `scale_{cuda,vaapi,qsv,vulkan,opencl,vt,d3d11,d3d12}`, `vpp_*` |
| `overlay` / `blend` | `overlay_{cuda,opencl,qsv,vaapi,vulkan}`, `blend_vulkan` |
| `transpose` / `flip` | `transpose_{cuda,opencl,vaapi,vt,vulkan}`, `{h,v}flip_vulkan`, `flip_vulkan` |
| `pad` | `pad_{cuda,opencl,vaapi}` |
| `gblur` / `avgblur` / `boxblur` | `{gblur,avgblur,boxblur}_{vulkan,opencl}` |
| `unsharp` | `unsharp_opencl` |
| `convolution` / `morphology` | `convolution_opencl`, `{dilation,erosion,neighbor}_opencl`, `{sobel,prewitt,roberts}_opencl` |
| `tonemap` | `tonemap_{opencl,vaapi}`, much of `libplacebo` |
| `lut3d` | `haldclut`/`lut3d` GPU paths, part of `libplacebo` |
| `chromakey` / `colorkey` | `chromakey_cuda`, `colorkey_opencl` |
| `deinterlace` (yadif/bwdif) | `yadif_{cuda,videotoolbox}`, `bwdif_{cuda,vulkan}`, `deinterlace_{qsv,vaapi,d3d12}` |
| `xfade` | `xfade_{opencl,vulkan}` |
| `nlmeans` | `nlmeans_{opencl,vulkan}` |
| `remap` / `v360` | `remap_opencl`, `v360_vulkan` |
| `stack` | `{h,v,x}stack_{vaapi,qsv}` |

## 7.3 The unsafe question — answered precisely

D2 puts `#![forbid(unsafe_code)]` on **our** crates. That lint is per-crate and does not reach dependencies;
D10 states this explicitly and makes it a measured trade-off rather than a prohibition. wgpu's safe API is
100% safe from the caller's side — `vaco-filter-gpu` carries `#![forbid(unsafe_code)]` and means it — while
wgpu-hal internally uses `unsafe` to talk to the drivers, exactly as `std` does to talk to the kernel.

The constraint that actually bites: **wgpu's `unsafe` entry points are off limits to us.**
`create_texture_from_hal`, external-memory import, and the raw-handle escapes are how you achieve zero-copy
interop with a hardware video decoder. We do not call them from `vaco-filter-gpu`.

## 7.4 Consequence: where zero-copy lives, and what it costs

Three pipelines, with honest numbers:

1. **Software decode → GPU filter → software encode.** Frames upload and download over the bus. 1080p NV12
   is 3.1 MB; at 60 fps that is ~190 MB/s each way. On unified memory (Apple silicon, integrated Intel/AMD)
   this is close to free. On a discrete GPU it costs real bandwidth and adds a frame of latency, and it only
   pays off when the filter chain is heavy — a single `scale` is not worth the round trip, a `nlmeans` or a
   long chain very much is. The graph should therefore prefer to keep a *run* of GPU filters together, which
   §1.7's converter-coalescing logic already expresses: `hwupload` and `hwdownload` are inserted at the
   boundaries of a GPU run, never per filter.
2. **`vaco-play`.** Zero copies on the output side: the graph's final GPU frame *is* the swapchain texture.
   `vaco-play` already owns a wgpu device (arch §3 layer 7), so GPU filtering there is strictly better than
   the CPU path with no interop problem at all. This is the strongest near-term case for the crate.
3. **Hardware decode → GPU filter.** Needs the hal import path, which needs `unsafe`. It therefore lives in
   `vaco-hw-videotoolbox` / `-vaapi` / `-d3d11` / `-vulkan` — already on the D2 allowlist — which produce an
   already-safe `wgpu::Texture` that `vaco-filter-gpu` consumes without knowing where it came from. The
   unsafe stays behind the boundary that was designed for it, in an opt-in, non-default crate. Given D9's
   finding that hardware delegation is our strongest patent mitigation, this path is strategically important
   and should not be deferred indefinitely.

## 7.5 Integration with the framework

- `MediaType` gains no new variant. Instead a link carries `hw: Option<Arc<GpuDevice>>`, and `Property::HwCtx`
  participates in negotiation: two links may only connect if their `hw` matches by device identity. A
  mismatch is **never** auto-converted (§1.7) — it errors and names `hwupload`/`hwdownload`.
- `GpuFrame` is a `Frame` whose planes are `PlaneStorage::Gpu(wgpu::Texture)` rather than `Arc<[u8]>`. Every
  CPU filter's `plane()` accessor returns `Err(Error::FrameOnDevice)` with a message naming `hwdownload`, so
  a mis-wired graph fails at the first frame with a clear message rather than at negotiation time with an
  obscure one.
- Filters marked `METADATA_ONLY` pass GPU frames through untouched, matching upstream's optimisation.
- `SliceFilter` does not apply; a GPU filter implements `GpuFilter` and dispatches workgroups, and the
  adapter handles command-encoder batching so a run of N GPU filters submits **one** command buffer per
  frame rather than N.

## 7.6 Limits to be honest about

- **Determinism.** GPU floating point differs across vendors and drivers. GPU filters are excluded from the
  exact-match differential harness and gated on a PSNR/SSIM threshold against the CPU implementation of the
  same filter instead. Every GPU kernel must have a CPU counterpart; that is also what makes the exclusion
  safe.
- **WGSL feature gaps.** `f16` needs the `shader-f16` extension; subgroup operations need another. High-bit-
  depth YUV needs `r16uint`/`rg16uint` storage textures and careful format mapping. Plan for the baseline
  feature set and gate the fast paths on runtime feature queries.
- **Not every filter belongs on the GPU.** Anything with a serial dependency across the frame (IIR blurs,
  `atempo`, temporal accumulators), anything with data-dependent output size, and anything that reads back
  per-frame statistics into a CPU decision are all better left alone. The kernel list in §7.2 is deliberately
  restricted to genuinely parallel, fixed-output-size operations.
- **Startup cost.** Device creation and shader compilation are hundreds of milliseconds. The device is
  created lazily on the first `hwupload` and cached per process; pipelines are compiled at graph
  configuration, not per frame.

---

# 8. Work breakdown

Estimates are person-weeks for an engineer already familiar with the codebase. "Lane" identifies work that
can proceed in parallel; work in the same lane is sequential. The critical path is lane A.

## 8.1 Phase 1 — the framework (blocking; almost nothing else can start)

| # | Item | Lane | Est. | Depends on |
|---|---|---|---:|---|
| 1.1 | Pad/link model, `FrameQueue`, `AudioFifo`, `QueueBudget`, link stats | A | 2.0 | `vaco-frame`, `vaco-pool` |
| 1.2 | `Filter` trait, `FilterCtx`, node/link arenas, the split-borrow driver | A | 1.5 | 1.1 |
| 1.3 | Readiness scheduler, `run_once`/`run`, quiescence classifier | A | 2.0 | 1.2 |
| 1.4 | Status/EOF propagation, timestamp rules, generic forwarding helpers | A | 1.5 | 1.3 |
| 1.5 | Format negotiation: `Constraint`, union-find equality, intersect, repair, loss function, PICK, configure | A | 3.5 | 1.2 |
| 1.6 | Negotiation diagnostics + provenance + snapshot tests | A | 1.0 | 1.5 |
| 1.7 | Adapters: `Simple`, `SourceFilter`, `AudioFilter` | A | 2.0 | 1.4 |
| 1.8 | `SliceFilter` adapter, band splitting, rayon pool, deterministic reductions | B | 2.0 | 1.7 |
| 1.9 | Timeline `enable=`, `vaco-expr` integration | B | 1.0 | 1.7 |
| 1.10 | Command dispatch, option-schema default impl, queued commands | B | 1.0 | 1.7 |
| 1.11 | Buffer sources/sinks, `SourceHandle`/`SinkHandle`, sink constraints | A | 1.5 | 1.4 |
| 1.12 | `vaco-filter-framesync` + adapter + the option truth table | C | 2.5 | 1.7 |
| | **subtotal** | | **21.5** | |

## 8.2 Phase 2 — the graph layer (starts once 1.5 lands)

| # | Item | Lane | Est. | Depends on |
|---|---|---|---:|---|
| 2.1 | `next_token` escaping scanner + the three-level test-vector corpus | D | 1.5 | — |
| 2.2 | AST, chain/filter/label parsing, spans, caret diagnostics | D | 1.5 | 2.1 |
| 2.3 | Instantiation, option binding (positional + key=value), dynamic pads | D | 1.5 | 2.2, `vaco-opts` |
| 2.4 | Link resolution (labels + unlabeled auto-connect), open-pad export | D | 1.5 | 2.3 |
| 2.5 | Validation checks + messages, Kahn sort, cycle detection | D | 1.0 | 2.4 |
| 2.6 | `ConverterFactory` policy, coalescing, `sws_flags=` plumbing, `auto_*` naming | D | 1.5 | 2.5, 1.5 |
| 2.7 | `to_dot`, `dump`, round-trip property tests | D | 1.0 | 2.2 |
| | **subtotal** | | **9.5** | |

## 8.3 Phase 3 — shared helpers (fully parallel once Phase 1 lands)

| # | Item | Lane | Est. |
|---|---|---|---:|
| 3.1 | `vaco-filter-draw`: colour parsing, format-aware fill/blend/box, subsampled + high-bit-depth paths | E | 3.0 |
| 3.2 | `vaco-filter-vdsp`: scene_sad, edge_common, box-blur core, SAD/hadamard, integral images | F | 3.0 |
| 3.3 | `vaco-filter-vdsp`: motion_estimation, affine transform, LUT sampling, morphology core | F | 3.0 |
| 3.4 | `vaco-filter-adsp`: biquad design, wave tables, windows, EBU R128 core, partitioned FIR | G | 3.5 |
| 3.5 | `vaco-filter-text`: `TextRenderer`, fontdb + alias table, shaping/glyph caches, mask rasterisation | H | 4.0 |
| | **subtotal** | | **16.5** |

## 8.4 Phase 4 — filters, by tier

Filters are the embarrassingly parallel part: after Phase 1 and the relevant Phase 3 helper, each crate is
independent, each filter within a crate is independent, and each lands with its own tests and fuzz target.

| # | Item | Lane | Est. | Note |
|---|---|---|---:|---|
| 4.1 | **T1 video** (13) | I | 5.0 | `scale` and `overlay` dominate; the rest are days each |
| 4.2 | **T1 audio** (11) | J | 4.0 | `aresample` is `vaco-resample` plumbing; `pan`/`join`/`channelmap` share layout logic |
| 4.3 | **T1 plumbing + sources/sinks + trim/concat** (24) | I | 4.0 | `concat` and `select` carry the complexity |
| 4.4 | **T2 colour + LUT** (~34) | K | 8.0 | shares `vaco-color`; LUT file parsers are ~1.5 of it |
| 4.5 | **T2 deinterlace** (yadif, bwdif, w3fdif, estdif, fieldmatch, decimate, telecine family) | L | 6.0 | spec sourcing per §5.7 |
| 4.6 | **T2 blur/sharpen/convolve** (~28) | M | 6.0 | |
| 4.7 | **T2 geometry** (~28) | I | 6.0 | |
| 4.8 | **T2 audio EQ + dynamics** (~40) | J | 9.0 | biquad family is one file for 12 filters |
| 4.9 | **T2 analysis/metrics** (psnr, ssim, vif, xpsnr, signalstats, …) | N | 5.0 | |
| 4.10 | **T2 text/drawing** (drawtext, drawbox, drawgrid, drawgraph) | H | 4.0 | after 3.5 |
| 4.11 | **T2 palette/GIF, stack, overlay family, temporal** | M | 6.0 | |
| 4.12 | **T3 video long tail** (~150) | any | 34.0 | ~1.5 days each on average; genuinely parallel |
| 4.13 | **T3 audio long tail** (~64) | any | 14.0 | |
| | **subtotal (T1+T2)** | | **63.0** | |
| | **subtotal (T3)** | | **48.0** | |

## 8.5 Phase 5 — the expensive singletons

| # | Item | Lane | Est. | Note |
|---|---|---|---:|---|
| 5.1 | Bitmap + simple-text subtitle rendering | O | 1.5 | reaches parity immediately |
| 5.2 | `vaco-ass` stage (a): parsing, styles, static tags | O | 4.0 | §6.3.2 |
| 5.3 | `vaco-ass` stage (b): animation, karaoke, `\p` drawing, 3-D rotation | O | 7.0 | |
| 5.4 | ASS visual differential harness (per-frame SSIM gate) + divergence list | O | 2.0 | |
| 5.5 | `vaco-filter-gpu`: device/frame model, negotiation integration, hwupload/hwdownload, encoder batching | P | 4.0 | |
| 5.6 | 16 WGSL kernels + CPU-counterpart differential gates | P | 10.0 | §7.2 |
| 5.7 | VMAF implementation + validation against published scores | Q | 10.0 | §5.5; schedule late |
| 5.8 | `v360` | any | 4.0 | 5100 LOC upstream; heavy but self-contained |
| 5.9 | Stabilisation (`stabdetect`/`stabtransform`, `deshake`) | any | 5.0 | after 3.3 |
| 5.10 | `vaco-filter-dnn` over `tract` (behind a non-default feature) | any | 4.0 | Gate 3 assessment of `tract` first |
| | **subtotal** | | **51.5** | |

## 8.6 Phase 6 — cross-cutting, continuous

| # | Item | Est. |
|---|---|---:|
| 6.1 | Differential harness integration: per-filter argument-vector corpus, framecrc comparison, allowlist management | 4.0 |
| 6.2 | Fuzz targets: graph-string parser (highest value — it is the one attacker-reachable text parser here), option parsing, per-filter frame fuzzing | 3.0 |
| 6.3 | Benchmarks: per-filter Criterion suites with CI regression tracking | 3.0 |
| 6.4 | `docs/filter/*.md` per crate, per the repository documentation standard | 4.0 |
| | **subtotal** | **14.0** |

## 8.7 Totals and shape

| Milestone | Person-weeks | Cumulative |
|---|---:|---:|
| Framework + graph + framesync (Phases 1–2) | 31.0 | 31.0 |
| Shared helpers (Phase 3) | 16.5 | 47.5 |
| T1 filters — a usable transcoder (4.1–4.3) | 13.0 | 60.5 |
| T2 filters — competitive coverage (4.4–4.11) | 50.0 | 110.5 |
| Text + subtitles stages (a)+(b) (3.5, 4.10, 5.1–5.4) | 18.5 | 129.0 |
| GPU path (5.5–5.6) | 14.0 | 143.0 |
| T3 long tail (4.12–4.13) | 48.0 | 191.0 |
| Remaining singletons (5.7–5.10) | 23.0 | 214.0 |
| Cross-cutting (Phase 6) | 14.0 | 228.0 |

**Parallelism.** Phase 1 lanes A/B/C support ~2–3 engineers; after it lands, lanes E–Q are largely
independent and the project can absorb 6–8 engineers productively. The T3 long tail (48 pw across ~214
filters) is the most parallelisable work in the entire project — it is the natural home for new
contributors, since each filter is small, independent, spec-driven, and lands with its own tests.

**The critical path is Phase 1, and it is short.** 21.5 person-weeks of framework work gates everything
else, which argues for staffing it with the most senior available people and not parallelising it beyond
three lanes. The adapters (1.7, 1.8, 1.12) deserve particular care: every one of ~560 filters is written
against them, so an awkward API there is paid for ~560 times.

**First useful milestone.** Phases 1–2 plus items 4.1–4.3 — 60.5 person-weeks — yields a filtergraph that
can scale, crop, pad, overlay, retime, trim, concatenate, resample, mix and remap channels, driven by the
full textual DSL. That is the point at which `vaco` becomes a transcoder rather than a remuxer.

---

# 9. Open questions

1. **`rustybuzz` provenance** (register §9.7). It is a port of HarfBuzz, not a wrapper. If any of it is a
   near-verbatim translation, Old-MIT attribution may travel with it. Blocks `vaco-filter-text` adoption;
   needs resolving early because the whole text stack sits on it.
2. **`ass-core` provenance** (register §9.6). Same question against libass. Lower stakes — we plan to write
   our own renderer regardless — but it decides whether we can reuse its parser.
3. **Gate 3 assessments not yet done**: `tract`, `rqrr`, `tiny-skia`, `qrcode`, `candle`. Each needs a
   `docs/dependencies.md` entry before adoption per D10.
4. **`vaco-filter-movie`'s layering exception.** A filter that opens media needs registries that sit above
   it. The `MediaOpener` trait injection (§4.4) resolves it, but it needs sign-off as a reviewed exception
   in the layer-check script rather than being discovered as a cycle later.
5. **Deinterlacer specification sourcing.** `yadif` in particular has no single authoritative published
   description; we are assembling one from independent reimplementations' documentation. A spec writer
   should produce `planning/spec/deinterlace.md` before implementation starts, per D7.
6. **`slice_count` defaults.** Arch §6 rejects upstream's 16-thread ceiling in favour of measurement. The
   measurement has not been done; until it is, the default is `available_parallelism()` and the benchmark
   suite must include a thread-scaling sweep per filter class.

## Amendment — §1.6.4's loss table corrected against the binary (2026-08-22)

The loss table was wrong in three ways, found by `vaco-filter-core` and then
re-measured after my own proposed correction turned out to be wrong too.

### The measured ordering

> **chroma-total > alpha > depth > colour model > chroma coarsening > packing**

### The three errors

1. **Chroma resolution sat above colour model. It is below.** A YUV↔RGB change
   is cheaper than losing even *one bit* of depth, confirmed at 1, 2, 4 and 8
   bits: from `yuv444p10le`, offered `yuv444p9le` (one bit) against `rgb48le`
   (colour model), the reference takes `rgb48le`.
2. **Chroma loss was graded per halved axis. It is a flag.** `yuv444p→yuv422p`
   and `yuv444p→yuv420p` score identically; the reference then picks on its own
   enum order, which D1 says we do not mirror.
3. **There was no notion of losing chroma *entirely*.** A greyscale destination
   is a distinct component from a colour-model change, it is the heaviest in the
   model, and it is the only one above alpha: from `yuva444p`, offered
   `yuv444p` (lose alpha) against `ya8` (lose chroma), the reference takes
   `yuv444p`.

### How the corpus missed it, and the general rule

The original 17 measured pairs had a colour destination on both sides, or grey
as the *source* — never grey as a *candidate*, which is the only pair that
discriminates. That is a coverage gap in the corpus rather than a fault in the
method, and it generalises:

> **Enumerate the components first, then cover every *pair* of components with
> the others held equal.** Collecting interesting-looking pairs and inferring an
> order from them leaves exactly this shape of hole.

The corpus is now 35 rows, all order-independent, with all 17 originals still
passing — so the reweighting resolved a gap rather than trading one error for
another.

### My own wrong inference, recorded because the shape recurs

I probed three grey-destination cases, saw the model pick grey where the
reference did not, and concluded that **colour model outranks depth**. The
probes were right and the inference was wrong: I had conflated "changes the
colour model" (YUV→RGB, which keeps chroma) with "loses chroma entirely"
(YUV→grey). Both look like a colour change in prose; the model treats them as
different components. Had the agent implemented my proposal it would have broken
four rows that were already passing.

The lesson is narrower than "measure": I *did* measure. It is that a probe
distinguishes the hypotheses you thought of. Three cases that all differ in two
ways at once cannot separate those two ways, however many of them you run — the
discriminating probe is the one that varies a single component. The agent found
it by holding depth equal and varying only chroma-total, which is the move I
should have made.

### Enforcement

The tier order is now five `const _: () = assert!(…)` statements at module
scope, so a reweighting that breaks it fails to **compile** rather than failing a
test. Same technique as `vaco-pool`'s `Padded::PAD`.
