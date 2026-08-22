# `vaco-codec-core`

Layer 3. The codec framework: what a decoder, encoder, parser and bitstream
filter *are*, the state machine all four share, and the primitives that make
frame threading expressible in safe Rust.

Per D14.1 this crate sits **below** `vaco-format-core`, because demuxers need
codec parameters and bitstream parsers. It knows about no specific codec — it
defines the seams and nothing else.

## What it is

| Module | Contents |
|---|---|
| `machine` | `Machine`, the send/receive state machine every component embeds |
| `protocol` | `SendReceive`, the adapters onto the three trait faces, and `Validated` |
| `parser` | `ParserDriver`, the harness that drives a `Parser` over a byte stream |
| `caps` | `Caps` (implementation capabilities) and `CodecProperties` (format facts) |
| `params` | `CodecParameters`, `Profile`/`ProfileTable`, `Level`/`LevelTable` |
| `picture` | `ProgressPicture`, `PictureWriter`, `PictureRef`, `PlaneView` |
| `threading` | `Threading`, `FrameThreadedDecoder`, `FrameTask`, `TaskCtx` |
| `mock` | a reference codec and parser that exercise every corner of the protocol |

The one idea worth reading first: **packet-to-frame is genuinely N:M.** One
packet can yield several frames, several packets can yield none while a reorder
buffer fills, and draining at end of stream can yield many. `decode(packet) ->
Frame` cannot express that. Send/receive can, and the rules are identical for
decoders, encoders and bitstream filters — so they are written once, executed
once, and enforced once.

## How it works

### The send/receive protocol

```text
               send(Some) ─┐        ┌─ receive → output
                           ▼        │
   open ──────────►   Feeding ──────┴──►  Feeding
                         │  send(None)
                         ▼
                     Draining ──receive*──► Drained ──receive──► Err(Eof)
                         │                     │
                         └───── flush() ───────┴──────► Feeding
```

Three states. `Feeding` is the steady state. `send(None)` moves to `Draining`,
where the component produces whatever it still holds. Once it has, `receive`
reports `Eof` and the state is `Drained`. `flush()` returns to `Feeding` from
anywhere.

#### The rules, normatively

Each rule has an identifier, which is what `Violation::describe` prints when a
component breaks it.

| # | Rule | What breaking it looks like |
|---|---|---|
| **S1** | `send` returns `Ok`, `OutputPending`, `Eof`, or a real error — never `NeedMoreInput` | `Violation::NeedMoreInputFromSend` |
| **S2** | `OutputPending` is **backpressure, not failure**: nothing was consumed, the caller still owns the input, and a `receive` must now succeed | `Violation::BackpressureWithoutOutput` |
| **S3** | After `send(None)`, every further `send` returns `Eof` | `Violation::SendAfterEof` |
| **R1** | `receive` returns `Ok`, `NeedMoreInput`, `Eof`, or a real error — never `OutputPending` | `Violation::OutputPendingFromReceive` |
| **R2** | `NeedMoreInput` while `Feeding` means "send more". It is not an error | — |
| **R3** | `NeedMoreInput` never occurs while `Draining`: output or `Eof`, nothing else | `Violation::NeedMoreInputWhileDraining` |
| **R4** | `Eof` never occurs before `send(None)` | `Violation::EofBeforeDrain` |
| **R5** | `Eof` is stable: once reported, no further output ever appears | `Violation::OutputAfterEof` |
| **R6** | Output only ever follows input | `Violation::OutputWithoutInput` |
| **C1** | A component without `Caps::DELAY` produces nothing during a drain | `Violation::DelayedOutputWithoutCap` |
| **C2** | A component without `Caps::SUBFRAMES` produces at most one output per input | `Violation::SubframesWithoutCap` |
| **F1** | `flush()` is infallible and total: the post-state is exactly a fresh component's, minus reparsing extradata | `Violation::FlushDidNotReset` |
| **E1** | A decode error is **not** terminal unless it is fatal. The caller decides whether to emit the frame with `FrameFlags::CORRUPT` or suppress it | — |
| **D1** | Output is bit-identical for any legal thread count. Conformance runs diff `threads ∈ {1, 2, 3, 8, 17}` | — |

