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
| `parser` | `ParserDesc` (how a parser is registered) and `ParserDriver` (the harness that drives it) |
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

### Typed video configuration

Offer container parameters through `Decoder::prime_video_params` before the
first packet. Its default uses `VideoParameters::coded_dimensions()` and calls
the existing `prime_video` hook, preserving dimension-only decoders. Raw video
overrides it to receive the declared pixel format too. `Box<Decoder>`,
`AsDecoder`, `DecoderProtocol`, and `Validated` forward the complete parameters;
container properties never need a codec-private extradata envelope.

### Two-pass encoder contract

Before feeding frames, call `Encoder::set_pass` with `EncoderPass::Single`,
`First`, or `Second(stats)`. The second-pass statistics are opaque bytes from
the same encoder's completed first pass. `pass_stats()` returns them only after
successful drain. File names and persistence belong to the caller, not the
codec interface.

The default accepts only `Single`; unsupported multipass requests fail
explicitly. An encoder adding two-pass rate control implements both methods.
`SendReceive`, `AsEncoder`, and `Validated` forward the same contract so a
protocol wrapper cannot silently discard it. The interface adds no dependencies
or global configuration.

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

Both capability rules are stated to the validator in terms of what a *caller*
can see, which is not quite the same as how they read:

* **C2** becomes "cumulative outputs never exceed cumulative inputs". Same
  statement, but it does not misfire when a caller sends several inputs before
  taking anything and then drains the queue in one go.
* **C1** becomes "output appeared after `send(None)`, from a component that had
  already answered `NeedMoreInput` and been given no input since". A component
  that merely had output queued under backpressure is not buffering; one that
  said it had nothing left and then produced something is. The consequence is
  that C1 is only detected when the caller actually drained before end of
  stream — it never reports a false violation, and it catches the real case
  every ordinary caller produces.
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
  push(chunk)      ──►  [ reassembly buffer ] ──► parser.parse(&buf) ──► Packet
  next_unit()           ▲                              │
                        └──── unconsumed tail ─────────┘
  finish()         ──►  parse(&[]) once the buffer drains ──► final Packet ──► Eof
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

#### `set_extradata`, and why the trait needed a fourth method

`Parser::set_extradata` takes the container's out-of-band configuration record
— `avcC`, `hvcC`, `av1C`, an `AudioSpecificConfig`, an `OpusHead`. It has a
default body that ignores it, so a codec whose containers carry none writes
nothing.

It looks like a convenience and it is not. In an MPEG-TS or raw elementary
stream every parameter set is in-band and `parse` alone finds everything. **In
MP4 the H.264 sequence parameter set is in `avcC` and appears in no sample at
all**, so a parser fed only payloads reports nothing, forever, however many
packets it is given. Measured on `av.mp4`: of the eight bitstream-derived values
`ffprobe -show_streams` prints for its H.264 track, 8 arrive through the record
and 0 through the packet path. Opus is the extreme case — its channel count,
pre-skip and mapping exist *only* in the identification header, so for that
codec there is no packet path at all.

Two things ride on it that are not bitstream facts:

* **The NAL length prefix size**, without which a length-prefixed sample cannot
  be read at all. `H264Parser` and `HevcParser` remember it and switch `parse`
  from the byte-stream scanner to the container path, because a length-prefixed
  sample contains no start codes and the scanner finds nothing in one.
* **`is_avc`/`nal_length_size`**, which `-show_streams` prints and which are
  properties of the *container's* framing rather than of the bitstream. They
  reach the caller through `VideoParameters::nal_length_size`.

An error from it is not fatal to a caller that is merely *offering* a record —
stream discovery is — because a malformed record means "this told me nothing",
not "stop reporting the file".

#### `packet_duration`, and why the fact belongs on a *parser*

`Parser::packet_duration(&self, packet: &[u8]) -> Option<Rational>` answers "how
long is this already-framed packet, in seconds", and defaults to `None`.

The gap it closes is narrow and load-bearing. Matroska writes **no
`DefaultDuration` element for an Opus track and no `BlockDuration` on its
blocks** — verified by searching the file for the element ID rather than by
trusting a demuxer — and `ffprobe` still prints 20 ms per packet, because it
reads Opus's own TOC byte. D14.1 forbids `vaco-demux-matroska` from naming
`vaco-parse-opus`, and the only seam it has onto codec code is `ParserProvider`,
which hands back a `dyn Parser`. So the answer arrives through this trait or it
does not arrive at all.

Four shape decisions, each with the measurement that forced it:

* **Seconds, exactly, as a `Rational`.** The consumer truncates into the
  stream's time base (see `vaco-format-core`'s `quantise_duration`), so an input
  that has already been rounded is wrong half a tick of the time. A 2.5 ms Opus
  packet on Matroska's 1 ms base is exactly that case: from `120/48000` the
  answer is 2, which is what the reference prints, and from a
  microsecond-rounded 2500 it is 3.