Rules S1–S3, R1–R6, C1–C2 and F1 are **checked**, not merely documented. Wrap a
component in `Validated` and a violation panics with the rule's description.

#### The caller's loop

```rust
for packet in packets {
    loop {
        match decoder.send_packet(Some(&packet)) {
            Ok(()) => break,
            // Backpressure: drain and retry with the SAME packet.
            Err(Error::OutputPending) => sink(decoder.receive_frame()?),
            Err(e) => return Err(e),
        }
    }
    loop {
        match decoder.receive_frame() {
            Ok(frame) => sink(frame),
            Err(Error::NeedMoreInput) => break,
            Err(e) => return Err(e),
        }
    }
}
decoder.send_packet(None)?;              // begin draining
loop {
    match decoder.receive_frame() {
        Ok(frame) => sink(frame),
        Err(Error::Eof) => break,
        Err(e) => return Err(e),
    }
}
```

Skipping the drain on a component with `Caps::DELAY` silently truncates the
stream. That is the single most common integration bug, and `Caps::needs_drain()`
is how a scheduler avoids it.

### `Machine`: the protocol, embedded

A codec does not reimplement the state machine; it embeds one and calls three
methods.

```rust
impl SendReceive for MyCodec {
    type Input = Packet;
    type Output = Frame;

    fn caps(&self) -> Caps { self.machine.caps() }

    fn send(&mut self, input: Option<&Packet>) -> Result<()> {
        // 1. The machine validates the transition and applies backpressure.
        //    Nothing below runs if it refuses.
        match self.machine.accept(input.is_none())? {
            Accept::Drain => {
                for frame in self.reorder_buffer.drain(..) {
                    self.machine.emit(frame);          // 2. produced output
                }
                self.machine.finish();                 // 3. nothing more, ever
                Ok(())
            }
            Accept::Input => { /* decode, emit zero or more frames */ Ok(()) }
        }
    }

    fn receive(&mut self) -> Result<Frame> { self.machine.receive() }
    fn flush(&mut self)  { self.machine.flush(); self.reorder_buffer.clear(); }
}
```

`Machine` never stores input. `OutputPending` means "you still own that packet",
so there is nothing to hold. It stores only produced-but-not-taken output, in a
queue whose depth comes from `Caps`: `TIGHT_CAPACITY` (one) for a component that
declares neither `DELAY` nor `SUBFRAMES`, `DEFAULT_CAPACITY` (sixteen) otherwise.
A codec that secretly buffers therefore hits backpressure immediately instead of
growing without bound.

`finish()` is separate from `accept(true)` on purpose. Until a component says it
is done, `receive` on an empty queue reports `NeedMoreInput` rather than `Eof` —
because claiming end of stream while the component still holds frames is exactly
how frames get lost.

### `SendReceive` and the three faces

`Decoder`, `Encoder` and `BitstreamFilter` are the same machine over three pairs
of types. `SendReceive` states it once; the adapters put the expected face on it:

| Implement | Wrap in | Get |
|---|---|---|
| `SendReceive<Input = Packet, Output = Frame>` | `AsDecoder` | `Decoder` |
| `SendReceive<Input = Frame, Output = Packet>` | `AsEncoder` | `Encoder` |
| `SendReceive<Input = Packet, Output = Packet>` | `AsBitstreamFilter` | `BitstreamFilter` |

The point is not economy of code. It is that `Validated` only has to exist once.
A hand-written `Decoder` reaches the same validator through `DecoderProtocol`, or
in one call through `validate_decoder(decoder, caps)`.

### `Validated`: violations fail a test, not production

```rust
let mut decoder = AsDecoder(Validated::new(MyCodec::new(params)?));
```

`Validated` shadows the protocol state, checks every call against the rule table,
and panics naming the rule that broke. `Validated::recording` collects violations
instead, for fuzz targets that want to accumulate findings rather than abort.

It is not debug-only. Wrapping in tests, fuzz targets and conformance runs is the
point; production builds simply do not wrap.

### The mock codec

`mock::MockCodec` is programmable through `Step`s and covers the three behaviours
a real codec has that a toy one does not:

| `Step` | Behaviour | Capability it needs |
|---|---|---|
| `Emit(n)` | *n* outputs from one input | `SUBFRAMES` when `n > 1` |
| `Reorder` | buffered, released once the delay is exceeded, flushed at EOF | `DELAY` |
| `Skip` | a header-only packet: no output at all | — |
| `Corrupt` | consumed, recoverable `InvalidData`, no output | — |

`MockProgram::caps()` **derives** the capabilities from the program rather than
letting the caller assert them — which is what makes the mock a correct example.
`MockProgram::expected()` is an independent reference model of the same
behaviour, written without reference to `Machine`; the property tests drive
arbitrary legal call sequences and compare against it, so "never loses or
duplicates an output" is checked rather than claimed.

### The parser harness

A `Parser` turns a byte stream into complete access units and fills in what the
container did not say. It never decodes — a distinction that is load-bearing
legally as well as architecturally (plan 15 §1.6): parsing an H.264 SPS
implements no decoder, so the parser crates ship in the default build while the
decoders do not. v0.1 is parsers, not decoders (D5), so this path carries the
milestone.

`ParserDriver` owns the three things every parser gets wrong at least once:

```text
  push(chunk)  ──►  [ reassembly buffer ] ──► parser.parse(&buf) ──► Packet
                           ▲                        │
                           └── unconsumed tail ─────┘
  finish()     ──►  parse(&[]) once the buffer drains ──► final Packet ──► Eof
```

* **Reassembly across chunk boundaries**, with a cursor and amortised compaction
  rather than an O(n) drain per call, and a cap (`DEFAULT_MAX_PENDING`) so a
  stream that never yields a unit cannot exhaust memory.
* **End of stream**, signalled by an empty input slice — the convention
  documented on the `Parser` trait, applied in exactly one place.
* **Byte accounting**: a parser claiming to have consumed more than it was given
  is caught, and a parser that neither consumes nor produces, repeatedly, is
  turned by `vaco_limits::ProgressGuard` into a localised error instead of a
  fuzzer timeout with no stack.

### Frame threading: mutable state never crosses a thread

The design that matters most (plan 15 §1.8.1). A frame-threaded decoder is split
in two:

* a **sequential header stage** (`FrameThreadedDecoder::split`) owning *all*
  mutable state — parameter sets, the DPB, reference lists, output allocation. It
  runs on the caller's thread in decode order and emits a task;
* a **stateless frame task** (`FrameTask: Send + 'static`) owning its bitstream
  bytes, `Arc` snapshots of its parameter sets, `PictureRef`s to its references
  and the sole `PictureWriter` for its output.

`Send + 'static` is the whole design in a bound: a task that can move to another
thread and outlive the call that made it cannot be holding a reference into
decoder state. There is no state-propagation step because there is no shared
mutable state to propagate.

#### Cross-frame progress: ownership transfer at band granularity

"Frame N+1 may proceed once frame N has produced row R" is the hard part. The
conventional answer is one contiguous buffer, a raw pointer and an atomic row
counter, with readers racing ahead of the writer into the same allocation. D2
rules that out, and ordinary borrow rules cannot express "`&mut` above row R and
`&` below it, and R moves".

A plane is instead allocated as a sequence of **bands** — `band_h` rows preceded
by `guard` rows of context copied from the band above. The writer owns a band
exclusively while filling it, then **moves** it into a `OnceLock`, which is
precisely where it stops being mutable and starts being shared.

```text
  writer                                    reader
  ------                                    ------
  band_mut(k)  ── exclusive &mut [u8]
  ...fill...
  publish_through(k):
      copy guard rows from band k-1
      bands[k].set(band)   ── release ──►   bands[k].get()   ── acquire
      ready.store(rows)                     ready.load() / wait_rows()
```