* **Not "samples".** A sample count needs a rate, and the rate a caller has is
  the one the *container* reports — wrong for both codecs that implement this.
  Opus always runs at 48 kHz whatever `input_sample_rate` says, and an SBR AAC
  stream reports the *extension* rate while its frames are counted at the core
  rate. Dividing inside the parser makes 1024 core samples at 22050 Hz and 2048
  output samples at 44100 Hz the same value, so SBR stops being a special case
  anywhere else in the tree.
* **Not a `fn` field on `ParserDesc`.** That would be inspectable without
  constructing a parser, which is the registry's preference — but AAC's answer
  lives in an `AudioSpecificConfig` the parser was handed by `set_extradata`, so
  a free function cannot produce it.
* **Not ticks of a caller-supplied base.** That would put the truncation rule —
  a reference-matching decision, not a codec one — inside every parser instead
  of in one place. D19.

`&self`, and defaulted. It is a question, not a step: callable once per packet
on the read path without advancing state, without allocating, and without the
caller having fed the same bytes to `parse` first. The default is what keeps the
change additive across the five crates that implement `Parser` — three of them
(H.264, HEVC, AV1) have nothing to say, because a video packet's duration is the
container's statement.

A malformed packet is `None`, never an error and never a panic. The caller is
filling in a field the container left blank; a packet it cannot measure is one
it reports without a duration, which is what the container said.

### Registering a parser: `ParserDesc`

The counterpart of `DecoderDesc`, and the descriptor type the registry's
`parser` fragment kind was waiting for.

```rust
pub const PARSER: ParserDesc = ParserDesc {
    name: "h264",
    long_name: "H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10",
    codecs: &[CodecId::H264],
    media_type: MediaType::Video,
    make: |limits| Box::new(H264Parser::new(limits)),
};
```

Two shape decisions:

* **`make` is a `fn` field, not a trait method.** The registry's rule is that a
  descriptor is inspectable without constructing anything, so `-parsers` can
  print a table without allocating a parser. A `const` holding a function
  pointer satisfies that; a `Box<dyn ParserFactory>` would not, because
  `Box::new` is not `const`.
* **`make` takes `Limits`.** A parser on the probe path reads
  attacker-controlled bytes before anything has validated them; there is
  deliberately no no-argument constructor, so every caller states a budget.
* **`codecs` is a slice**, because one implementation genuinely covers several
  `CodecId`s and a one-to-one field would force a second descriptor that then
  drifts.

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

### `DecoderDesc`/`EncoderDesc::make`, and why `EncoderDesc` exists at all

C-13: `DecoderDesc` used to carry no constructor, so a registered decoder made
`vaco_registry::can_decode` true and nothing more — there was no path from a
name to a live `Decoder`. `make: fn(Limits) -> Box<dyn Decoder>` is that path,
mirroring `ParserDesc::make` exactly (a `fn` pointer, not a boxed factory, so
`-h decoder=<name>` stays inspectable without allocating; `Limits` bounds the
decode side the way it bounds parsing, since a codec's own dimensions/sample
counts are attacker-controlled). `EncoderDesc` is the same shape and did not
exist before C-13 at all — `vaco-registry`'s `encoder` fragment kind had a
name and a metadata row and nothing else, because there was no descriptor
type for a `ctor` to name.

A payload-carrying `CodecId::Ext(&'static ExtCodec)` was drafted as the
policy for registering a new codec without a `vaco-codec-core` edit per one,
and rejected: at least one call site (`vaco-bsf-generic`'s noise generator)
casts `CodecId as u64`, which Rust only allows when every variant is
fieldless. See `planning/TECH-DEBT.md`'s C-13 entry for what replaced it.

### `CodecId::ticks_per_frame` — measured, not one per property table

`params.video.frame_rate` (set by a codec's own parser, e.g.
`vaco-parse-h264`) is a **tick rate** for some codecs — twice the picture
rate — and a picture rate directly for others. A caller filling in a
packet's duration from it (`vaco-format-core::discovery`'s R21 rule) needs
`frame_rate / ticks_per_frame`, mirroring the reference's
`ff_compute_frame_duration`; using `frame_rate` alone silently halves every
duration it fills in for a codec whose rate field is doubled (issue #632
part 1).