`OnceLock::set` is a release store and `get` an acquire load, so the fast path is
one atomic load with no syscall, and no type in the module can observe a
partially written band. `PictureWriter` is not `Clone` — exactly one task holds
it. `PictureRef` is cheap to clone and `Send + Sync` — it is what a task holds
for each reference picture.

A motion-compensation loop therefore reads:

```rust
let need = mv_bottom_row(block, mv) + GUARD as i32;
let src  = ctx.wait_rows(&reference, plane, need.clamp(0, h - 1) as u32)?;
let blk  = src.block(x0, y0, bw, bh + TAPS, &mut scratch)?;
kernels.mc_8tap_hv(blk.data, blk.stride, dst, dst_stride, frac_x, frac_y, bw, bh);
```

and after each macroblock or CTU row the producer calls
`writer.publish_through(plane, row_band)`.

`PlaneView::block` has two paths. **Fast**: the region lies inside one band's
allocation, and the borrow is that band's own memory at its natural stride —
identical in cost to a raw `(ptr, stride)` pair. **Copy**: the region straddles a
seam or falls outside the picture, and it is copied into a caller-owned
`BlockScratch` with edge replication. That is the *same* cold path out-of-picture
motion vectors already need, so it costs one extra condition rather than a new
mechanism. Budget: under 1.5% of decode time, to be measured in `vaco-checkasm`
before AV1 lands.

Three escape hatches, in order of preference:

1. `PictureSpec::single_band()` whenever frame threading is off or the codec is
   intra-only. Every block then takes the fast path and the non-threaded case
   pays nothing at all.
2. Slice or tile threading instead, for codecs where it is at least as good — AV1
   tiles, HEVC tiles and WPP, VP9 tile columns. Frame threading matters most for
   H.264 and VP8, which have neither.
3. If a kernel provably cannot reach parity, escalate as a decision per D2. Do
   not reach for `unsafe`.

#### Deadlock freedom

1. **Acyclicity.** A task waits only on pictures earlier in decode order, and the
   header stage emits tasks in decode order, so the wait graph is a DAG.
   `TaskCtx::wait_rows` carries the debug assertion that checks it.
2. **Monotonic progress.** `ready` never decreases; `publish_through` is its only
   writer and is called from the single task owning the `PictureWriter`.
3. **Liveness under failure.** `PictureWriter::drop` marks an incomplete picture
   failed and wakes every waiter with an error. A panicking, cancelled or
   early-returning task therefore unblocks its readers instead of hanging the
   pipeline. Every `wait_rows` terminates: progress arrives, or the picture fails.

### `Caps`, and what CI does with them

`Caps` describes an *implementation*; `CodecProperties` describes a *format* and
hangs off `CodecId`, so a container can ask whether timestamps may be reordered
before it has opened anything.

`Caps::PATENT_ENCUMBERED` is the runtime half of D4's assertion. D4 requires CI
to prove no encumbered component is reachable from a default build, and to prove
it on the compiled artefact rather than on intent:
`DecoderDesc::is_default_build_safe()` is the predicate CI evaluates over every
descriptor the registry exposes.

### Profiles and levels

Profiles are a codec-scoped integer plus a name; levels are a codec-scoped
integer plus a *constraint* table, because levels are what `-level` validation,
DPB sizing and hardware capability matching all consult. The raw encoding is
never normalised across codecs — H.264 level ×10, HEVC `general_level_idc` ×30,
AV1 `seq_level_idx`, VP9 level ×10 — because round-tripping a container's value
back out byte-identically is what `vaco-probe` needs.

The tables live **with the codec** (`vaco-codec-av1` supplies AV1's, from the AV1
specification Annex A) and are reached here through `ProfileTable` and
`LevelTable`. A central table would mean this crate had to know every codec.

## How to change it

* **Adding a rule to the protocol.** Add the `Violation` variant with its
  description, implement the check in `Validated`, add the row to the rule table
  above, and add a component to `tests/protocol.rs` that breaks it. A rule with
  no failing test is documentation, not a rule.