Measured per codec on real 25/24/30 fps encodes, comparing bitstream-derived
`r_frame_rate` against the true encode rate (`ffprobe 8.1`): **H.264 and
MPEG-1 video** report exactly double, at every rate tried; **MPEG-2 video and
MPEG-4 part 2 do not**, despite both being interlace-capable — the natural
guess from `CodecProperties::FIELDS`, and wrong. `ticks_per_frame` is
therefore its own function with two hand-picked match arms, not a read of
`FIELDS` or a new `CodecProperties` bit: the two concepts coincide for H.264
by chance, not by definition, and MPEG-2 proves they are not the same
property. Defaults to `1` for every other codec, including ones with no
parser yet — safe, since a caller with no better information should not
divide by anything.

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
  is what the CLI prints and parses. Note that `bitflags` generates a
  `from_name` of its own over the *constant* names, which is why the CLI-facing
  lookup is `Caps::from_cli_name`.
* **Adding a `ticks_per_frame` exception.** Do not guess from `CodecProperties`
  or from what another codec in the same family does — MPEG-1 needs it and
  MPEG-2 does not, which is the whole reason this is measured per codec. Mux a
  short clip with the reference at two or three different frame rates, compare
  `r_frame_rate` against the true rate on the raw elementary stream *and* in a
  container, and only add the arm once the ratio is exactly 2 at every rate
  tried.
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
* **Charging a decoded frame's real byte size against a budget.** Use
  `planar_frame_bytes(width, height, monochrome, sub_width_c, sub_height_c,
  bit_depth_luma, bit_depth_chroma)` — it sums one full-resolution luma plane
  and, unless monochrome, two chroma planes decimated by the bitstream's own
  `SubWidthC`/`SubHeightC` (H.264 and HEVC's SPS-level frame-budget checks both
  call it). Do not charge a flat bytes-per-pixel guess: `PixFmt::bits_per_pixel`
  covers only the *named* formats, and both H.264 and HEVC permit luma bit
  depths (11, 13) with no corresponding one, so a `PixFmt`-based computation
  cannot serve this purpose — the caller must supply the raw subsampling
  factors and depths from its own SPS. Where a `PixFmt` *is* already resolved
  (any codec whose frame format is fixed or known before the check, e.g.
  ProRes, VC-1, Theora, or a container-supplied uncompressed track), charge
  `bits_per_pixel().div_ceil(8)` instead — the same quantity
  `Frame::alloc_video` itself charges — rather than either helper's guess. A
  flat "4 bytes per pixel" is only correct for a decoder that genuinely
  produces packed 8-bit RGBA; for a subsampled YUV frame it overshoots badly
  enough to reject a legitimately large, valid frame (measured: a stock 4K
  4:2:0 HEVC stream, 12.4 MB of real samples, was charged 33.2 MB and blew
  through `Limits::strict`'s 16 MiB `max_frame_bytes` cap).
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

## Fuzzing

Three targets in `fuzz/`, behind the `codec-core` feature. The untrusted input
is the *call sequence*, not a byte stream, because that is what this crate
actually consumes:

| Target | What it drives |
|---|---|
| `codec_send_receive` | arbitrary caller behaviour against the mock codec, checked against the reference model and for recorded violations |
| `codec_parser_driver` | arbitrary chunking and parser misbehaviour through `ParserDriver`, with a hard step cap so a hang is a finding |
| `codec_picture_bands` | arbitrary picture geometry, publication and block reads, with every byte checked against the row it should have come from |

## Testing

* `tests/parser.rs` — the driver's reassembly, end-of-stream and progress rules,
  plus the two defaulted trait methods: `set_extradata` must be harmless when
  ignored, and `packet_duration` must default to `None` and forward through
  `Box<dyn Parser>` — the only way a provider-supplied parser is ever reached.
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

* **`AudioParameters::bits_per_raw_sample` is the wrong home for a container's
  sample depth.** `VideoParameters` now carries one too — the reference prints
  `bits_per_raw_sample=8` for an 8-bit H.264 stream and `N/A` for the AAC track
  beside it, the exact opposite of what the model could express — but the
  audio-side field is being filled by `vaco-demux-mp4` and `vaco-demux-matroska`
  from the container's `stsd` sample entry / `BitDepth`. Probed on a WAV,
  `pcm_s16le` reports `bits_per_sample=16` and `bits_per_raw_sample="N/A"`, so
  that number is `bits_per_coded_sample`, a **different field** with nowhere to
  live. `vaco-probe` suppresses the audio value for float-output codecs as a
  stopgap; the fix is a `bits_per_coded_sample` on `AudioParameters`.
* **`CodecParameters` has no `max_bit_rate`**, which the reference prints for
  every stream.
* `vaco-pool::Buffer` has no public constructor yet, so no crate outside
  `vaco-pool` can build a `Packet`. The mock codec is therefore exercised over
  its own lightweight input type; `MockDecoder` provides the `Packet`/`Frame`
  face for the day that lands.
* Plan 15 §1.8.2 puts these picture primitives in `vaco-frame`. They live here
  instead, because `vaco-frame` is a layer-1 crate owned elsewhere and still
  frozen. Moving them later is a re-export.