* **Adding a `Caps` flag.** Append at the next free bit. Never renumber:
  descriptors are `const` values across many crates. Add it to `CAP_NAMES`, which
  is what the CLI prints and parses.
* **Adding a codec.** `CodecId` is hand-written here for now; plan 15 §1.1 has it
  generated from `codecs.toml` the way `vaco-pixfmt`'s table is. The `CodecEntry`
  shape is what that generator must emit, so adding a variant means adding a row
  — the lookup is a linear scan over a table small enough that it does not
  matter.
* **Changing band geometry.** `DEFAULT_GUARD` is the maximum inter-prediction
  filter reach across H.264, HEVC, VP9 and AV1. Raising it widens the fast path
  and costs memory; lowering it below any codec's reach makes that codec's reads
  take the copy path silently rather than incorrectly. `DEFAULT_BAND_HEIGHT`
  trades fast-path hit rate against how long a consumer waits for a producer.
* **The gotcha.** `PictureWriter::finish()` consumes the writer and marks the
  picture complete. Dropping it any other way marks the picture *failed*, which
  is deliberate — it is the liveness guarantee — but it means an early `return`
  in a task fails the picture, which is exactly what you want and exactly what
  surprises people.

## Configuration

No Cargo features. Behaviour is set through values, not build flags:

| Knob | Where | Default |
|---|---|---|
| Output queue depth | `Machine::with_capacity` | `TIGHT_CAPACITY` (1) or `DEFAULT_CAPACITY` (16), from `Caps` |
| Violation policy | `Validated::new` / `::recording` | panic |
| Flush probe | `Validated::with_flush_probe` | on |
| Reassembly cap | `ParserDriver::with_max_pending` | `DEFAULT_MAX_PENDING` (2 MiB) |
| Allocation budget | `Limits` passed to `ParserDriver::new`, `ProgressPicture::allocate`, `BlockScratch::new` | caller's |
| Band height / guard | `PictureSpec::with_band_height` / `::with_guard` | 256 rows / 8 rows |
| Thread count | `Threading::clamped_to` | narrows only; `1` yields `Threading::None` |

## Dependencies

`vaco-core` (error taxonomy, `Rational`, `Timestamp`, `MediaType`), `vaco-frame`,
`vaco-packet`, `vaco-pixfmt`, `vaco-sampfmt`, `vaco-chlayout`, `vaco-color`
(parameter and frame vocabulary), `vaco-limits` (every allocation here is
budgeted), `vaco-opts`, and `bitflags`. Nothing outside the workspace beyond
`bitflags`; `proptest` for tests.

Nothing depends on a concrete codec, and nothing here knows one exists.

## Testing

* `tests/protocol.rs` — every state transition, both violation kinds caught, and
  the panic mode itself.
* `tests/proptest_sendreceive.rs` — arbitrary legal call sequences against the
  reference model: never panics, never loses or duplicates an output, queue depth
  never changes the output, and a flushed codec is indistinguishable from a fresh
  one.
* `tests/picture.rs` — publication order, guard-row contiguity, the copy path,
  edge replication, a real blocking reader thread, disjoint slice band ranges,
  and the dropped-writer liveness guarantee.
* `tests/parser.rs` — reassembly, end of stream, over-reported consumption,
  stalls and the buffer cap.
* `tests/params.rs` — codec identity round-trips, capability naming, the D4
  predicate, parameter validation and merging, profile subsumption and
  `-level auto`.

## Known gaps

* `vaco-pool::Buffer` has no public constructor yet, so no crate outside
  `vaco-pool` can build a `Packet`. The mock codec is therefore exercised over
  its own lightweight input type; `MockDecoder` provides the `Packet`/`Frame`
  face for the day that lands.
* Plan 15 §1.8.2 puts these picture primitives in `vaco-frame`. They live here
  instead, because `vaco-frame` is a layer-1 crate owned elsewhere and still
  frozen. Moving them later is a re-export.
